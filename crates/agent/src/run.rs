use crate::context::{Context, msg};
use crate::proto::{Command, Event, StopReason};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Runs until the command channel closes.
pub async fn run(
    client: llm::Client,
    mut cx: Context,
    mut cmds: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) {
    while let Some(cmd) = cmds.recv().await {
        let Command::Submit(text) = cmd else { continue };
        let len = cx.messages.len();
        cx.messages.push(msg("user", &text));
        let reason = tokio::select! {
            r = turn(&client, &mut cx, &events) => r.map_or_else(|e| StopReason::Error(e.to_string().into()), |()| StopReason::Done),
            () = cancelled(&mut cmds) => StopReason::Cancelled,
        };
        if reason != StopReason::Done {
            cx.messages.truncate(len);
        }
        // A send fails only once the UI is gone, and then the command channel closes too.
        let _ = events.send(Event::TurnEnd(reason));
    }
}

async fn turn(
    client: &llm::Client,
    cx: &mut Context,
    events: &UnboundedSender<Event>,
) -> Result<(), llm::Error> {
    let c = client
        .stream(cx.body(), |t| {
            let _ = events.send(Event::Text(t.into()));
        })
        .await?;
    cx.messages.push(c.message);
    let _ = events.send(Event::Usage(c.usage));
    Ok(())
}

/// Resolves on `Cancel`, or when the UI hangs up. Anything else sent mid-turn is dropped.
async fn cancelled(cmds: &mut UnboundedReceiver<Command>) {
    while let Some(cmd) = cmds.recv().await {
        if cmd == Command::Cancel {
            return;
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
        let client = llm::Client::new(&format!("http://{}", l.local_addr().unwrap()), "k");
        let (body_tx, mut bodies) = unbounded_channel();
        let replies = vec![
            sse(&["Hel", "lo"], true),
            sse(&["wai"], false),
            sse(&["ok"], true),
        ];
        tokio::spawn(serve(l, replies, body_tx));
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, mut ev) = unbounded_channel();
        tokio::spawn(run(client, Context::new("m"), cmd_rx, ev_tx));
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
}
