use crate::{Ctx, err, io_err, numbered};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};

const LIMIT: usize = 2000;
const LINE_MAX: usize = 2000;
const BYTES_MAX: usize = 100_000;
/// Bytes kept of a shown line: enough for `LINE_MAX` characters and one more, so a longer
/// line is always seen as cut.
const LINE_BYTES: usize = 4 * (LINE_MAX + 1);

#[derive(Deserialize)]
pub struct Args {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub fn run(a: Args, cx: &Ctx) -> Result<String, String> {
    let path = cx.cwd.join(&a.path);
    let fail = |e| io_err(&path, e);
    // Streamed: memory is one buffer plus the lines shown, whatever the file's size.
    let mut f = BufReader::with_capacity(64 * 1024, File::open(&path).map_err(fail)?);
    let head = f.fill_buf().map_err(fail)?;
    if head[..head.len().min(8192)].contains(&0) {
        return Err(err(
            format_args!("{} is a binary file", a.path),
            "inspect it with bash, e.g. `file` or `xxd | head`",
        ));
    }
    let first = a.offset.unwrap_or(1).max(1);
    let end = first - 1 + a.limit.unwrap_or(LIMIT).max(1);
    let (mut out, mut total, mut last) = (String::new(), 0, first - 1);
    // The line being read, if it's shown; `cut` once bytes past `LINE_BYTES` were dropped.
    // `open` once a line has bytes but no newline yet.
    let (mut line, mut cut, mut open) = (Vec::new(), false, false);
    loop {
        let buf = f.fill_buf().map_err(fail)?;
        let eof = buf.is_empty();
        let nl = buf.iter().position(|&b| b == b'\n');
        let part = &buf[..nl.unwrap_or(buf.len())];
        let shown = (first..=end).contains(&(total + 1)) && out.len() <= BYTES_MAX;
        if shown {
            let room = LINE_BYTES - line.len();
            cut |= part.len() > room;
            line.extend_from_slice(&part[..part.len().min(room)]);
        }
        let used = nl.map_or(buf.len(), |i| i + 1);
        f.consume(used);
        // Like `str::lines`: a last line without a newline counts, a final newline starts none.
        if nl.is_some() || eof && open {
            total += 1;
            if shown {
                push(&mut out, total, &line, nl.is_some() && !cut);
                last = total;
            }
            (cut, open) = (false, false);
            line.clear();
        } else {
            open = true;
        }
        if eof {
            break;
        }
    }
    if total == 0 {
        return Ok("(empty file)".into());
    }
    if first > total {
        return Err(err(
            format_args!("offset {first} is past the end of {} ({total} lines)", a.path),
            "pass a smaller offset",
        ));
    }
    if last < total {
        out += &format!(
            "[lines {first}-{last} of {total}. hint: read again with offset {}]\n",
            last + 1
        );
    }
    Ok(out)
}

/// Adds line `n`, cut to `LINE_MAX` characters; `crlf` drops the `\r` of a `\r\n` ending.
fn push(out: &mut String, n: usize, line: &[u8], crlf: bool) {
    let line = if crlf { line.strip_suffix(b"\r").unwrap_or(line) } else { line };
    let s = String::from_utf8_lossy(line);
    match s.char_indices().nth(LINE_MAX) {
        Some((i, _)) => numbered(out, n, &format!("{} [line cut]", &s[..i])),
        None => numbered(out, n, &s),
    }
}

#[cfg(test)]
mod tests {
    use crate::{exec, test_ctx};

    #[tokio::test]
    async fn numbers_pages_and_rejects_binary() {
        let cx = test_ctx("read");
        std::fs::write(cx.cwd.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let o = exec("read", r#"{"path":"a.txt"}"#, &cx).await;
        assert_eq!(o.content, "     1\tone\n     2\ttwo\n     3\tthree\n");
        let o = exec("read", r#"{"path":"a.txt","offset":2,"limit":1}"#, &cx).await;
        assert_eq!(o.content, "     2\ttwo\n[lines 2-2 of 3. hint: read again with offset 3]\n");
        let o = exec("read", r#"{"path":"a.txt","offset":9}"#, &cx).await;
        assert!(!o.ok && o.content.contains("past the end"));

        std::fs::write(cx.cwd.join("b.bin"), [1, 0, 2]).unwrap();
        let o = exec("read", r#"{"path":"b.bin"}"#, &cx).await;
        assert!(!o.ok && o.content.contains("binary"));
        let o = exec("read", r#"{"path":"."}"#, &cx).await;
        assert!(!o.ok && o.content.contains("hint: list its files with glob"));
        let o = exec("read", r#"{"path":"missing"}"#, &cx).await;
        assert!(!o.ok && o.content.contains("hint: check the path, or find it with glob"));
    }

    /// The old reader: the whole file in memory, split with `str::lines`.
    fn whole(bytes: &[u8], first: usize, limit: usize) -> String {
        let text = String::from_utf8_lossy(bytes);
        let total = text.lines().count();
        if total == 0 {
            return "(empty file)".into();
        }
        let (mut out, mut last) = (String::new(), first - 1);
        for line in text.lines().skip(first - 1).take(limit) {
            if out.len() > super::BYTES_MAX {
                break;
            }
            last += 1;
            match line.char_indices().nth(super::LINE_MAX) {
                Some((i, _)) => {
                    crate::numbered(&mut out, last, &format!("{} [line cut]", &line[..i]))
                }
                None => crate::numbered(&mut out, last, line),
            }
        }
        if last < total {
            out += &format!(
                "[lines {first}-{last} of {total}. hint: read again with offset {}]\n",
                last + 1
            );
        }
        out
    }

    #[tokio::test]
    async fn streaming_reads_like_the_whole_file() {
        let cx = test_ctx("read-stream");
        let long = "é".repeat(5000);
        let wide = "x".repeat(70_000);
        let many = format!("{wide}\n").repeat(60);
        let files: [&[u8]; 10] = [
            b"",
            b"\n",
            b"a\r\nb\r\n",
            b"no newline\r",
            b"\n\n\nend",
            b"bad \xff utf8\nnext\n",
            long.as_bytes(),
            wide.as_bytes(),
            &b"y\n".repeat(100_000),
            many.as_bytes(),
        ];
        for (n, bytes) in files.iter().enumerate() {
            std::fs::write(cx.cwd.join("f"), bytes).unwrap();
            for (first, limit) in [(1, 2000), (2, 1), (1, 3), (3, 2000), (150_000, 5)] {
                let total = String::from_utf8_lossy(bytes).lines().count();
                if total > 0 && first > total {
                    continue;
                }
                let args = format!(r#"{{"path":"f","offset":{first},"limit":{limit}}}"#);
                let o = exec("read", &args, &cx).await;
                assert_eq!(o.content, whole(bytes, first, limit), "file {n} at {first}+{limit}");
            }
        }
    }
}
