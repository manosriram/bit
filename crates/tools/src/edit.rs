use crate::{Ctx, err, io_err, numbered};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Args {
    path: String,
    old: String,
    new: String,
}

pub fn run(a: Args, cx: &Ctx) -> Result<String, String> {
    if a.old.is_empty() {
        return Err(err("`old` is empty", "to create a file, use write"));
    }
    let path = cx.cwd.join(&a.path);
    let text = std::fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    match text.matches(&a.old).count() {
        1 => {
            std::fs::write(&path, text.replacen(&a.old, &a.new, 1))
                .map_err(|e| io_err(&path, e))?;
            Ok(format!("edited {}", a.path))
        }
        0 => Err(near_misses(&text, &a.old, &a.path)),
        n => Err(err(
            format_args!("`old` matches {n} times in {}", a.path),
            "include more surrounding lines so it matches once",
        )),
    }
}

/// The 3 windows of the file most like `old`, so the model can copy the exact text and
/// retry without reading the file again.
fn near_misses(text: &str, old: &str, path: &str) -> String {
    let mut msg = err(
        format_args!("`old` was not found in {path}"),
        "copy the exact text from the closest matches below and retry",
    );
    let lines: Vec<&str> = text.lines().collect();
    let want: Vec<Vec<u64>> = old.lines().map(bigrams).collect();
    let k = want.len().clamp(1, lines.len().max(1));
    if lines.is_empty() || want.is_empty() {
        return msg;
    }
    let have: Vec<Vec<u64>> = lines.iter().map(|l| bigrams(l)).collect();
    let mut scored: Vec<(f32, usize)> = (0..=lines.len() - k)
        .map(|s| {
            let sum: f32 = (0..k).map(|j| dice(&have[s + j], &want[j])).sum();
            (sum / k as f32, s)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    msg.push('\n');
    let mut picked: Vec<usize> = Vec::new();
    for (score, s) in scored {
        // Below this, a window is noise that costs tokens and helps nothing.
        if picked.len() == 3 || score < 0.3 {
            break;
        }
        if picked.iter().all(|&p| p.abs_diff(s) >= k) {
            picked.push(s);
            msg += &format!(
                "\nlines {}-{} ({:.0}% similar):\n",
                s + 1,
                s + k,
                score * 100.0
            );
            for (j, line) in lines[s..s + k].iter().enumerate() {
                numbered(&mut msg, s + j + 1, line);
            }
        }
    }
    msg
}

/// Sorted char bigrams of the trimmed line, so indentation differences don't count.
fn bigrams(line: &str) -> Vec<u64> {
    let cs: Vec<char> = line.trim().chars().collect();
    let mut g: Vec<u64> = cs
        .windows(2)
        .map(|w| (u64::from(w[0]) << 32) | u64::from(w[1]))
        .collect();
    g.sort_unstable();
    g
}

/// Dice coefficient over two sorted bigram multisets.
fn dice(a: &[u64], b: &[u64]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => (common, i, j) = (common + 1, i + 1, j + 1),
        }
    }
    2.0 * common as f32 / (a.len() + b.len()) as f32
}

#[cfg(test)]
mod tests {
    use crate::{exec, test_ctx};

    const SRC: &str = "fn main() {\n    let total = add(1, 2);\n    println!(\"{total}\");\n}\n\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";

    #[tokio::test]
    async fn replaces_a_unique_match() {
        let cx = test_ctx("edit");
        std::fs::write(cx.cwd.join("m.rs"), SRC).unwrap();
        let o = exec(
            "edit",
            r#"{"path":"m.rs","old":"a + b","new":"a - b"}"#,
            &cx,
        )
        .await;
        assert!(o.ok, "{}", o.content);
        let now = std::fs::read_to_string(cx.cwd.join("m.rs")).unwrap();
        assert_eq!(now, SRC.replace("a + b", "a - b"));
        let o = exec(
            "edit",
            r#"{"path":"m.rs","old":"fn ","new":"pub fn "}"#,
            &cx,
        )
        .await;
        assert!(!o.ok && o.content.contains("matches 2 times"));
        let o = exec("edit", r#"{"path":"m.rs","old":"","new":"x"}"#, &cx).await;
        assert!(!o.ok && o.content.contains("use write"));
    }

    #[tokio::test]
    async fn a_miss_shows_the_closest_windows() {
        let cx = test_ctx("edit-miss");
        std::fs::write(cx.cwd.join("m.rs"), SRC).unwrap();
        // Wrong indentation and a typo: the right window should come first.
        let args =
            r#"{"path":"m.rs","old":"  let totl = add(1, 2);\n  println!(\"{total}\");","new":""}"#;
        let o = exec("edit", args, &cx).await;
        assert!(!o.ok);
        assert!(o.content.starts_with("error: `old` was not found in m.rs."));
        let first = o.content.split("\n\n").nth(1).unwrap();
        assert!(first.starts_with("lines 2-3 ("), "{first}");
        assert!(first.contains("     2\t    let total = add(1, 2);\n     3\t    println!"));
        assert_eq!(o.content.matches("% similar):").count(), 1);
        assert_eq!(std::fs::read_to_string(cx.cwd.join("m.rs")).unwrap(), SRC);
    }
}
