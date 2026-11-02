use lexopt::{Arg::Long, ValueExt};

#[derive(Default)]
pub struct Args {
    pub model: Option<String>,
    pub auto: bool,
    pub print_prompt: bool,
    pub version: bool,
}

pub fn parse() -> Result<Args, lexopt::Error> {
    let mut a = Args::default();
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("model") => a.model = Some(p.value()?.string()?),
            Long("mode") => {
                a.auto = match p.value()?.string()?.as_str() {
                    "ask" => false,
                    "auto" => true,
                    m => return Err(format!("invalid --mode `{m}`: use ask or auto").into()),
                }
            }
            Long("print-prompt") => a.print_prompt = true,
            Long("version") => a.version = true,
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(a)
}
