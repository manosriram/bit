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
    let mut cfg = config::load(args.model)?;
    // Checked once here so a missing rg is a notice now, not a confusing tool error later.
    if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_err()
    {
        cfg.notices.push("error: rg (ripgrep) is not installed, so grep and glob will fail. hint: install it, e.g. `brew install ripgrep`".into());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("error: current directory: {e}"))?;
    let mode = if args.auto {
        agent::proto::Mode::Auto
    } else {
        agent::proto::Mode::Ask
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let agent = agent::Agent {
            client: agent::Client::new(&cfg.base_url, &cfg.api_key, cfg.headers),
            tools: agent::ToolCtx {
                cwd,
                spill: std::env::temp_dir().join(format!("bit-{}", std::process::id())),
            },
            policy: agent::Policy::new(cfg.allow),
            mode,
        };
        tokio::spawn(agent::run(
            agent,
            agent::Context::new(&cfg.model),
            cmd_rx,
            ev_tx,
        ));
        tui::run(cmd_tx, ev_rx, &cfg.model, mode, &cfg.notices).await
    })
    .map_err(|e| format!("error: terminal: {e}"))
}
