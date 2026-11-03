use crate::context::{Context, msg, tool_msg};
use crate::policy::Policy;
use crate::proto::{Command, Decision, Event, Mode, StopReason};
use crate::providers::{self, Auth};
use crate::session::Session;
use serde_json::Value;
use serde_json::value::RawValue;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// One provider as `cmd` resolved it.
#[derive(Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub key_env: String,
    /// The key, or the error a turn shows when it's missing.
    pub key: Result<String, String>,
    /// What the model picker offers for it.
    pub models: Vec<String>,
}

/// The client for `p`, as its spec says. A provider from the config speaks Chat Completions
/// with no extra headers.
pub fn connect(p: &Provider, session: &str) -> llm::Client {
    let key = p.key.as_deref().unwrap_or("");
    let spec = providers::find(&p.name);
    let mut headers = vec![("user-agent", concat!("bit/", env!("CARGO_PKG_VERSION")).into())];
    headers.extend(spec.map(|s| (s.headers)(session, key)).unwrap_or_default());
    let headers = headers.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    llm::Client::new(&p.base_url, key, headers, spec.map_or(llm::Api::Chat, |s| s.wire))
}

/// The `prompt_cache_key` for `provider`, if it routes its cache by one.
pub fn cache_key<'a>(provider: &str, session: &'a str) -> Option<&'a str> {
    providers::find(provider).is_some_and(|s| s.cache_key).then_some(session)
}

/// Everything a session needs besides its history and channels.
pub struct Agent {
    pub client: llm::Client,
    /// Every provider, and the one `client` talks to.
    pub providers: Vec<Provider>,
    pub provider: String,
    pub tools: tools::Ctx,
    pub policy: Policy,
    pub mode: Mode,
    pub session: Session,
    /// Set when the provider's key is missing: each turn ends with this error, unsent.
    pub no_key: Option<String>,
    /// Where a picked model is written, so the next session starts with it. The thinking
    /// level goes beside it, in `thinking`.
    pub remember: Option<PathBuf>,
    /// The thinking level the user picked; see `Command::SetThinking`.
    pub thinking: Option<String>,
    /// `auth.json`, where `/login` saves the ChatGPT tokens.
    pub auth: PathBuf,
    /// No user is present (`bit -p`): a resumed history isn't replayed, a call that needs
    /// approval is declined without asking, and `ask` gets no answer.
    pub print: bool,
}

/// Runs until the command channel closes.
pub async fn run(
    mut agent: Agent,
    mut cx: Context,
    mut cmds: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) {
    if !cx.messages.is_empty() {
        if !agent.print {
            replay(&cx, &events);
        }
        let note =
            format!("resumed {} ({})", agent.session.id(), count(cx.messages.len(), "message"));
        let _ = events.send(Event::Notice(note.into()));
    }
    // Shared by the turn and `listen`, which run side by side in one `select!`. Atomic
    // only because `tokio::spawn` needs the future to be `Send`.
    set_thinking(&agent, &mut cx);
    let auto = AtomicBool::new(agent.mode == Mode::Auto);
    let (replies_tx, mut replies) = unbounded_channel();
    while let Some(cmd) = cmds.recv().await {
        // `None` is `/compact` or `/login`: a turn that sends no message.
        let login = matches!(cmd, Command::Login);
        let text = match cmd {
            Command::Submit(text) => Some(text),
            Command::Compact | Command::Login => None,
            Command::SetMode(m) => {
                auto.store(m == Mode::Auto, Relaxed);
                continue;
            }
            Command::SetModel { provider, model } => {
                set_model(&mut agent, &mut cx, &provider, &model, &events);
                set_thinking(&agent, &mut cx);
                continue;
            }
            Command::SetThinking(level) => {
                agent.thinking = level.map(Into::into);
                set_thinking(&agent, &mut cx);
                let path = agent.remember.as_deref().map(|p| p.with_file_name("thinking"));
                if let Err(e) = remember(path.as_deref(), agent.thinking.as_deref().unwrap_or("")) {
                    let n = format!(
                        "error: could not remember the thinking level: {e}. hint: check that the state directory is writable"
                    );
                    let _ = events.send(Event::Notice(n.into()));
                }
                continue;
            }
            Command::Clear => {
                cx.messages.clear();
                cx.tokens = 0;
                if let Err(e) = agent.session.clear() {
                    let _ = events.send(save_failed(&e));
                }
                continue;
            }
            Command::Approve { .. } | Command::Answer { .. } | Command::Cancel => continue,
        };
        while replies.try_recv().is_ok() {}
        let len = cx.messages.len();
        if let Some(text) = &text {
            cx.messages.push(msg("user", text));
        }
        let mut turn = Turn { a: &mut agent, events: &events, auto: &auto };
        let work = async {
            match text {
                Some(_) => turn.run(&mut cx, &mut replies).await,
                None if login => turn.login(&mut replies).await,
                None => {
                    turn.compact(&mut cx).await;
                    Ok(())
                }
            }
        };
        let reason = tokio::select! {
            r = work => r.err().unwrap_or(StopReason::Done),
            () = listen(&mut cmds, &auto, &replies_tx) => StopReason::Cancelled,
        };
        let why = match reason {
            StopReason::Done => None,
            StopReason::Cancelled => Some("the user cancelled this call"),
            StopReason::Error(_) => Some("the turn failed"),
        };
        if let Some(why) = why {
            cx.repair(len, &format!("error: {why} before it finished"));
        }
        turn.save(&cx);
        if let Err(e) = turn.a.session.sync() {
            turn.save_failed(&e);
        }
        // A send fails only once the UI is gone, and then the command channel closes too.
        let _ = events.send(Event::TurnEnd(reason));
    }
}

struct Turn<'a> {
    a: &'a mut Agent,
    events: &'a UnboundedSender<Event>,
    auto: &'a AtomicBool,
}

impl Turn<'_> {
    async fn run(
        &mut self,
        cx: &mut Context,
        replies: &mut UnboundedReceiver<Command>,
    ) -> Result<(), StopReason> {
        if let Some(e) = &self.a.no_key {
            return Err(StopReason::Error(e.as_str().into()));
        }
        loop {
            if cx.full() {
                self.compact(cx).await;
            }
            let c = self.request(cx).await.map_err(|e| StopReason::Error(e.to_string().into()))?;
            cx.tokens = c.usage.input;
            if let Err(e) = self.a.session.usage(&c.usage) {
                self.save_failed(&e);
            }
            cx.messages.push(c.message);
            self.send(Event::Usage(c.usage));
            for call in &c.tool_calls {
                let (id, name, args) = (&*call.id, &*call.name, &*call.args);
                let summary = tools::summary(name, args);
                let allowed = self.a.policy.check(self.auto.load(Relaxed), name, args)
                    || !self.a.print && {
                        self.send(Event::Approval {
                            id: id.into(),
                            name: name.into(),
                            summary: summary.as_str().into(),
                            detail: tools::detail(name, args).into(),
                        });
                        let d = match reply(replies, id).await {
                            Some(Command::Approve { decision, .. }) => decision,
                            _ => Decision::No,
                        };
                        if d == Decision::Always {
                            self.a.policy.always(name, args);
                        }
                        d != Decision::No
                    };
                self.send(Event::ToolStart {
                    id: id.into(),
                    name: name.into(),
                    summary: summary.into(),
                    diff: diff(name, args).into(),
                });
                let out = match (name, allowed) {
                    ("ask", _) => self.ask(id, args, replies).await,
                    (_, true) => tools::exec(name, args, &self.a.tools).await,
                    (_, false) => {
                        let why = if self.a.print { NOT_ALLOWED } else { DECLINED };
                        tools::Output { ok: false, content: why.into() }
                    }
                };
                let (ok, output) = (out.ok, out.content.as_str().into());
                let preview = preview(name, ok, &out.content).into();
                self.send(Event::ToolEnd { id: id.into(), ok, preview, output });
                cx.messages.push(tool_msg(id, &out.content));
            }
            self.save(cx);
            if c.tool_calls.is_empty() {
                return Ok(());
            }
        }
    }

    /// `/login`: asks for a sign-in or an API key, then which provider, then signs in
    /// through the browser, or takes the key and saves it in `auth.json`, so nobody has to
    /// edit the file.
    async fn login(&mut self, replies: &mut UnboundedReceiver<Command>) -> Result<(), StopReason> {
        let fail = |e: &str| StopReason::Error(e.into());
        let options = vec![SIGN_IN.into(), API_KEY.into()];
        self.send(Event::Ask { id: "login-kind".into(), question: "Log in with?".into(), options });
        let oauth = match answer(replies, "login-kind").await.unwrap_or_default().trim() {
            SIGN_IN => true,
            API_KEY => false,
            _ => return Err(fail(&format!("error: pick {SIGN_IN} or {API_KEY}"))),
        };
        let is_oauth = |p: &&Provider| {
            providers::find(&p.name).is_some_and(|s| matches!(s.auth, Auth::Oauth)) == oauth
        };
        let kind: Vec<&Provider> = self.a.providers.iter().filter(is_oauth).collect();
        let status = |p: &Provider| if p.key.is_ok() { CONNECTED } else { NOT_CONNECTED };
        let options = kind.iter().map(|p| format!("{} {}", p.name, status(p)).into()).collect();
        let question = "Sign in to which provider?".into();
        self.send(Event::Ask { id: "login".into(), question, options });
        let text = answer(replies, "login").await.unwrap_or_default();
        let Some(p) = kind.iter().find(|p| text.split_whitespace().next() == Some(&*p.name)) else {
            let names: Vec<&str> = kind.iter().map(|p| p.name.as_str()).collect();
            return Err(fail(&format!("error: pick one of {}", names.join(", "))));
        };
        let p = (*p).clone();
        let key = match providers::find(&p.name) {
            Some(s) if matches!(s.auth, Auth::Oauth) => {
                let show = |url: &str| self.send(Event::Notice(format!("{OPEN}{url}").into()));
                providers::sign_in(s, &self.a.auth, show).await.map_err(|e| fail(&e))?
            }
            _ => {
                let prompt = format!("enter your {} API key ({}), then Enter", p.name, p.key_env);
                self.send(Event::Secret { id: "key".into(), prompt: prompt.into() });
                let key = answer(replies, "key").await.unwrap_or_default().trim().to_owned();
                if key.is_empty() {
                    return Err(fail("error: no key given. hint: run /login again"));
                }
                let cred = providers::auth::Cred::ApiKey { key: key.clone() };
                providers::auth::put(&self.a.auth, &p.name, cred).map_err(|e| fail(&e))?;
                key
            }
        };
        if let Some(q) = self.a.providers.iter_mut().find(|q| q.name == p.name) {
            q.key = Ok(key);
            if self.a.provider == q.name {
                (self.a.client, self.a.no_key) = (connect(q, self.a.session.id()), None);
            }
        }
        self.send(Event::SignedIn(p.name.as_str().into()));
        self.send(Event::Notice(format!("{} is ready; its models are in /model", p.name).into()));
        Ok(())
    }

    /// Puts the model's question to the user and waits for the answer.
    async fn ask(
        &self,
        id: &str,
        args: &str,
        replies: &mut UnboundedReceiver<Command>,
    ) -> tools::Output {
        if self.a.print {
            return tools::Output { ok: false, content: NO_USER.into() };
        }
        let a: Value = serde_json::from_str(args).unwrap_or_default();
        let options = a["options"].as_array().into_iter().flatten().filter_map(Value::as_str);
        let question = a["question"].as_str().unwrap_or("").into();
        self.send(Event::Ask {
            id: id.into(),
            question,
            options: options.map(Into::into).collect(),
        });
        let content =
            answer(replies, id).await.unwrap_or_else(|| "error: the user gave no answer".into());
        tools::Output { ok: true, content }
    }

    /// Writes the messages the log doesn't have yet. Each call ends a whole step, so the log
    /// never holds a tool call without its result.
    fn save(&mut self, cx: &Context) {
        if let Err(e) = self.a.session.append(&cx.messages[self.a.session.logged..]) {
            self.save_failed(&e);
        }
    }

    /// Replaces the history with a summary and the user's latest message (§4). Runs only
    /// between steps, so no tool call is left without its result.
    async fn compact(&mut self, cx: &mut Context) {
        let text = |m: &RawValue| {
            let v: Value = serde_json::from_str(m.get()).unwrap_or_default();
            v["content"].as_str().unwrap_or("").to_owned()
        };
        let Some(last) = cx.messages.iter().rposition(|m| m.get().contains(r#""role":"user""#))
        else {
            return self.send(Event::Notice("nothing to compact yet".into()));
        };
        let latest = text(&cx.messages[last]);
        let latest = latest.split_once(LATEST).map_or(latest.as_str(), |(_, l)| l).to_owned();
        let body = cx.summary_body(&msg("user", SUMMARIZE));
        let c = match self.a.client.stream(body, |_| {}).await {
            Ok(c) if !text(&c.message).trim().is_empty() => c,
            r => {
                let e = r.map_or_else(|e| e.to_string(), |_| "the model sent no summary".into());
                let n = format!(
                    "error: compaction failed: {e}. hint: the full history is kept; /compact retries"
                );
                return self.send(Event::Notice(n.into()));
            }
        };
        if let Err(e) = self.a.session.usage(&c.usage) {
            self.save_failed(&e);
        }
        let summary = msg("user", &format!("{SUMMARY}\n\n{}{LATEST}{latest}", text(&c.message)));
        if let Err(e) = self.a.session.compact(&summary) {
            self.save_failed(&e);
        }
        let n = cx.messages.len();
        (cx.messages, cx.tokens) = (vec![summary], 0);
        let k = f64::from(c.usage.input) / 1e3;
        self.send(Event::Notice(format!("compacted {n} messages ({k:.1}k tokens)").into()));
    }

    fn save_failed(&self, e: &std::io::Error) {
        self.send(save_failed(e));
    }

    /// One model request, retried after 1s and 3s while nothing from it has reached the screen.
    async fn request(&self, cx: &Context) -> Result<llm::Completion, llm::Error> {
        let mut waits = [1, 3].into_iter();
        loop {
            let mut shown = false;
            let on_part = |p: llm::Part| {
                shown = true;
                self.send(match p {
                    llm::Part::Text(t) => Event::Text(t.into()),
                    llm::Part::Thinking(t) => Event::Thinking(t.into()),
                });
            };
            match (self.a.client.stream(cx.body(), on_part).await, waits.next()) {
                (Err(e), Some(wait)) if !shown && e.retryable() => {
                    self.send(Event::Notice(format!("{e}; retrying in {wait}s").into()));
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                (r, _) => return r,
            }
        }
    }

    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }
}

/// Switches to `model` on `provider`, reconnecting if the provider changed, and records the
/// pick in the session and for the next one.
fn set_model(
    agent: &mut Agent,
    cx: &mut Context,
    provider: &str,
    model: &str,
    events: &UnboundedSender<Event>,
) {
    if provider != agent.provider
        && let Some(p) = agent.providers.iter().find(|p| p.name == provider)
    {
        let id = agent.session.id().to_owned();
        (agent.client, agent.no_key) = (connect(p, &id), p.key.clone().err());
        cx.cache_key = cache_key(&p.name, &id).map(Into::into);
        agent.provider = p.name.clone();
    }
    cx.set_model(model);
    if let Err(e) = agent.session.set_model(&agent.provider, model) {
        let _ = events.send(save_failed(&e));
    }
    let pick = format!("{}/{model}", agent.provider);
    if let Err(e) = remember(agent.remember.as_deref(), &pick) {
        let n = format!(
            "error: could not remember {pick} for next time: {e}. hint: check that the state directory is writable"
        );
        let _ = events.send(Event::Notice(n.into()));
    }
}

/// Asks for the picked thinking level if the current model takes it, in the provider's own
/// fields (a provider from the config gets `reasoning_effort`); else for the default.
fn set_thinking(agent: &Agent, cx: &mut Context) {
    let levels = llm::models::levels(&agent.provider, &cx.model);
    let level = agent.thinking.as_deref().filter(|l| levels.contains(l));
    let spec = providers::find(&agent.provider);
    let to = spec.map_or(providers::reasoning_effort as fn(&str) -> _, |s| s.thinking);
    // bit says `off` where the providers say `none`.
    let fields = level.map(|l| to(if l == "off" { "none" } else { l }));
    cx.set_thinking(fields);
}

fn remember(path: Option<&Path>, pick: &str) -> std::io::Result<()> {
    let Some(path) = path else { return Ok(()) };
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
    std::fs::write(path, pick)
}

fn save_failed(e: &std::io::Error) -> Event {
    Event::Notice(format!("error: could not save the session: {e}. hint: check disk space").into())
}

const SUMMARIZE: &str = "Summarize this conversation so that you can continue the work from the summary alone: it will replace the history. Include the user's goal and requests, decisions made and why, files read or changed (with paths), commands and results that still matter, unresolved errors, the current state and the next steps, and any preferences the user stated. Be specific and terse. Do not call tools.";
const SUMMARY: &str = "[The conversation so far was compacted. Summary:]";
const LATEST: &str = "\n\n[The user's latest message, verbatim:]\n\n";

const DECLINED: &str = "error: the user declined this call. hint: ask them what to do instead";
const NOT_ALLOWED: &str = "error: bit -p runs without a user, so this call needs --mode auto or a [bash] allow entry. hint: do without it, or tell the user how to allow it";
const OPEN: &str = "sign in in your browser; if it didn't open, visit ";
/// The two answers to `/login`'s first question.
const SIGN_IN: &str = "subscription";
const API_KEY: &str = "api key";
/// After each provider's name in `/login`'s list.
const CONNECTED: &str = "· connected";
const NOT_CONNECTED: &str = "· not connected";
const NO_USER: &str =
    "error: bit -p runs without a user to answer. hint: decide yourself and say what you chose";

/// The few words shown at the end of a tool's gutter line.
fn preview(name: &str, ok: bool, content: &str) -> String {
    match (ok, name) {
        (true, "write" | "edit") => "ok".into(),
        (true, "ask") => content.into(),
        (true, _) => count(content.lines().count(), "line"),
        // The gutter adds its own brackets, so `[exit 1]` loses its own.
        (false, _) => {
            let mut lines = content.lines();
            let error = lines.clone().find(|l| l.starts_with("error:") || l.starts_with("[exit"));
            let line = error.or(lines.next_back()).unwrap_or("failed");
            line.trim_start_matches('[').trim_end_matches(']').into()
        }
    }
}

/// The `-`/`+` lines of an `edit`, for scrollback. Other tools' changes are a whole file or
/// a command, which the tool line already sums up.
fn diff(name: &str, args: &str) -> String {
    if name == "edit" { tools::detail(name, args) } else { String::new() }
}

/// `1 line`, `2 lines`.
fn count(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Sends a resumed history to the UI as the events it would have seen live.
fn replay(cx: &Context, events: &UnboundedSender<Event>) {
    let send = |e| {
        let _ = events.send(e);
    };
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for m in &cx.messages {
        let v: Value = serde_json::from_str(m.get()).unwrap_or_default();
        let text = v["content"].as_str().unwrap_or("");
        match v["role"].as_str() {
            Some("user") => match text.strip_prefix(SUMMARY) {
                Some(rest) => {
                    send(Event::Notice("earlier messages were compacted".into()));
                    let latest = rest.split_once(LATEST).map(|(_, l)| l);
                    latest.into_iter().for_each(|l| send(Event::User(l.into())));
                }
                None => send(Event::User(text.into())),
            },
            Some("assistant") => {
                if !text.is_empty() {
                    send(Event::Text(format!("{text}\n").into()));
                }
                for c in v["tool_calls"].as_array().into_iter().flatten() {
                    let s = |k: &str| c["function"][k].as_str().unwrap_or("").to_owned();
                    let id = c["id"].as_str().unwrap_or("").to_owned();
                    calls.push((id, s("name"), s("arguments")));
                }
            }
            Some("tool") => {
                let id = v["tool_call_id"].as_str().unwrap_or("");
                let Some((_, name, args)) = calls.iter().find(|c| c.0 == id) else {
                    continue;
                };
                let last = text.lines().next_back().unwrap_or("");
                let failed = text.starts_with("error:")
                    || last.starts_with("error:")
                    || last.starts_with("[exit ");
                send(Event::ToolStart {
                    id: id.into(),
                    name: name.as_str().into(),
                    summary: tools::summary(name, args).into(),
                    diff: diff(name, args).into(),
                });
                send(Event::ToolEnd {
                    id: id.into(),
                    ok: !failed,
                    preview: preview(name, !failed, text).into(),
                    output: text.into(),
                });
            }
            _ => {}
        }
    }
}

/// The user's reply (`Approve` or `Answer`) for call `id`; `None` if the UI is gone.
async fn reply(replies: &mut UnboundedReceiver<Command>, id: &str) -> Option<Command> {
    loop {
        let c = replies.recv().await?;
        if let Command::Approve { id: i, .. } | Command::Answer { id: i, .. } = &c
            && **i == *id
        {
            return Some(c);
        }
    }
}

/// The text of the user's `Answer` to `id`; `None` if the UI is gone.
async fn answer(replies: &mut UnboundedReceiver<Command>, id: &str) -> Option<String> {
    match reply(replies, id).await? {
        Command::Answer { text, .. } => Some(text),
        _ => None,
    }
}

/// Resolves on `Cancel`, or when the UI hangs up. Meanwhile it forwards replies to the
/// turn and applies mode switches.
async fn listen(
    cmds: &mut UnboundedReceiver<Command>,
    auto: &AtomicBool,
    replies: &UnboundedSender<Command>,
) {
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Command::Cancel => return,
            c @ (Command::Approve { .. } | Command::Answer { .. }) => {
                let _ = replies.send(c);
            }
            Command::SetMode(m) => auto.store(m == Mode::Auto, Relaxed),
            Command::Submit(_)
            | Command::SetModel { .. }
            | Command::SetThinking(_)
            | Command::Clear
            | Command::Compact
            | Command::Login => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Usage;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc::unbounded_channel;

    fn sse(text: &[&str], done: bool) -> String {
        let mut s: String = text
            .iter()
            .map(|t| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{t}\"}}}}]}}\n\n"))
            .collect();
        if done {
            s += "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\ndata: [DONE]\n\n";
        }
        s
    }

    fn call(id: &str, name: &str, args: &str) -> String {
        let delta = serde_json::json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": args}}]});
        format!(
            "data: {}\n\n{}",
            serde_json::json!({"choices": [{"delta": delta}]}),
            sse(&[], true)
        )
    }

    fn agent(addr: std::net::SocketAddr, name: &str) -> Agent {
        let dir = std::env::temp_dir().join(format!("bit-agent-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = Session::new(&dir.join("sessions"), "s1", &dir, "p", "m");
        let auth = dir.join("auth.json");
        Agent {
            client: llm::Client::new(&format!("http://{addr}"), "k", Vec::new(), llm::Api::Chat),
            providers: Vec::new(),
            provider: "p".into(),
            tools: tools::Ctx { spill: dir.join(".spill"), cwd: dir },
            policy: Policy::new(Vec::new()),
            mode: Mode::Ask,
            session,
            no_key: None,
            remember: None,
            thinking: None,
            auth,
            print: false,
        }
    }

    /// Runs `agent` against the scripted `replies`; returns its command sender, its events and
    /// the request bodies the server saw.
    async fn start(
        agent: Agent,
        cx: Context,
        l: TcpListener,
        replies: Vec<String>,
    ) -> (UnboundedSender<Command>, UnboundedReceiver<Event>, UnboundedReceiver<String>) {
        let (body_tx, bodies) = unbounded_channel();
        tokio::spawn(serve(l, replies, body_tx));
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev) = unbounded_channel();
        tokio::spawn(run(agent, cx, cmd_rx, ev_tx));
        (cmd_tx, ev, bodies)
    }

    /// The next event that isn't `Usage`.
    async fn next(ev: &mut UnboundedReceiver<Event>) -> Event {
        loop {
            match ev.recv().await.unwrap() {
                Event::Usage(_) => {}
                e => return e,
            }
        }
    }

    /// Serves one scripted reply per connection and sends each request body to `bodies`.
    /// A reply without `[DONE]` is left open, like a model that is still thinking.
    async fn serve(l: TcpListener, replies: Vec<String>, bodies: UnboundedSender<String>) {
        let mut open = Vec::new();
        for reply in replies {
            let (mut s, _) = l.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0; 4096];
            let body_at = loop {
                let n = s.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = std::str::from_utf8(&req[..i]).unwrap().to_ascii_lowercase();
                let len: usize = head
                    .split("content-length: ")
                    .nth(1)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                if req.len() >= i + 4 + len {
                    break i + 4;
                }
            };
            bodies.send(String::from_utf8(req[body_at..].to_vec()).unwrap()).unwrap();
            // A reply that is a whole HTTP response, such as an error, is sent as is.
            let head = match reply.starts_with("HTTP/") {
                true => "",
                false => {
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
                }
            };
            s.write_all(format!("{head}{reply}").as_bytes()).await.unwrap();
            if head.is_empty() || reply.ends_with("[DONE]\n\n") {
                s.shutdown().await.unwrap();
            } else {
                open.push(s);
            }
        }
    }

    #[tokio::test]
    async fn turns_cancel_and_stable_prefix() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "turns");
        let replies = vec![sse(&["Hel", "lo"], true), sse(&["wai"], false), sse(&["ok"], true)];
        let cx = Context::new("m", None, Vec::new());
        let (cmd_tx, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        let usage = Event::Usage(Usage { input: 10, cached: Some(8), output: 2 });

        cmd_tx.send(Command::Submit("one".into())).unwrap();
        for want in [
            Event::Text("Hel".into()),
            Event::Text("lo".into()),
            usage,
            Event::TurnEnd(StopReason::Done),
        ] {
            assert_eq!(ev.recv().await.unwrap(), want);
        }

        cmd_tx.send(Command::Submit("two".into())).unwrap();
        assert_eq!(ev.recv().await.unwrap(), Event::Text("wai".into()));
        cmd_tx.send(Command::Cancel).unwrap();
        assert_eq!(ev.recv().await.unwrap(), Event::TurnEnd(StopReason::Cancelled));

        cmd_tx.send(Command::Submit("three".into())).unwrap();
        assert_eq!(ev.recv().await.unwrap(), Event::Text("ok".into()));

        let (b1, b2, b3) = (
            bodies.recv().await.unwrap(),
            bodies.recv().await.unwrap(),
            bodies.recv().await.unwrap(),
        );
        let prefix = Context::new("m", None, Vec::new()).prefix().to_owned();
        assert!([&b1, &b2, &b3].iter().all(|b| b.starts_with(&prefix)));
        // Append-only: each request extends the previous one; the cancelled turn is gone.
        let unclosed = |b: &str| b.strip_suffix("]}").unwrap().to_owned();
        assert!(b2.starts_with(&unclosed(&b1)));
        let reply = r#"{"role":"assistant","content":"Hello"}"#;
        let next = r#"{"content":"three","role":"user"}"#;
        assert_eq!(b3, format!("{},{reply},{next}]}}", unclosed(&b1)));
    }

    #[tokio::test]
    async fn tool_calls_ask_then_run_in_auto() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "tools");
        let dir = agent.tools.cwd.clone();
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let replies = vec![
            call("w1", "write", write),
            sse(&["fine"], true),
            call("w2", "write", write),
            sse(&["done"], true),
        ];
        let cx = Context::new("m", None, Vec::new());
        let (cmd_tx, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        let start = |id: &str| Event::ToolStart {
            id: id.into(),
            name: "write".into(),
            summary: "f.txt".into(),
            diff: "".into(),
        };

        cmd_tx.send(Command::Submit("go".into())).unwrap();
        assert_eq!(
            next(&mut ev).await,
            Event::Approval {
                id: "w1".into(),
                name: "write".into(),
                summary: "f.txt".into(),
                detail: "+ hi\n".into(),
            }
        );
        let decision = Decision::No;
        cmd_tx.send(Command::Approve { id: "w1".into(), decision }).unwrap();
        assert_eq!(next(&mut ev).await, start("w1"));
        let end = Event::ToolEnd {
            id: "w1".into(),
            ok: false,
            preview: DECLINED.into(),
            output: DECLINED.into(),
        };
        assert_eq!(next(&mut ev).await, end);
        assert_eq!(next(&mut ev).await, Event::Text("fine".into()));
        assert_eq!(next(&mut ev).await, Event::TurnEnd(StopReason::Done));
        assert!(!dir.join("f.txt").exists());

        cmd_tx.send(Command::SetMode(Mode::Auto)).unwrap();
        cmd_tx.send(Command::Submit("again".into())).unwrap();
        assert_eq!(next(&mut ev).await, start("w2"));
        let end = Event::ToolEnd {
            id: "w2".into(),
            ok: true,
            preview: "ok".into(),
            output: "wrote 1 lines to f.txt".into(),
        };
        assert_eq!(next(&mut ev).await, end);
        assert_eq!(next(&mut ev).await, Event::Text("done".into()));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "hi");

        let _ = bodies.recv().await.unwrap();
        let b2 = bodies.recv().await.unwrap();
        let assistant = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"w1","type":"function","function":{"name":"write","arguments":"{\"path\":\"f.txt\",\"content\":\"hi\"}"}}]}"#;
        let result = format!(r#"{{"content":"{DECLINED}","role":"tool","tool_call_id":"w1"}}]}}"#);
        assert!(b2.ends_with(&format!("{assistant},{result}")), "{b2}");
    }

    #[tokio::test]
    async fn auto_set_mid_turn_applies_to_the_next_call() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "midturn");
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let replies =
            vec![call("w1", "write", write), call("w2", "write", write), sse(&["ok"], true)];
        let (cmds, mut ev, _bodies) =
            start(agent, Context::new("m", None, Vec::new()), l, replies).await;
        cmds.send(Command::Submit("go".into())).unwrap();
        assert!(matches!(next(&mut ev).await, Event::Approval { .. }));
        // Switched while w1 waits: w1 still needs its answer, w2 runs without asking.
        cmds.send(Command::SetMode(Mode::Auto)).unwrap();
        cmds.send(Command::Approve { id: "w1".into(), decision: Decision::Yes }).unwrap();
        let rest = [next(&mut ev).await, next(&mut ev).await, next(&mut ev).await];
        assert!(matches!(&rest[2], Event::ToolStart { id, .. } if &**id == "w2"), "{rest:?}");
    }

    #[tokio::test]
    async fn cancel_keeps_finished_steps_and_answers_open_calls() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "cancel");
        let read = r#"{"path":"nope.txt"}"#;
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let replies =
            vec![call("r1", "read", read), call("w1", "write", write), sse(&["ok"], true)];
        let cx = Context::new("m", None, Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        cmds.send(Command::Submit("go".into())).unwrap();
        while !matches!(next(&mut ev).await, Event::Approval { .. }) {}
        cmds.send(Command::Cancel).unwrap();
        assert_eq!(next(&mut ev).await, Event::TurnEnd(StopReason::Cancelled));
        cmds.send(Command::Submit("next".into())).unwrap();
        assert_eq!(next(&mut ev).await, Event::Text("ok".into()));

        let mut b = String::new();
        for _ in 0..3 {
            b = bodies.recv().await.unwrap();
        }
        // The finished read stays; the write that never ran gets a result saying so.
        let cancelled = r#"{"content":"error: the user cancelled this call before it finished","role":"tool","tool_call_id":"w1"}"#;
        let user = r#"{"content":"next","role":"user"}"#;
        assert!(b.contains(r#""tool_call_id":"r1""#), "{b}");
        assert!(b.ends_with(&format!("{cancelled},{user}]}}")), "{b}");
    }

    #[tokio::test]
    async fn resume_sends_the_bytes_a_running_session_would() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent1 = agent(l.local_addr().unwrap(), "resume");
        let dir = agent1.tools.cwd.join("sessions");
        let replies = vec![sse(&["A"], true), sse(&["B"], true)];
        let cx = Context::new("m", Some("s1"), Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent1, cx, l, replies).await;
        for text in ["one", "two"] {
            cmds.send(Command::Submit(text.into())).unwrap();
            while next(&mut ev).await != Event::TurnEnd(StopReason::Done) {}
        }
        let _ = bodies.recv().await.unwrap();
        let b2 = bodies.recv().await.unwrap();
        drop(cmds);

        let loaded = Session::open(&crate::session::find(&dir, "s1").unwrap()).unwrap();
        assert_eq!(loaded.messages.len(), 4);
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent2 = agent(l.local_addr().unwrap(), "resume2");
        agent2.session = loaded.session;
        let cx = Context::new("m", Some("s1"), loaded.messages);
        let (cmd_tx, mut ev2, mut bodies) = start(agent2, cx, l, vec![sse(&["C"], true)]).await;
        assert_eq!(next(&mut ev2).await, Event::User("one".into()));
        assert_eq!(next(&mut ev2).await, Event::Text("A\n".into()));
        assert_eq!(next(&mut ev2).await, Event::User("two".into()));
        assert_eq!(next(&mut ev2).await, Event::Text("B\n".into()));
        let note = Event::Notice("resumed s1 (4 messages)".into());
        assert_eq!(next(&mut ev2).await, note);
        cmd_tx.send(Command::Submit("three".into())).unwrap();
        assert_eq!(next(&mut ev2).await, Event::Text("C".into()));
        let b3 = bodies.recv().await.unwrap();

        assert!(b2.contains(r#""prompt_cache_key":"s1""#));
        let unclosed = b2.strip_suffix("]}").unwrap();
        let reply = r#"{"role":"assistant","content":"B"}"#;
        let user = r#"{"content":"three","role":"user"}"#;
        assert_eq!(b3, format!("{unclosed},{reply},{user}]}}"));
    }

    #[tokio::test]
    async fn retries_a_server_error_before_any_text() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "retry");
        let error = "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\nconnection: close\r\n\r\nboom";
        let replies = vec![error.into(), sse(&["ok"], true)];
        let cx = Context::new("m", None, Vec::new());
        let (cmds, mut ev, _bodies) = start(agent, cx, l, replies).await;
        cmds.send(Command::Submit("hi".into())).unwrap();
        let retry = Event::Notice("status 500: boom; retrying in 1s".into());
        assert_eq!(next(&mut ev).await, retry);
        assert_eq!(next(&mut ev).await, Event::Text("ok".into()));
        assert_eq!(next(&mut ev).await, Event::TurnEnd(StopReason::Done));
    }

    #[tokio::test]
    async fn set_model_switches_provider_and_model() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        // The starting provider is unreachable; only the switch makes the request land.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let mut agent = agent(dead, "model");
        let state = agent.tools.cwd.join("state/model");
        agent.remember = Some(state.clone());
        agent.providers = vec![Provider {
            name: "openai".into(),
            base_url: format!("http://{addr}"),
            key_env: "K".into(),
            key: Ok("k".into()),
            models: Vec::new(),
        }];
        let cx = Context::new("", None, Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent, cx, l, vec![sse(&["a"], true)]).await;
        cmds.send(Command::SetModel { provider: "openai".into(), model: "m2".into() }).unwrap();
        cmds.send(Command::Submit("hi".into())).unwrap();
        assert_eq!(next(&mut ev).await, Event::Text("a".into()));
        let body = bodies.recv().await.unwrap();
        assert!(body.starts_with(r#"{"model":"m2","prompt_cache_key":"s1","#), "{body}");
        assert_eq!(std::fs::read_to_string(state).unwrap(), "openai/m2");
    }

    #[tokio::test]
    async fn compacts_a_full_window_and_keeps_the_latest_message() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "compact");
        let dir = agent.tools.cwd.join("sessions");
        // 100k input tokens: past 70% of the 128k window an unknown model gets.
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":100000,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";
        let replies =
            vec![full.into(), sse(&["SUM"], true), sse(&["B"], true), sse(&["SUM2"], true)];
        let cx = Context::new("m", None, Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        cmds.send(Command::Submit("one".into())).unwrap();
        while next(&mut ev).await != Event::TurnEnd(StopReason::Done) {}
        cmds.send(Command::Submit("two".into())).unwrap();
        let Event::Notice(n) = next(&mut ev).await else { panic!("expected a notice") };
        assert!(n.starts_with("compacted 3 messages"), "{n}");
        assert_eq!(next(&mut ev).await, Event::Text("B".into()));
        assert_eq!(next(&mut ev).await, Event::TurnEnd(StopReason::Done));

        let (_, b2, b3) = (
            bodies.recv().await.unwrap(),
            bodies.recv().await.unwrap(),
            bodies.recv().await.unwrap(),
        );
        assert!(b2.ends_with(r#"call tools.","role":"user"}]}"#), "{b2}");
        let prefix = Context::new("m", None, Vec::new()).prefix().to_owned();
        let summary = format!("{SUMMARY}\n\nSUM{LATEST}two");
        let want = format!("{prefix},{}]}}", msg("user", &summary).get());
        assert_eq!(b3, want);

        // `/compact` on a compacted history keeps the latest message once, not nested.
        cmds.send(Command::Compact).unwrap();
        assert!(matches!(next(&mut ev).await, Event::Notice(n) if n.starts_with("compacted 2")));
        assert_eq!(next(&mut ev).await, Event::TurnEnd(StopReason::Done));
        let log = Session::open(&dir.join("s1.jsonl")).unwrap().messages;
        let want = msg("user", &format!("{SUMMARY}\n\nSUM2{LATEST}two"));
        assert_eq!(log.iter().map(|m| m.get()).collect::<Vec<_>>(), [want.get()]);
        // Resuming keeps the summary, though it's a lone user message.
        assert_eq!(Context::new("m", None, log).messages.len(), 1);
    }

    #[tokio::test]
    async fn ask_waits_for_the_answer_and_returns_it() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "ask");
        let args = r#"{"question":"Which?","options":["a","b"]}"#;
        let replies = vec![call("q1", "ask", args), sse(&["ok"], true)];
        let cx = Context::new("m", None, Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        cmds.send(Command::Submit("go".into())).unwrap();
        assert!(matches!(next(&mut ev).await, Event::ToolStart { .. }));
        let ask = Event::Ask {
            id: "q1".into(),
            question: "Which?".into(),
            options: vec!["a".into(), "b".into()],
        };
        assert_eq!(next(&mut ev).await, ask);
        cmds.send(Command::Answer { id: "q1".into(), text: "b".into() }).unwrap();
        let end =
            Event::ToolEnd { id: "q1".into(), ok: true, preview: "b".into(), output: "b".into() };
        assert_eq!(next(&mut ev).await, end);
        assert_eq!(next(&mut ev).await, Event::Text("ok".into()));
        let _ = bodies.recv().await.unwrap();
        let b2 = bodies.recv().await.unwrap();
        assert!(b2.ends_with(r#"{"content":"b","role":"tool","tool_call_id":"q1"}]}"#), "{b2}");
    }

    #[tokio::test]
    async fn print_mode_declines_and_answers_without_asking() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = agent(l.local_addr().unwrap(), "print");
        agent.print = true;
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let ask = r#"{"question":"Which?","options":["a","b"]}"#;
        let replies = vec![call("w1", "write", write), call("q1", "ask", ask), sse(&["ok"], true)];
        let (cmds, mut ev, _bodies) =
            start(agent, Context::new("m", None, Vec::new()), l, replies).await;
        cmds.send(Command::Submit("go".into())).unwrap();
        let mut ends = Vec::new();
        loop {
            match next(&mut ev).await {
                Event::ToolEnd { output, .. } => ends.push(output),
                Event::Approval { .. } | Event::Ask { .. } => panic!("print mode asked the user"),
                Event::TurnEnd(r) => break assert_eq!(r, StopReason::Done),
                _ => {}
            }
        }
        assert_eq!(ends, [NOT_ALLOWED.into(), NO_USER.into()]);
    }

    #[tokio::test]
    async fn a_missing_key_ends_the_turn_without_a_request() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = agent(l.local_addr().unwrap(), "nokey");
        agent.no_key = Some("error: K is not set. hint: export it".into());
        let cx = Context::new("m", None, Vec::new());
        let (cmds, mut ev, mut bodies) = start(agent, cx, l, vec![sse(&["x"], true)]).await;
        cmds.send(Command::Submit("hi".into())).unwrap();
        let end = Event::TurnEnd(StopReason::Error("error: K is not set. hint: export it".into()));
        assert_eq!(next(&mut ev).await, end);
        assert!(bodies.try_recv().is_err(), "nothing is sent without a key");
    }

    #[tokio::test]
    async fn login_takes_an_api_key_and_saves_it_in_auth_json() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = agent(l.local_addr().unwrap(), "login");
        let auth = agent.auth.clone();
        agent.providers = vec![Provider {
            name: "openai".into(),
            base_url: String::new(),
            key_env: "OPENAI_API_KEY".into(),
            key: Err("error: not set".into()),
            models: Vec::new(),
        }];
        let (cmds, mut ev, mut bodies) =
            start(agent, Context::new("m", None, vec![]), l, vec![]).await;
        let login = |text: &str| Command::Answer { id: "login".into(), text: text.into() };
        let options = vec!["openai · not connected".into()];
        let ask = Event::Ask {
            id: "login".into(),
            question: "Sign in to which provider?".into(),
            options,
        };

        let kind_ask = Event::Ask {
            id: "login-kind".into(),
            question: "Log in with?".into(),
            options: vec![SIGN_IN.into(), API_KEY.into()],
        };
        let kind = |text: &str| Command::Answer { id: "login-kind".into(), text: text.into() };

        cmds.send(Command::Login).unwrap();
        assert_eq!(next(&mut ev).await, kind_ask);
        cmds.send(kind("nope")).unwrap();
        let end = Event::TurnEnd(StopReason::Error("error: pick subscription or api key".into()));
        assert_eq!(next(&mut ev).await, end);

        // An API-key provider isn't offered as a sign-in.
        cmds.send(Command::Login).unwrap();
        assert_eq!(next(&mut ev).await, kind_ask);
        cmds.send(kind(SIGN_IN)).unwrap();
        let none = Event::Ask {
            id: "login".into(),
            question: "Sign in to which provider?".into(),
            options: vec![],
        };
        assert_eq!(next(&mut ev).await, none);
        cmds.send(login("openai")).unwrap();
        let end = Event::TurnEnd(StopReason::Error("error: pick one of ".into()));
        assert_eq!(next(&mut ev).await, end);

        cmds.send(Command::Login).unwrap();
        assert_eq!(next(&mut ev).await, kind_ask);
        cmds.send(kind(API_KEY)).unwrap();
        assert_eq!(next(&mut ev).await, ask);
        cmds.send(login("gemini")).unwrap();
        let end = Event::TurnEnd(StopReason::Error("error: pick one of openai".into()));
        assert_eq!(next(&mut ev).await, end);

        cmds.send(Command::Login).unwrap();
        assert_eq!(next(&mut ev).await, kind_ask);
        cmds.send(kind(API_KEY)).unwrap();
        assert_eq!(next(&mut ev).await, ask);
        cmds.send(login("openai · not connected")).unwrap();
        let prompt = "enter your openai API key (OPENAI_API_KEY), then Enter".into();
        assert_eq!(next(&mut ev).await, Event::Secret { id: "key".into(), prompt });
        cmds.send(Command::Answer { id: "key".into(), text: " sk-1\n".into() }).unwrap();
        assert_eq!(next(&mut ev).await, Event::SignedIn("openai".into()));
        let ready = Event::Notice("openai is ready; its models are in /model".into());
        assert_eq!(next(&mut ev).await, ready);
        let saved = crate::providers::auth::load(&auth).unwrap();
        assert!(
            matches!(&saved["openai"], crate::providers::auth::Cred::ApiKey { key } if key == "sk-1")
        );
        assert!(bodies.try_recv().is_err(), "a login sends no model request");
    }

    #[tokio::test]
    async fn thinking_goes_only_to_models_that_take_it_and_is_remembered() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = agent(l.local_addr().unwrap(), "thinking");
        let state = agent.tools.cwd.join("model");
        (agent.provider, agent.remember) = ("openai".into(), Some(state.clone()));
        let replies =
            vec![sse(&["a"], true), sse(&["b"], true), sse(&["c"], true), sse(&["d"], true)];
        let (cmds, mut ev, mut bodies) =
            start(agent, Context::new("gpt-5", None, vec![]), l, replies).await;
        let model = |m: &str| Command::SetModel { provider: "openai".into(), model: m.into() };
        let mut turn = async |text: &str| {
            cmds.send(Command::Submit(text.into())).unwrap();
            while !matches!(next(&mut ev).await, Event::TurnEnd(_)) {}
            bodies.recv().await.unwrap()
        };

        cmds.send(Command::SetThinking(Some("low".into()))).unwrap();
        assert!(turn("one").await.contains(r#""reasoning_effort":"low""#));
        assert_eq!(std::fs::read_to_string(state.with_file_name("thinking")).unwrap(), "low");
        cmds.send(model("not-in-the-table")).unwrap();
        assert!(!turn("two").await.contains("reasoning_effort"));
        cmds.send(model("gpt-5")).unwrap();
        assert!(turn("three").await.contains(r#""reasoning_effort":"low""#));
    }

    #[test]
    fn off_is_sent_as_the_providers_none() {
        let mut a = agent("127.0.0.1:9".parse().unwrap(), "off");
        (a.provider, a.thinking) = ("chatgpt".into(), Some("off".into()));
        let mut cx = Context::new("gpt-6-luna", None, vec![]);
        set_thinking(&a, &mut cx);
        assert!(cx.body().contains(r#""reasoning":{"effort":"none"}"#));
    }

    #[test]
    fn previews_count_lines_and_show_the_exit_once() {
        assert_eq!(preview("bash", false, "boom\n[exit 1]"), "exit 1");
        assert_eq!(preview("read", true, "one"), "1 line");
        assert_eq!(preview("read", true, "one\ntwo"), "2 lines");
    }
}
