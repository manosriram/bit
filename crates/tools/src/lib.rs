mod bash;
mod edit;
mod read;
mod search;
mod write;

pub use search::files;

use serde::de::DeserializeOwned;
use std::fmt::Display;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const SCHEMAS: &str = r#"[
{"type":"function","function":{"name":"read","description":"Read a text file. Returns numbered lines, 2000 at a time.","parameters":{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","description":"First line, 1-based"},"limit":{"type":"integer","description":"Max lines, default 2000"}},"required":["path"]}}},
{"type":"function","function":{"name":"write","description":"Create or overwrite a file with the given content.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}},
{"type":"function","function":{"name":"edit","description":"Replace the exact text `old` with `new` in a file. `old` must match exactly once, including whitespace.","parameters":{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}}},
{"type":"function","function":{"name":"bash","description":"Run a shell command in the working directory. stderr is merged into stdout. Background jobs are killed when the command exits.","parameters":{"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer","description":"Seconds, default 120, max 600"}},"required":["command"]}}},
{"type":"function","function":{"name":"ask","description":"Ask the user to choose. Use this instead of listing options in text and asking for a number. Returns the user's answer, which may be one of the options or their own text.","parameters":{"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"},"description":"2 to 9 short choices"}},"required":["question","options"]}}},
{"type":"function","function":{"name":"grep","description":"Search file contents with a ripgrep regex. Respects .gitignore.","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string","description":"Only search files matching this glob, e.g. *.rs"}},"required":["pattern"]}}},
{"type":"function","function":{"name":"glob","description":"List files matching a glob, e.g. *.rs or src/**/mod.rs. Respects .gitignore.","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}}}
]"#;

pub struct Ctx {
    pub cwd: PathBuf,
    /// Directory for spill files; created on first use.
    pub spill: PathBuf,
}

pub struct Output {
    pub ok: bool,
    pub content: String,
}

pub fn mutates(name: &str) -> bool {
    matches!(name, "write" | "edit" | "bash")
}

/// One line naming what a call touches, for the gutter and the approval title.
pub fn summary(name: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    match (name, s("path")) {
        ("bash", _) => s("command").unwrap_or("").lines().next().unwrap_or("").into(),
        ("ask", _) => s("question").unwrap_or("").into(),
        ("grep" | "glob", None) => format!("{:?}", s("pattern").unwrap_or("")),
        ("grep" | "glob", Some(p)) => format!("{:?} in {p}", s("pattern").unwrap_or("")),
        (_, p) => p.unwrap_or("").into(),
    }
}

/// The body of an approval: what would change, as `-`/`+` lines, or the command to run.
pub fn detail(name: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mark = |m: &str, t: &str| t.lines().map(|l| format!("{m} {l}\n")).collect::<String>();
    match name {
        "edit" => mark("-", s("old")) + &mark("+", s("new")),
        "write" => mark("+", s("content")),
        "bash" => s("command").into(),
        _ => String::new(),
    }
}

pub async fn exec(name: &str, args: &str, cx: &Ctx) -> Output {
    let r = call(name, args, cx).await;
    Output { ok: r.is_ok(), content: r.unwrap_or_else(|e| e) }
}

async fn call(name: &str, args: &str, cx: &Ctx) -> Result<String, String> {
    match name {
        "read" => read::run(parse(args)?, cx),
        "write" => write::run(parse(args)?, cx),
        "edit" => edit::run(parse(args)?, cx),
        "bash" => bash::run(parse(args)?, cx).await,
        "grep" => search::grep(parse(args)?, cx).await,
        "glob" => search::glob(parse(args)?, cx).await,
        _ => Err(err(
            format_args!("unknown tool `{name}`"),
            "use read, write, edit, bash, grep or glob",
        )),
    }
}

fn parse<T: DeserializeOwned>(args: &str) -> Result<T, String> {
    serde_json::from_str(args).map_err(|e| {
        err(format_args!("bad arguments: {e}"), "pass a JSON object that matches the tool's schema")
    })
}

fn err(what: impl Display, hint: &str) -> String {
    format!("error: {what}. hint: {hint}")
}

fn io_err(path: &Path, e: std::io::Error) -> String {
    let hint = match e.kind() {
        std::io::ErrorKind::NotFound => "check the path, or find it with glob",
        std::io::ErrorKind::IsADirectory => "list its files with glob",
        std::io::ErrorKind::PermissionDenied => "the file isn't accessible to bit",
        _ => "check the path",
    };
    err(format_args!("{}: {e}", path.display()), hint)
}

/// Output as text, copied only when it isn't valid UTF-8.
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Numbered like `cat -n`, so the model can cite and edit by line.
fn numbered(out: &mut String, n: usize, line: &str) {
    use std::fmt::Write;
    let _ = writeln!(out, "{n:>6}\t{line}");
}

const KEEP: usize = 200;
const HALF: usize = 20_000;

/// Keeps long output from flooding the context: the head and tail stay inline and the full
/// text goes to a spill file the model can page through with `read`.
fn cap(text: String, cx: &Ctx) -> String {
    if text.lines().count() <= 2 * KEEP && text.len() <= 2 * HALF {
        return text;
    }
    let head_end = text.match_indices('\n').nth(KEEP - 1).map_or(text.len(), |(i, _)| i + 1);
    let head = &text[..text.floor_char_boundary(head_end.min(HALF))];
    // A final newline ends the last line rather than starting a new one.
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let tail_start = body
        .rmatch_indices('\n')
        .nth(KEEP - 1)
        .map_or(0, |(i, _)| i + 1)
        .max(text.len().saturating_sub(HALF))
        .max(head.len());
    let tail = &text[text.ceil_char_boundary(tail_start)..];
    let left_out = text[head.len()..text.len() - tail.len()].lines().count();
    format!("{head}[... {left_out} lines left out; {}]\n{tail}", saved(spill(&text, cx)))
}

/// The note on where the full output went.
fn saved(r: std::io::Result<PathBuf>) -> String {
    match r {
        Ok(p) => format!("full output in {}. hint: read it with offset", p.display()),
        Err(e) => format!("the full output could not be saved: {e}"),
    }
}

fn spill(text: &str, cx: &Ctx) -> std::io::Result<PathBuf> {
    let (mut f, path) = spill_file(cx)?;
    f.write_all(text.as_bytes())?;
    Ok(path)
}

fn spill_file(cx: &Ctx) -> std::io::Result<(File, PathBuf)> {
    static N: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(&cx.spill)?;
    let path = cx.spill.join(format!("{}.txt", N.fetch_add(1, Ordering::Relaxed)));
    Ok((File::create(&path)?, path))
}

/// A command's output, gathered as it arrives with bounded memory. Up to `2 * HALF` bytes it
/// is all kept and `cap`ped at the end. Past that, `cap` would cut it anyway: it goes to the
/// spill file as it comes, and only the bytes `cap` could show stay, the first `HALF + 1`
/// and the last `HALF + 1` (one more each way, to see past the cut).
pub(crate) struct Capped<'a> {
    cx: &'a Ctx,
    /// Everything so far; once spilling, the last `HALF + 1` bytes.
    buf: Vec<u8>,
    head: Vec<u8>,
    newlines: usize,
    spill: Option<std::io::Result<(File, PathBuf)>>,
}

impl<'a> Capped<'a> {
    pub(crate) fn new(cx: &'a Ctx) -> Self {
        Self { cx, buf: Vec::new(), head: Vec::new(), newlines: 0, spill: None }
    }

    /// Reads `r` to its end; a read error ends it too.
    pub(crate) async fn read(&mut self, mut r: impl AsyncRead + Unpin) {
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = r.read(&mut chunk).await {
            self.feed(&chunk[..n]);
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.newlines += lines_in(chunk);
        self.buf.extend_from_slice(chunk);
        let new = match self.spill {
            Some(_) => chunk,
            None if self.buf.len() > 2 * HALF => {
                self.head = self.buf[..=HALF].to_vec();
                self.spill = Some(spill_file(self.cx));
                &self.buf
            }
            None => return,
        };
        // A failed write keeps the error for the note; the output is still shown.
        if let Some(Ok((f, _))) = &mut self.spill
            && let Err(e) = f.write_all(new)
        {
            self.spill = Some(Err(e));
        }
        self.buf.drain(..self.buf.len().saturating_sub(HALF + 1));
    }

    /// What `cap` gives for the whole output.
    pub(crate) fn finish(self) -> String {
        let Some(spill) = self.spill else { return cap(text(self.buf), self.cx) };
        let nth_nl = |b: &[u8], rev: bool| {
            let mut at = b.iter().enumerate().filter(|(_, c)| **c == b'\n').map(|(i, _)| i + 1);
            if rev { at.rev().nth(KEEP - 1) } else { at.nth(KEEP - 1) }
        };
        let h = &self.head;
        let head = &h[..floor(h, nth_nl(&h[..HALF], false).unwrap_or(HALF))];
        let ring = &self.buf;
        let body = ring.strip_suffix(b"\n").unwrap_or(ring);
        let start = ceil(ring, nth_nl(body, true).unwrap_or(0).max(ring.len() - HALF));
        let tail = &ring[start..];
        let left_out =
            self.newlines - lines_in(head) - lines_in(tail) + usize::from(ring[start - 1] != b'\n');
        let (head, tail) = (String::from_utf8_lossy(head), String::from_utf8_lossy(tail));
        format!("{head}[... {left_out} lines left out; {}]\n{tail}", saved(spill.map(|(_, p)| p)))
    }
}

fn lines_in(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

/// `str::floor_char_boundary` and `ceil_char_boundary` for bytes; `i` is at most `b.len()`.
fn floor(b: &[u8], i: usize) -> usize {
    (0..=i).rev().find(|&i| b.get(i).is_none_or(|c| c & 0xC0 != 0x80)).unwrap_or(0)
}

fn ceil(b: &[u8], i: usize) -> usize {
    (i..b.len()).find(|&i| b[i] & 0xC0 != 0x80).unwrap_or(b.len())
}

#[cfg(test)]
pub(crate) fn test_ctx(name: &str) -> Ctx {
    let dir = std::env::temp_dir().join(format!("bit-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Ctx { spill: dir.join(".spill"), cwd: dir }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_keeps_head_and_tail_and_spills_the_rest() {
        let cx = test_ctx("cap");
        let text: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        let out = cap(text.clone(), &cx);
        assert!(out.starts_with("line 1\n"));
        assert!(out.contains("\nline 200\n[... 600 lines left out; full output in "));
        assert!(out.contains("hint: read it with offset]\nline 801\n"));
        assert!(out.ends_with("line 1000\n"));
        let path = out.split("full output in ").nth(1).unwrap().split(". hint").next().unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
        assert_eq!(cap("short".into(), &cx), "short");
    }

    #[test]
    fn capped_streams_to_what_cap_gives() {
        let cx = test_ctx("capped");
        let note = |s: &str| {
            let (a, b) = s.split_once("full output in ").unwrap_or((s, ""));
            (a.to_owned(), b.split_once(". hint").map_or(b, |x| x.1).to_owned())
        };
        let lines =
            |n: usize, w: usize| -> String { (1..=n).map(|i| format!("{i:0w$}é\n")).collect() };
        let mut cases = vec![
            String::new(),
            "short".into(),
            lines(399, 3),
            lines(401, 3),
            lines(1000, 3),
            lines(100_000, 3),
            lines(50, 2000),
            lines(30, 2000).trim_end().into(),
            "x".repeat(100_000),
            "é".repeat(30_000),
            format!("{}\n\n\n", "ab\n".repeat(20_000)),
        ];
        cases.push(cases[5].replace('\n', "\n\n"));
        for (n, text) in cases.iter().enumerate() {
            for size in [1, 7, 4096, 65_536] {
                let mut c = Capped::new(&cx);
                text.as_bytes().chunks(size).for_each(|b| c.feed(b));
                let got = c.finish();
                assert_eq!(note(&got), note(&cap(text.clone(), &cx)), "case {n}, chunks of {size}");
                if let Some(p) = got.split("full output in ").nth(1) {
                    let p = p.split(". hint").next().unwrap();
                    assert_eq!(&std::fs::read_to_string(p).unwrap(), text, "case {n} spill");
                }
            }
        }
    }

    #[tokio::test]
    async fn exec_reports_bad_args_and_unknown_tools() {
        let cx = test_ctx("exec");
        let o = exec("read", "{}", &cx).await;
        assert!(!o.ok && o.content.starts_with("error: bad arguments: missing field `path`"));
        let o = exec("nope", "{}", &cx).await;
        assert!(!o.ok && o.content.contains("unknown tool `nope`"));
        serde_json::from_str::<serde_json::Value>(SCHEMAS).unwrap();
        assert_eq!(summary("read", r#"{"path":"a.rs"}"#), "a.rs");
        assert_eq!(summary("grep", r#"{"pattern":"feed(","path":"src"}"#), r#""feed(" in src"#);
        assert_eq!(summary("bash", r#"{"command":"ls\npwd"}"#), "ls");
        assert_eq!(detail("edit", r#"{"old":"a\nb","new":"c"}"#), "- a\n- b\n+ c\n");
    }
}
