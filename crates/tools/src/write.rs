use crate::{Ctx, io_err};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Args {
    path: String,
    content: String,
}

pub fn run(a: Args, cx: &Ctx) -> Result<String, String> {
    let path = cx.cwd.join(&a.path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    std::fs::write(&path, &a.content).map_err(|e| io_err(&path, e))?;
    Ok(format!(
        "wrote {} lines to {}",
        a.content.lines().count(),
        a.path
    ))
}

#[cfg(test)]
mod tests {
    use crate::{exec, test_ctx};

    #[tokio::test]
    async fn creates_parents_and_overwrites() {
        let cx = test_ctx("write");
        let o = exec("write", r#"{"path":"x/y/z.txt","content":"a\nb\n"}"#, &cx).await;
        assert_eq!(o.content, "wrote 2 lines to x/y/z.txt");
        exec("write", r#"{"path":"x/y/z.txt","content":"c"}"#, &cx).await;
        assert_eq!(
            std::fs::read_to_string(cx.cwd.join("x/y/z.txt")).unwrap(),
            "c"
        );
    }
}
