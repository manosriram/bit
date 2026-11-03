use agent::proto::{Command, Event, StopReason};
use std::io::Write;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Runs one turn: the model's text goes to `out`, the rest to stderr. The agent runs with
/// `Agent::print`, so it never waits on an approval or an answer.
pub async fn run(
    cmds: UnboundedSender<Command>,
    mut events: UnboundedReceiver<Event>,
    prompt: String,
    out: &mut impl Write,
) -> Result<(), String> {
    let _ = cmds.send(Command::Submit(prompt));
    // Whether `out` ends a line, so a reply after tool calls starts on a new one.
    let mut fresh = true;
    let mut tool = None;
    let r = loop {
        let Some(e) = events.recv().await else { break Err("error: the agent stopped".into()) };
        match e {
            Event::Text(t) if !t.is_empty() => {
                let _ = out.write_all(t.as_bytes()).and_then(|()| out.flush());
                fresh = t.ends_with('\n');
            }
            Event::ToolStart { name, summary, .. } => {
                if !fresh {
                    let _ = out.write_all(b"\n");
                    fresh = true;
                }
                tool = Some((name, summary));
            }
            Event::ToolEnd { ok, preview, .. } => {
                if let Some((name, summary)) = tool.take() {
                    eprintln!("{} {name} {summary}  [{preview}]", if ok { "*" } else { "!" });
                }
            }
            Event::Notice(n) => eprintln!("bit: {n}"),
            Event::TurnEnd(StopReason::Done) => break Ok(()),
            Event::TurnEnd(StopReason::Error(e)) => break Err(e.into()),
            Event::TurnEnd(StopReason::Cancelled) => break Err("error: cancelled".into()),
            _ => {}
        }
    };
    if !fresh {
        let _ = out.write_all(b"\n");
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    /// Plays `events` as the agent would; returns what went to stdout and the result.
    fn play(events: Vec<Event>) -> (String, Result<(), String>) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (cmd_tx, _cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        events.into_iter().for_each(|e| ev_tx.send(e).unwrap());
        drop(ev_tx);
        let mut out = Vec::new();
        let r = rt.block_on(run(cmd_tx, ev_rx, "go".into(), &mut out));
        (String::from_utf8(out).unwrap(), r)
    }

    #[test]
    fn replies_after_a_tool_call_start_a_new_line() {
        let tool = Event::ToolStart { id: "w1".into(), name: "write".into(), summary: "f".into() };
        let (out, r) = play(vec![
            Event::Text("Let me".into()),
            Event::Text(" look.".into()),
            tool,
            Event::ToolEnd {
                id: "w1".into(),
                ok: false,
                preview: "no".into(),
                output: "no".into(),
            },
            Event::Text("Done".into()),
            Event::TurnEnd(StopReason::Done),
        ]);
        assert_eq!((out.as_str(), r), ("Let me look.\nDone\n", Ok(())));
    }

    #[test]
    fn a_failed_turn_is_an_error() {
        let (out, r) = play(vec![Event::TurnEnd(StopReason::Error("error: no key".into()))]);
        assert_eq!((out.as_str(), r), ("", Err("error: no key".into())));
        let (_, r) = play(Vec::new());
        assert_eq!(r, Err("error: the agent stopped".into()));
    }
}
