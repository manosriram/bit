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
        println!("{}", agent::Context::new(model, None, Vec::new()).prefix());
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
    let dir = config::sessions_dir()?;
    let found = match (&args.resume, args.cont) {
        (Some(id), _) => Some(
            agent::session::find(&dir, id)
                .map_err(|e| format!("error: {e}. hint: the ids are in {}", dir.display()))?,
        ),
        (None, true) => Some(agent::session::latest(&dir, &cwd).ok().flatten().ok_or(
            "error: no session was started in this directory. hint: run bit to start one",
        )?),
        (None, false) => None,
    };
    let max_cost = args.max_cost.or(cfg.max_cost);
    if max_cost.is_some() && cfg.price.is_none() {
        let m = &cfg.model;
        cfg.notices.push(format!("error: the price of {m} is unknown, so max_cost can't be enforced. hint: add [models.\"{m}\"] with input, cached and output prices to the config"));
    }
    let (session, messages, spent) = match found {
        Some(path) => {
            let l = agent::session::Session::open(&path)
                .map_err(|e| format!("error: {}: {e}", path.display()))?;
            if !l.model.is_empty() && l.model != cfg.model {
                let note = format!(
                    "this session used {}; continuing with {}",
                    l.model, cfg.model
                );
                cfg.notices.push(note);
            }
            (l.session, l.messages, l.cost)
        }
        None => (
            agent::session::Session::new(&dir, &new_id(), &cwd, &cfg.model),
            Vec::new(),
            0.0,
        ),
    };
    // One id per conversation routes every request, resumed or not, to the cache that saw it.
    let id = session.id().to_owned();
    if cfg.provider == "opencode-go" {
        cfg.headers
            .push(("x-opencode-session".into(), id.as_str().into()));
    }
    let cache_key = (cfg.provider == "openai").then_some(id.as_str());
    let cx = agent::Context::new(&cfg.model, cache_key, messages);
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
            session,
            price: cfg.price,
            max_cost,
            spent,
        };
        tokio::spawn(agent::run(agent, cx, cmd_rx, ev_tx));
        tui::run(cmd_tx, ev_rx, &cfg.model, mode, &cfg.notices).await
    })
    .map_err(|e| format!("error: terminal: {e}"))?;
    if dir.join(format!("{id}.jsonl")).exists() {
        println!("resume with: bit -r {id}");
    }
    Ok(())
}

/// 12 hex digits: short enough to type after `bit -r`, unique enough for one machine.
fn new_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mixed = t ^ (u64::from(std::process::id()) << 40);
    format!("{:012x}", mixed & 0xffff_ffff_ffff)
}
