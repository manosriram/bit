use crate::{Ctx, cap, err};
use serde::Deserialize;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Deserialize)]
pub struct Args {
    command: String,
    timeout: Option<u64>,
}

pub async fn run(a: Args, cx: &Ctx) -> Result<String, String> {
    let secs = a.timeout.unwrap_or(120).clamp(1, 600);
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{}", a.command))
        .current_dir(&cx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            err(
                format_args!("cannot start sh: {e}"),
                "check the working directory",
            )
        })?;
    let group = Group(child.id().map_or(0, |id| id as i32));
    let mut pipe = child.stdout.take().expect("stdout is piped");
    let mut buf = Vec::new();
    let done = tokio::time::timeout(Duration::from_secs(secs), async {
        let wait = async {
            let status = child.wait().await;
            // Background jobs would otherwise hold the pipe open and stall the read.
            group.kill();
            status
        };
        tokio::join!(wait, pipe.read_to_end(&mut buf)).0
    })
    .await;
    let out = cap(String::from_utf8_lossy(&buf).into_owned(), cx);
    match done {
        Ok(Ok(s)) if s.success() => Ok(if out.is_empty() {
            "(no output)".into()
        } else {
            out
        }),
        Ok(Ok(s)) => Err(format!(
            "{out}[exit {}]",
            s.code().map_or("killed".into(), |c| c.to_string())
        )),
        Ok(Err(e)) => Err(err(format_args!("waiting for sh: {e}"), "try again")),
        Err(_) => Err(format!(
            "{out}{}",
            err(
                format_args!("timed out after {secs}s and was killed"),
                "pass a larger timeout (up to 600)",
            )
        )),
    }
}

/// Kills the command's whole process group when dropped, which also covers a cancelled
/// turn: dropping the future drops this.
struct Group(i32);

impl Group {
    fn kill(&self) {
        if self.0 > 0 {
            // SAFETY: kill(2) takes plain integers; a negative pid signals the group.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use crate::{exec, test_ctx};
    use std::time::Instant;

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[tokio::test]
    async fn merges_stderr_and_reports_exit() {
        let cx = test_ctx("bash");
        let o = exec("bash", r#"{"command":"echo out; echo err >&2"}"#, &cx).await;
        assert_eq!((o.ok, o.content.as_str()), (true, "out\nerr\n"));
        let o = exec("bash", r#"{"command":"pwd; exit 3"}"#, &cx).await;
        assert!(!o.ok);
        assert!(o.content.ends_with("[exit 3]"), "{}", o.content);
        assert!(o.content.contains("bit-test-"));
    }

    #[tokio::test]
    async fn background_jobs_do_not_hang_and_are_killed() {
        let cx = test_ctx("bash-bg");
        let t = Instant::now();
        let o = exec(
            "bash",
            r#"{"command":"sleep 30 & echo $! > pid; echo hi"}"#,
            &cx,
        )
        .await;
        assert_eq!(o.content, "hi\n");
        assert!(t.elapsed().as_secs() < 5);
        let pid = std::fs::read_to_string(cx.cwd.join("pid")).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!alive(&pid));
    }

    #[tokio::test]
    async fn timeout_kills_the_group() {
        let cx = test_ctx("bash-timeout");
        let t = Instant::now();
        let args = r#"{"command":"sleep 30 & echo $! > pid; echo started; sleep 30","timeout":1}"#;
        let o = exec("bash", args, &cx).await;
        assert!(t.elapsed().as_secs() < 5);
        assert!(!o.ok);
        assert!(
            o.content.starts_with("started\nerror: timed out after 1s"),
            "{}",
            o.content
        );
        let pid = std::fs::read_to_string(cx.cwd.join("pid")).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!alive(&pid));
    }
}
