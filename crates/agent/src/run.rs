use crate::context::{Context, msg, tool_msg};
use crate::policy::Policy;
use crate::proto::{Command, Decision, Event, Mode, StopReason};
use crate::session::Session;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Everything a session needs besides its history and channels.
pub struct Agent {
    pub client: llm::Client,
    pub tools: tools::Ctx,
    pub policy: Policy,
    pub mode: Mode,
    pub session: Session,
}

/// Runs until the command channel closes.
pub async fn run(
    agent: Agent,
    mut cx: Context,
    mut cmds: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) {
    let Agent {
        client,
        tools,
        mut policy,
        mode,
        mut session,
    } = agent;
    if !cx.messages.is_empty() {
        replay(&cx, &events);
        let n = cx.messages.len();
        let note = format!("resumed {} ({n} messages)", session.id());
        let _ = events.send(Event::Notice(note.into()));
    }
    // Shared by the turn and `listen`, which run side by side in one `select!`. Atomic
    // only because `tokio::spawn` needs the future to be `Send`.
    let auto = AtomicBool::new(mode == Mode::Auto);
    let (approve_tx, mut approvals) = unbounded_channel();
    while let Some(cmd) = cmds.recv().await {
        let text = match cmd {
            Command::Submit(text) => text,
            Command::SetMode(m) => {
                auto.store(m == Mode::Auto, Relaxed);
                continue;
            }
            Command::Approve { .. } | Command::Cancel => continue,
        };
        while approvals.try_recv().is_ok() {}
        let len = cx.messages.len();
        cx.messages.push(msg("user", &text));
        let turn = Turn {
            client: &client,
            tools: &tools,
            events: &events,
            auto: &auto,
        };
        let reason = tokio::select! {
            r = turn.run(&mut cx, &mut policy, &mut approvals, &mut session) => r.map_or_else(|e| StopReason::Error(e.to_string().into()), |()| StopReason::Done),
            () = listen(&mut cmds, &auto, &approve_tx) => StopReason::Cancelled,
        };
        match reason {
            StopReason::Done => {}
            StopReason::Cancelled => cx.repair(
                len,
                "error: the user cancelled this call before it finished",
            ),
            StopReason::Error(_) => {
                cx.repair(len, "error: the turn failed before this call finished")
            }
        }
        turn.save(&cx, &mut session);
        if let Err(e) = session.sync() {
            turn.save_failed(&e);
        }
        // A send fails only once the UI is gone, and then the command channel closes too.
        let _ = events.send(Event::TurnEnd(reason));
    }
}

struct Turn<'a> {
    client: &'a llm::Client,
    tools: &'a tools::Ctx,
    events: &'a UnboundedSender<Event>,
    auto: &'a AtomicBool,
}

impl Turn<'_> {
    async fn run(
        &self,
        cx: &mut Context,
        policy: &mut Policy,
        approvals: &mut UnboundedReceiver<(Box<str>, Decision)>,
        session: &mut Session,
    ) -> Result<(), llm::Error> {
        loop {
            let c = self
                .client
                .stream(cx.body(), |t| self.send(Event::Text(t.into())))
                .await?;
            cx.messages.push(c.message);
            self.send(Event::Usage(c.usage));
            if c.tool_calls.is_empty() {
                self.save(cx, session);
                return Ok(());
            }
            for call in c.tool_calls {
                let (id, name) = (&*call.id, &*call.name);
                let summary = tools::summary(name, &call.args);
                let allowed = policy.check(self.auto.load(Relaxed), name, &call.args) || {
                    self.send(Event::Approval {
                        id: id.into(),
                        name: name.into(),
                        summary: summary.clone().into(),
                        detail: tools::detail(name, &call.args).into(),
                    });
                    let d = loop {
                        match approvals.recv().await {
                            Some((for_id, d)) if *for_id == *id => break d,
                            Some(_) => {}
                            None => break Decision::No,
                        }
                    };
                    if d == Decision::Always {
                        policy.always(name, &call.args);
                    }
                    d != Decision::No
                };
                self.send(Event::ToolStart {
                    id: id.into(),
                    name: name.into(),
                    summary: summary.into(),
                });
                let out = if allowed {
                    tools::exec(name, &call.args, self.tools).await
                } else {
                    tools::Output {
                        ok: false,
                        content:
                            "error: the user declined this call. hint: ask them what to do instead"
                                .into(),
                    }
                };
                self.send(Event::ToolEnd {
                    id: id.into(),
                    ok: out.ok,
                    preview: preview(name, out.ok, &out.content).into(),
                });
                cx.messages.push(tool_msg(id, &out.content));
            }
            self.save(cx, session);
        }
    }

    /// Writes the messages the log doesn't have yet. Each call ends a whole step, so the log
    /// never holds a tool call without its result.
    fn save(&self, cx: &Context, session: &mut Session) {
        if let Err(e) = session.append(&cx.messages[session.logged..]) {
            self.save_failed(&e);
        }
    }

    fn save_failed(&self, e: &std::io::Error) {
        let n = format!("error: could not save the session: {e}. hint: check disk space");
        self.send(Event::Notice(n.into()));
    }

    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }
}

/// The few words shown at the end of a tool's gutter line.
fn preview(name: &str, ok: bool, content: &str) -> String {
    let mut lines = content.lines();
    match (ok, name) {
        (true, "write" | "edit") => "ok".into(),
        (true, _) => format!("{} lines", content.lines().count()),
        (false, _) => lines
            .clone()
            .find(|l| l.starts_with("error:") || l.starts_with("[exit"))
            .or(lines.next_back())
            .unwrap_or("failed")
            .into(),
    }
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
            Some("user") => send(Event::User(text.into())),
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
                });
                send(Event::ToolEnd {
                    id: id.into(),
                    ok: !failed,
                    preview: preview(name, !failed, text).into(),
                });
            }
            _ => {}
        }
    }
}

/// Resolves on `Cancel`, or when the UI hangs up. Meanwhile it forwards approvals to the
/// turn and applies mode switches.
async fn listen(
    cmds: &mut UnboundedReceiver<Command>,
    auto: &AtomicBool,
    approve: &UnboundedSender<(Box<str>, Decision)>,
) {
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Command::Cancel => return,
            Command::Approve { id, decision } => {
                let _ = approve.send((id, decision));
            }
            Command::SetMode(m) => auto.store(m == Mode::Auto, Relaxed),
            Command::Submit(_) => {}
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
        let session = Session::new(&dir.join("sessions"), "s1", &dir, "m");
        Agent {
            client: llm::Client::new(&format!("http://{addr}"), "k", Vec::new()),
            tools: tools::Ctx {
                spill: dir.join(".spill"),
                cwd: dir,
            },
            policy: Policy::new(Vec::new()),
            mode: Mode::Ask,
            session,
        }
    }

    /// Runs `agent` against the scripted `replies`; returns its command sender, its events and
    /// the request bodies the server saw.
    async fn start(
        agent: Agent,
        cx: Context,
        l: TcpListener,
        replies: Vec<String>,
    ) -> (
        UnboundedSender<Command>,
        UnboundedReceiver<Event>,
        UnboundedReceiver<String>,
    ) {
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
            bodies
                .send(String::from_utf8(req[body_at..].to_vec()).unwrap())
                .unwrap();
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            s.write_all(format!("{head}{reply}").as_bytes())
                .await
                .unwrap();
            if reply.ends_with("[DONE]\n\n") {
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
        let replies = vec![
            sse(&["Hel", "lo"], true),
            sse(&["wai"], false),
            sse(&["ok"], true),
        ];
        let cx = Context::new("m", None, Vec::new());
        let (cmd_tx, mut ev, mut bodies) = start(agent, cx, l, replies).await;
        let usage = Event::Usage(Usage {
            input: 10,
            cached: 8,
            output: 2,
        });

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
        assert_eq!(
            ev.recv().await.unwrap(),
            Event::TurnEnd(StopReason::Cancelled)
        );

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
        cmd_tx
            .send(Command::Approve {
                id: "w1".into(),
                decision,
            })
            .unwrap();
        assert_eq!(next(&mut ev).await, start("w1"));
        let declined = "error: the user declined this call. hint: ask them what to do instead";
        let end = Event::ToolEnd {
            id: "w1".into(),
            ok: false,
            preview: declined.into(),
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
        };
        assert_eq!(next(&mut ev).await, end);
        assert_eq!(next(&mut ev).await, Event::Text("done".into()));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "hi");

        let _ = bodies.recv().await.unwrap();
        let b2 = bodies.recv().await.unwrap();
        let assistant = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"w1","type":"function","function":{"name":"write","arguments":"{\"path\":\"f.txt\",\"content\":\"hi\"}"}}]}"#;
        let result = format!(r#"{{"content":"{declined}","role":"tool","tool_call_id":"w1"}}]}}"#);
        assert!(b2.ends_with(&format!("{assistant},{result}")), "{b2}");
    }

    #[tokio::test]
    async fn cancel_keeps_finished_steps_and_answers_open_calls() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = agent(l.local_addr().unwrap(), "cancel");
        let read = r#"{"path":"nope.txt"}"#;
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let replies = vec![
            call("r1", "read", read),
            call("w1", "write", write),
            sse(&["ok"], true),
        ];
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
}
