mod args;
mod config;
mod print;
mod skills;

use agent::session::{self, Session};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
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
        let cfg = config::load()?;
        let mut cx = agent::Context::new(model, None, Vec::new());
        cx.add_skills(agent::skills::list(&cfg.skills));
        println!("{}", cx.prefix());
        return Ok(());
    }
    match args.sub.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => {}
        ["plugins", "guide"] => {
            print!("{GUIDE}");
            return Ok(());
        }
        ["plugins"] => return plugins(&config::load()?.plugins),
        ["skills", ..] => return skills::run(&args.sub[1..], &config::load()?.skills),
        _ => {
            let s = args.sub.join(" ");
            return Err(format!(
                "error: unknown command `{s}`. hint: bit plugins, bit plugins guide, bit skills"
            ));
        }
    }
    let cfg = config::load()?;
    let plugins = if args.no_plugins {
        agent::Plugins::default()
    } else {
        agent::Plugins::load(&cfg.plugins).map_err(|e| format!("{e}\n{FIX}"))?
    };
    let skills = if args.no_plugins { Vec::new() } else { agent::skills::list(&cfg.skills) };
    let cwd = std::env::current_dir().map_err(|e| format!("error: current directory: {e}"))?;
    let dir = cfg.sessions.clone();
    let found = match (&args.resume, args.cont) {
        (Some(id), _) => Some(
            session::find(&dir, id)
                .map_err(|e| format!("error: {e}. hint: the ids are in {}", dir.display()))?,
        ),
        (None, true) => Some(session::latest(&dir, &cwd).ok().flatten().ok_or(
            "error: no session was started in this directory. hint: run bit to start one",
        )?),
        (None, false) => None,
    };
    // `--model` runs on the provider the table lists it under; see ARCHITECTURE.md §10.
    let flag = args
        .model
        .map(|m| (agent::models::provider_of(&m).unwrap_or(&cfg.configured.0).to_owned(), m));
    let (session, messages, used) = match found {
        Some(path) => {
            let l = Session::open(&path).map_err(|e| format!("error: {}: {e}", path.display()))?;
            let (p, m) = (l.session.provider(), l.session.model());
            let known = cfg.providers.iter().any(|x| x.name == p) && !m.is_empty();
            let used = known.then(|| (p.to_owned(), m.to_owned()));
            (Some(l.session), l.messages, used)
        }
        None => (None, Vec::new(), None),
    };
    let mut notices = Vec::new();
    if let (Some(f), Some(u)) = (&flag, &used)
        && f != u
    {
        notices.push(format!("this session used {}/{}; continuing with {}/{}", u.0, u.1, f.0, f.1));
    }
    let (provider, model) = flag.or(used).or(cfg.last).unwrap_or(cfg.configured);
    let print = args.print.is_some();
    if print && model.is_empty() {
        return Err("error: no model is set. hint: pass --model, or set model in the config".into());
    }
    let session = session.unwrap_or_else(|| Session::new(&dir, &new_id(), &cwd, &provider, &model));
    let p = cfg.providers.iter().find(|x| x.name == provider).ok_or("error: no provider")?;
    // One id per conversation routes every request, resumed or not, to the cache that saw it.
    let id = session.id().to_owned();
    let mut cx = agent::Context::new(&model, agent::cache_key(&provider, &id), messages);
    cx.add_tools(plugins.schemas());
    cx.add_skills(skills.clone());
    let (client, no_key) = (agent::connect(p, &id), p.key.clone().err());
    let mode = if args.auto { agent::proto::Mode::Auto } else { agent::proto::Mode::Ask };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let r = rt.block_on(async {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let agent = agent::Agent {
            client,
            providers: cfg.providers.clone(),
            provider: provider.clone(),
            no_key,
            tools: agent::ToolCtx {
                cwd: cwd.clone(),
                spill: std::env::temp_dir().join(format!("bit-{}", std::process::id())),
            },
            policy: agent::Policy::new(cfg.allow, plugins.tools()),
            mode,
            session,
            remember: Some(cfg.remember),
            thinking: cfg.thinking.clone(),
            auth: cfg.auth,
            print,
            plugins: Arc::new(plugins),
        };
        tokio::spawn(agent::run(agent, cx, cmd_rx, ev_tx));
        match args.print {
            Some(prompt) => {
                notices.iter().for_each(|n| eprintln!("bit: {n}"));
                print::run(cmd_tx, ev_rx, prompt, &mut std::io::stdout()).await
            }
            None => {
                let at = (provider.as_str(), model.as_str(), cfg.thinking.as_deref());
                tui::run(
                    cmd_tx,
                    ev_rx,
                    at,
                    &cfg.providers,
                    mode,
                    &notices,
                    (
                        cfg.skills.clone(),
                        cfg.sessions.clone(),
                        cwd.clone(),
                        id.clone().into_boxed_str(),
                    ),
                )
                .await
                .map_err(|e| format!("error: terminal: {e}"))
            }
        }
    });
    if dir.join(format!("{id}.jsonl")).exists() {
        // In print mode stdout holds only the model's text.
        match print {
            true => eprintln!("resume with: bit -r {id}"),
            false => println!("resume with: bit -r {id}"),
        }
    }
    r
}

/// What `bit plugins guide` prints: how to write a plugin, as one working example.
const GUIDE: &str = include_str!("../../../docs/plugins/example.lua");
const FIX: &str = "hint: fix the file (bit plugins checks them all), or run bit --no-plugins";

/// `bit plugins`: each plugin with its hooks, or why it doesn't load.
fn plugins(dir: &Path) -> Result<(), String> {
    let files = agent::Plugins::check(dir)?;
    if files.is_empty() {
        println!("no plugins in {}", dir.display());
    }
    let mut failed = false;
    for (name, r) in files {
        match r {
            Ok(hooks) => println!("✓ {name}: {}", hooks.join(", ")),
            Err(e) => {
                failed = true;
                println!("✗ {name}: {e}");
            }
        }
    }
    if failed { Err(format!("error: a plugin doesn't load. {FIX}")) } else { Ok(()) }
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
