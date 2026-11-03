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
        println!("bit v{}", env!("CARGO_PKG_VERSION"));
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
        let model = args.model.as_deref().unwrap_or("");
        println!("{}", agent::Context::new(model, None, Vec::new()).prefix());
        return Ok(());
    }
    let mut cfg = config::load()?;
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
    // `--model` runs on the provider the table lists it under; see ARCHITECTURE.md §10.
    let (configured, _) = &cfg.configured;
    let flag = args
        .model
        .map(|m| (agent::models::provider_of(&m).unwrap_or(configured).to_owned(), Some(m)));
    let start = flag.clone().or(cfg.last.clone().map(|(p, m)| (p, Some(m))));
    let start = start.unwrap_or_else(|| cfg.configured.clone());
    let known = |p: &str| cfg.providers.iter().any(|x| x.name == p);
    let (session, messages, (provider, model)) = match found {
        Some(path) => {
            let l = agent::session::Session::open(&path)
                .map_err(|e| format!("error: {}: {e}", path.display()))?;
            let used = (l.session.provider(), l.session.model());
            let own = (known(used.0) && !used.1.is_empty())
                .then(|| (used.0.to_owned(), Some(used.1.to_owned())));
            let pick = match (flag, own) {
                (Some(f), Some((p, m))) => {
                    let (fp, fm) = (&f.0, f.1.as_deref().unwrap_or(""));
                    let m = m.unwrap_or_default();
                    if (fp, fm) != (&p, &m) {
                        cfg.notices
                            .push(format!("this session used {p}/{m}; continuing with {fp}/{fm}"));
                    }
                    f
                }
                (f, own) => f.or(own).unwrap_or(start),
            };
            (l.session, l.messages, pick)
        }
        None => {
            let (p, m) = start;
            let s =
                agent::session::Session::new(&dir, &new_id(), &cwd, &p, m.as_deref().unwrap_or(""));
            (s, Vec::new(), (p, m))
        }
    };
    let p = cfg.providers.iter().find(|x| x.name == provider).ok_or("error: no provider")?;
    // One id per conversation routes every request, resumed or not, to the cache that saw it.
    let id = session.id().to_owned();
    let model = model.unwrap_or_default();
    let cx = agent::Context::new(&model, agent::cache_key(&provider, &id), messages);
    let (client, no_key) = (agent::connect(p, &id), p.key.clone().err());
    let mode = if args.auto { agent::proto::Mode::Auto } else { agent::proto::Mode::Ask };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let agent = agent::Agent {
            client,
            providers: cfg.providers.clone(),
            provider: provider.clone(),
            no_key,
            tools: agent::ToolCtx {
                cwd,
                spill: std::env::temp_dir().join(format!("bit-{}", std::process::id())),
            },
            policy: agent::Policy::new(cfg.allow),
            mode,
            session,
            remember: cfg.remember,
        };
        tokio::spawn(agent::run(agent, cx, cmd_rx, ev_tx));
        tui::run(cmd_tx, ev_rx, (&provider, &model), &cfg.providers, mode, &cfg.notices).await
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
