use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bit: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), lexopt::Error> {
    let mut args = lexopt::Parser::from_env();
    while let Some(arg) = args.next()? {
        match arg {
            lexopt::Arg::Long("version") => println!("bit {}", env!("CARGO_PKG_VERSION")),
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(())
}
