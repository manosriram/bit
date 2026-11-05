use lexopt::{
    Arg::{Long, Short, Value},
    ValueExt,
};

#[derive(Default)]
pub struct Args {
    pub model: Option<String>,
    pub auto: bool,
    /// `-c`: continue the last session in this directory.
    pub cont: bool,
    /// `-r <id>`: resume a session by id or id prefix.
    pub resume: Option<String>,
    /// `-p <prompt>`: run one turn without the TUI.
    pub print: Option<String>,
    pub print_prompt: bool,
    pub no_plugins: bool,
    /// `bit plugins [guide]`.
    pub sub: Vec<String>,
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
            Short('c') | Long("continue") => a.cont = true,
            Short('r') | Long("resume") => a.resume = Some(p.value()?.string()?),
            Short('p') | Long("print") => a.print = Some(p.value()?.string()?),
            Long("print-prompt") => a.print_prompt = true,
            Long("version") => a.version = true,
            Long("no-plugins") => a.no_plugins = true,
            Value(v) => a.sub.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(a)
}
