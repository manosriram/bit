use crate::context::{Context, msg, tool_msg};
use crate::policy::Policy;
use crate::proto::{Command, Decision, Event, Mode, StopReason};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Everything a session needs besides its history and channels.
pub struct Agent {
    pub client: llm::Client,
    pub tools: tools::Ctx,
    pub policy: Policy,
    pub mode: Mode,
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
    } = agent;
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
            r = turn.run(&mut cx, &mut policy, &mut approvals) => r.map_or_else(|e| StopReason::Error(e.to_string().into()), |()| StopReason::Done),
            () = listen(&mut cmds, &auto, &approve_tx) => StopReason::Cancelled,
        };
        if reason != StopReason::Done {
            cx.messages.truncate(len);
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
    ) -> Result<(), llm::Error> {
        loop {
            let c = self
                .client
                .stream(cx.body(), |t| self.send(Event::Text(t.into())))
                .await?;
            cx.messages.push(c.message);
            self.send(Event::Usage(c.usage));
            if c.tool_calls.is_empty() {
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
                    preview: preview(name, &out).into(),
                });
                cx.messages.push(tool_msg(id, &out.content));
            }
        }
    }

    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }
}

/// The few words shown at the end of a tool's gutter line.
fn preview(name: &str, out: &tools::Output) -> String {
    let mut lines = out.content.lines();
    match (out.ok, name) {
        (true, "write" | "edit") => "ok".into(),
        (true, _) => format!("{} lines", out.content.lines().count()),
        (false, _) => lines
            .clone()
            .find(|l| l.starts_with("error:") || l.starts_with("[exit"))
            .or(lines.next_back())
            .unwrap_or("failed")
            .into(),
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
        Agent {
            client: llm::Client::new(&format!("http://{addr}"), "k", Vec::new()),
            tools: tools::Ctx {
                spill: dir.join(".spill"),
                cwd: dir,
            },
            policy: Policy::new(Vec::new()),
            mode: Mode::Ask,
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
        let (body_tx, mut bodies) = unbounded_channel();
        let replies = vec![
            sse(&["Hel", "lo"], true),
            sse(&["wai"], false),
            sse(&["ok"], true),
        ];
        tokio::spawn(serve(l, replies, body_tx));
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, mut ev) = unbounded_channel();
        tokio::spawn(run(agent, Context::new("m"), cmd_rx, ev_tx));
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
        let prefix = Context::new("m").prefix().to_owned();
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
        let (body_tx, mut bodies) = unbounded_channel();
        let write = r#"{"path":"f.txt","content":"hi"}"#;
        let replies = vec![
            call("w1", "write", write),
            sse(&["fine"], true),
            call("w2", "write", write),
            sse(&["done"], true),
        ];
        tokio::spawn(serve(l, replies, body_tx));
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, mut ev) = unbounded_channel();
        tokio::spawn(run(agent, Context::new("m"), cmd_rx, ev_tx));
        let mut next = async || loop {
            match ev.recv().await.unwrap() {
                Event::Usage(_) => {}
                e => return e,
            }
        };
        let start = |id: &str| Event::ToolStart {
            id: id.into(),
            name: "write".into(),
            summary: "f.txt".into(),
        };

        cmd_tx.send(Command::Submit("go".into())).unwrap();
        assert_eq!(
            next().await,
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
        assert_eq!(next().await, start("w1"));
        let declined = "error: the user declined this call. hint: ask them what to do instead";
        let end = Event::ToolEnd {
            id: "w1".into(),
            ok: false,
            preview: declined.into(),
        };
        assert_eq!(next().await, end);
        assert_eq!(next().await, Event::Text("fine".into()));
        assert_eq!(next().await, Event::TurnEnd(StopReason::Done));
        assert!(!dir.join("f.txt").exists());

        cmd_tx.send(Command::SetMode(Mode::Auto)).unwrap();
        cmd_tx.send(Command::Submit("again".into())).unwrap();
        assert_eq!(next().await, start("w2"));
        let end = Event::ToolEnd {
            id: "w2".into(),
            ok: true,
            preview: "ok".into(),
        };
        assert_eq!(next().await, end);
        assert_eq!(next().await, Event::Text("done".into()));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "hi");

        let _ = bodies.recv().await.unwrap();
        let b2 = bodies.recv().await.unwrap();
        let assistant = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"w1","type":"function","function":{"name":"write","arguments":"{\"path\":\"f.txt\",\"content\":\"hi\"}"}}]}"#;
        let result = format!(r#"{{"content":"{declined}","role":"tool","tool_call_id":"w1"}}]}}"#);
        assert!(b2.ends_with(&format!("{assistant},{result}")), "{b2}");
    }
}
