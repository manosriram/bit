mod args;
mod config;

use std::process::ExitCode;
use tokio::sync::mpsc::unbounded_channel;

fn main() -> ExitCode {
    let args = match args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("bit: {e}");
            return ExitCode::from(2);
        }
    };
    if args.version {
        println!("bit {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bit: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: args::Args) -> Result<(), String> {
    if args.print_prompt {
        let model = args.model.as_deref().unwrap_or("deepseek-chat");
        println!("{}", agent::Context::new(model).prefix());
        return Ok(());
    }
    let cfg = config::load(args.model)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let client = agent::Client::new(&cfg.base_url, &cfg.api_key, cfg.headers);
        tokio::spawn(agent::run(
            client,
            agent::Context::new(&cfg.model),
            cmd_rx,
            ev_tx,
        ));
        tui::run(cmd_tx, ev_rx, &cfg.model, cfg.notice.as_deref()).await
    })
    .map_err(|e| format!("error: terminal: {e}"))
}
