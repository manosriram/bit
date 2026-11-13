use crate::{Capped, Ctx, err};
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
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
    rg(vec!["--files", "--sort=path", "-g", &a.pattern], a.path.as_deref(), cx).await
}

/// The project's files for `@` completion, as `rg --files` lists them; empty without `rg`.
pub fn files(cwd: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("rg").arg("--files").current_dir(cwd).output();
    let out = out.map(|o| crate::text(o.stdout)).unwrap_or_default();
    out.lines().take(50_000).map(Into::into).collect()
}

async fn rg(mut args: Vec<&str>, path: Option<&str>, cx: &Ctx) -> Result<String, String> {
    // Without a path rg prints paths relative to cwd, with no `./` prefix.
    if let Some(p) = path {
        args.extend(["--", p]);
    }
    let mut child = Command::new("rg")
        .args(args)
        .current_dir(&cx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                err("rg is not installed", "install ripgrep, e.g. `brew install ripgrep`")
            }
            _ => err(format_args!("cannot start rg: {e}"), "try again"),
        })?;
    // Both pipes at once, so a full stderr can't stall rg while stdout is read.
    let (mut out, mut stderr) = (Capped::new(cx), Vec::new());
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut e = child.stderr.take().expect("stderr is piped");
    let _ = tokio::join!(out.read(stdout), e.read_to_end(&mut stderr));
    let status =
        child.wait().await.map_err(|e| err(format_args!("waiting for rg: {e}"), "try again"))?;
    let stderr = String::from_utf8_lossy(&stderr);
    let stderr = stderr.trim().trim_start_matches("rg: ");
    let out = out.finish();
    match status.code() {
        Some(0) => Ok(out),
        Some(1) => Ok("no matches".into()),
        // rg exits 2 when any path failed, even with matches: keep them, say what failed.
        Some(2) if !out.is_empty() => {
            Ok(format!("{out}[some paths could not be searched: {}]", clip(stderr)))
        }
        _ => Err(err(stderr, "check the pattern and the path")),
    }
}

/// The first line of rg's errors, so one unreadable tree doesn't flood the context.
fn clip(stderr: &str) -> &str {
    stderr.lines().next().unwrap_or("")
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

    #[tokio::test]
    async fn an_unreadable_dir_keeps_the_other_matches() {
        use std::os::unix::fs::PermissionsExt;
        let cx = test_ctx("search-locked");
        std::fs::write(cx.cwd.join("a.txt"), "needle\n").unwrap();
        let locked = cx.cwd.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let o = exec("grep", r#"{"pattern":"needle"}"#, &cx).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(o.ok && o.content.starts_with("a.txt:1:needle\n"), "{}", o.content);
        assert!(o.content.contains("[some paths could not be searched: "));
    }
}
