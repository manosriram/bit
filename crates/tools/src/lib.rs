mod bash;
mod edit;
mod read;
mod search;
mod write;

use serde::de::DeserializeOwned;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

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

#[derive(Debug)]
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
    let saved = match spill(&text, cx) {
        Ok(p) => format!("full output in {}. hint: read it with offset", p.display()),
        Err(e) => format!("the full output could not be saved: {e}"),
    };
    format!("{head}[... {left_out} lines left out; {saved}]\n{tail}")
}

fn spill(text: &str, cx: &Ctx) -> std::io::Result<PathBuf> {
    static N: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(&cx.spill)?;
    let path = cx.spill.join(format!("{}.txt", N.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&path, text)?;
    Ok(path)
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
