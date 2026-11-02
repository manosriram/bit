use crate::{Ctx, cap, err};
use serde::Deserialize;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Deserialize)]
pub struct Grep {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
}

#[derive(Deserialize)]
pub struct Glob {
    pattern: String,
    path: Option<String>,
}

pub async fn grep(a: Grep, cx: &Ctx) -> Result<String, String> {
    let mut args = vec![
        "-n",
        "-H",
        "--no-heading",
        "--color=never",
        "--max-columns=300",
        "--max-columns-preview",
        "-e",
        &a.pattern,
    ];
    if let Some(g) = &a.glob {
        args.extend(["-g", g]);
    }
    rg(args, a.path.as_deref(), cx).await
}

pub async fn glob(a: Glob, cx: &Ctx) -> Result<String, String> {
    rg(
        vec!["--files", "--sort=path", "-g", &a.pattern],
        a.path.as_deref(),
        cx,
    )
    .await
}

async fn rg(mut args: Vec<&str>, path: Option<&str>, cx: &Ctx) -> Result<String, String> {
    // Without a path rg prints paths relative to cwd, with no `./` prefix.
    if let Some(p) = path {
        args.extend(["--", p]);
    }
    let out = Command::new("rg")
        .args(args)
        .current_dir(&cx.cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => err(
                "rg is not installed",
                "install ripgrep, e.g. `brew install ripgrep`",
            ),
            _ => err(format_args!("cannot start rg: {e}"), "try again"),
        })?;
    match out.status.code() {
        Some(0) => Ok(cap(String::from_utf8_lossy(&out.stdout).into_owned(), cx)),
        Some(1) => Ok("no matches".into()),
        _ => Err(err(
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .trim_start_matches("rg: "),
            "check the pattern and the path",
        )),
    }
}

#[cfg(test)]
mod tests {
    use crate::{exec, test_ctx};

    #[tokio::test]
    async fn grep_and_glob_respect_gitignore() {
        let cx = test_ctx("search");
        let w = |p: &str, s: &str| {
            let p = cx.cwd.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(".gitignore", "target/\n");
        w("src/a.rs", "fn feed() {}\n");
        w("src/b.txt", "feed me\n");
        w("target/c.rs", "fn feed() {}\n");
        // rg only reads .gitignore inside a git repo.
        std::fs::create_dir(cx.cwd.join(".git")).unwrap();

        let o = exec("grep", r#"{"pattern":"feed\\("}"#, &cx).await;
        assert_eq!(o.content, "src/a.rs:1:fn feed() {}\n");
        let o = exec("grep", r#"{"pattern":"feed","glob":"*.txt"}"#, &cx).await;
        assert_eq!(o.content, "src/b.txt:1:feed me\n");
        let o = exec("grep", r#"{"pattern":"nothing here"}"#, &cx).await;
        assert_eq!((o.ok, o.content.as_str()), (true, "no matches"));
        let o = exec("grep", r#"{"pattern":"(unclosed"}"#, &cx).await;
        assert!(!o.ok && o.content.starts_with("error: regex parse error"));

        let o = exec("glob", r#"{"pattern":"*.rs"}"#, &cx).await;
        assert_eq!(o.content, "src/a.rs\n");
        let o = exec("glob", r#"{"pattern":"*","path":"src"}"#, &cx).await;
        assert_eq!(o.content, "src/a.rs\nsrc/b.txt\n");
    }
}
