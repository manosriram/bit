use crate::{Ctx, err, io_err, numbered};
use serde::Deserialize;

const LIMIT: usize = 2000;
const LINE_MAX: usize = 2000;
const BYTES_MAX: usize = 100_000;

#[derive(Deserialize)]
pub struct Args {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub fn run(a: Args, cx: &Ctx) -> Result<String, String> {
    let path = cx.cwd.join(&a.path);
    let bytes = std::fs::read(&path).map_err(|e| io_err(&path, e))?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(err(
            format_args!("{} is a binary file", a.path),
            "inspect it with bash, e.g. `file` or `xxd | head`",
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    if total == 0 {
        return Ok("(empty file)".into());
    }
    let first = a.offset.unwrap_or(1).max(1);
    if first > total {
        return Err(err(
            format_args!(
                "offset {first} is past the end of {} ({total} lines)",
                a.path
            ),
            "pass a smaller offset",
        ));
    }
    let mut out = String::new();
    let mut last = first - 1;
    for line in text
        .lines()
        .skip(first - 1)
        .take(a.limit.unwrap_or(LIMIT).max(1))
    {
        if out.len() > BYTES_MAX {
            break;
        }
        last += 1;
        match line.char_indices().nth(LINE_MAX) {
            Some((i, _)) => numbered(&mut out, last, &format!("{} [line cut]", &line[..i])),
            None => numbered(&mut out, last, line),
        }
    }
    if last < total {
        out += &format!(
            "[lines {first}-{last} of {total}. hint: read again with offset {}]\n",
            last + 1
        );
    }
    Ok(out)
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
        assert_eq!(
            o.content,
            "     2\ttwo\n[lines 2-2 of 3. hint: read again with offset 3]\n"
        );
        let o = exec("read", r#"{"path":"a.txt","offset":9}"#, &cx).await;
        assert!(!o.ok && o.content.contains("past the end"));

        std::fs::write(cx.cwd.join("b.bin"), [1, 0, 2]).unwrap();
        let o = exec("read", r#"{"path":"b.bin"}"#, &cx).await;
        assert!(!o.ok && o.content.contains("binary"));
        let o = exec("read", r#"{"path":"."}"#, &cx).await;
        assert!(!o.ok && o.content.contains("hint: list its files with glob"));
        let o = exec("read", r#"{"path":"missing"}"#, &cx).await;
        assert!(
            !o.ok
                && o.content
                    .contains("hint: check the path, or find it with glob")
        );
    }
}
