mod input;
mod render;

use agent::proto::{Command, Event, StopReason, Usage};
use crossterm::event::{self as ct, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use input::Input;
use render::{Line, Term, wrap};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep_until};

const FRAME: Duration = Duration::from_millis(33);
const SPIN: Duration = Duration::from_millis(100);
const DIM: &str = "2";

/// Runs the terminal UI until the user quits or the agent goes away.
pub async fn run(
    cmds: UnboundedSender<Command>,
    events: UnboundedReceiver<Event>,
    model: &str,
    notice: Option<&str>,
) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), ct::EnableBracketedPaste)?;
    let r = event_loop(cmds, events, model, notice).await;
    let _ = execute!(io::stdout(), ct::DisableBracketedPaste);
    let _ = terminal::disable_raw_mode();
    r
}

async fn event_loop(
    cmds: UnboundedSender<Command>,
    mut events: UnboundedReceiver<Event>,
    model: &str,
    notice: Option<&str>,
) -> io::Result<()> {
    // A blocking reader thread avoids pulling in `futures` for crossterm's EventStream.
    let (key_tx, mut keys) = unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(k) = ct::read() {
            if key_tx.send(k).is_err() {
                break;
            }
        }
    });
    let mut ui = Ui {
        model: model.into(),
        color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        cols: terminal::size()?.0.into(),
        input: Input::default(),
        partial: String::new(),
        done: Vec::new(),
        busy: None,
        usage: None,
        quit_armed: false,
    };
    if let Some(n) = notice {
        let mut l = Line::new(ui.color);
        ui.done
            .push(l.add("33", "!").add("", &format!(" {n}")).take());
    }
    let mut term = Term::default();
    let mut last = Instant::now() - FRAME;
    let mut dirty = true;
    loop {
        if dirty && last.elapsed() >= FRAME {
            let (frame, cursor) = ui.frame();
            term.draw(&ui.done, &frame, cursor)?;
            ui.done.clear();
            last = Instant::now();
            dirty = false;
        }
        let wake = if dirty {
            Some(last + FRAME)
        } else {
            ui.busy.map(|_| last + SPIN)
        };
        tokio::select! {
            k = keys.recv() => match k {
                Some(ct::Event::Resize(c, _)) => {
                    ui.cols = c.into();
                    term.clear()?;
                }
                Some(k) => if !ui.key(k, &cmds) { break },
                None => break,
            },
            e = events.recv() => match e {
                Some(e) => ui.event(e),
                None => break,
            },
            () = sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {}
        }
        dirty = true;
    }
    term.clear()
}

struct Ui {
    model: Box<str>,
    color: bool,
    cols: usize,
    input: Input,
    /// The last, unfinished row of model text. Finished rows move to `done` at once, so the
    /// live region stays small however long the reply is.
    partial: String,
    /// Rows waiting to be written to scrollback.
    done: Vec<String>,
    busy: Option<Instant>,
    usage: Option<Usage>,
    quit_armed: bool,
}

impl Ui {
    fn width(&self) -> usize {
        self.cols.max(30)
    }

    /// Returns false when the user quits.
    fn key(&mut self, k: ct::Event, cmds: &UnboundedSender<Command>) -> bool {
        match k {
            ct::Event::Key(k) if k.kind != KeyEventKind::Release => {
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    if self.busy.is_some() {
                        let _ = cmds.send(Command::Cancel);
                    } else if self.quit_armed {
                        return false;
                    } else {
                        self.quit_armed = true;
                    }
                    return true;
                }
                self.quit_armed = false;
                if k.code == KeyCode::Enter {
                    self.submit(cmds);
                } else {
                    self.input.key(k);
                }
            }
            ct::Event::Paste(s) => self.input.insert(&s),
            _ => {}
        }
        true
    }

    fn submit(&mut self, cmds: &UnboundedSender<Command>) {
        if self.busy.is_some() || self.input.text.trim().is_empty() {
            return;
        }
        let text = self.input.take();
        let mut l = Line::new(self.color);
        self.done.push(String::new());
        for (i, line) in text.lines().enumerate() {
            for (j, r) in wrap(line, self.width() - 2).into_iter().enumerate() {
                let mark = if i + j == 0 { ">" } else { " " };
                l.add("34;1", mark).add("1", &format!(" {}", &line[r]));
                self.done.push(l.take());
            }
        }
        self.done.push(String::new());
        let _ = cmds.send(Command::Submit(text));
        self.busy = Some(Instant::now());
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Text(t) => {
                for c in t.chars() {
                    match c {
                        '\n' => self.flush(true),
                        '\t' => self.partial.push_str("    "),
                        c if c.is_control() => {}
                        c => self.partial.push(c),
                    }
                }
                self.flush(false);
            }
            Event::Usage(u) => self.usage = Some(u),
            Event::TurnEnd(r) => {
                if !self.partial.is_empty() {
                    self.flush(true);
                }
                let secs = self.busy.take().map_or(0.0, |t| t.elapsed().as_secs_f32());
                let mut l = Line::new(self.color);
                match r {
                    StopReason::Done => l.add(DIM, &format!("  ({secs:.1}s)")),
                    StopReason::Cancelled => l.add("33", "!").add("", " cancelled"),
                    StopReason::Error(e) => {
                        let e: String = e
                            .chars()
                            .map(|c| if c.is_control() { ' ' } else { c })
                            .collect();
                        l.add("31", "!").add("", &format!(" {e}"))
                    }
                };
                self.done.push(l.take());
            }
        }
    }

    /// Moves finished rows of model text to scrollback; with `all`, the last row too.
    fn flush(&mut self, all: bool) {
        let rows = wrap(&self.partial, self.width() - 2);
        let keep = if all { rows.len() } else { rows.len() - 1 };
        for r in &rows[..keep] {
            self.done
                .push(format!("  {}", self.partial[r.clone()].trim_end()));
        }
        let cut = rows.get(keep).map_or(self.partial.len(), |r| r.start);
        self.partial.drain(..cut);
    }

    /// The live region: unfinished text, then the input box. Returns the rows and the
    /// cursor's (row, column).
    fn frame(&self) -> (Vec<String>, (usize, usize)) {
        let w = self.width();
        let mut f = Vec::new();
        if !self.partial.is_empty() {
            f.push(format!("  {}", self.partial));
        }
        f.push(String::new());

        let mut l = Line::new(self.color);
        l.add(DIM, "+");
        if let Some(t) = self.busy {
            let e = t.elapsed();
            let spin = ["|", "/", "-", "\\"][(e.as_millis() / 100 % 4) as usize];
            l.add(DIM, "-- ")
                .add("33", spin)
                .add("", &format!(" working {:.1}s", e.as_secs_f32()))
                .add(DIM, " ");
            let right = " ctrl-c cancels --+";
            l.fill(DIM, '-', w - right.len()).add(DIM, right);
        } else {
            if self.quit_armed {
                l.add(DIM, "-- ")
                    .add("33", "ctrl-c again to quit")
                    .add(DIM, " ");
            }
            l.fill(DIM, '-', w - 1).add(DIM, "+");
        }
        f.push(l.take());

        let (rows, (row, col)) = self.input.layout(w - 4);
        let top = f.len();
        for (i, r) in rows.iter().enumerate() {
            l.add(DIM, "| ");
            if i == 0 {
                l.add("34;1", ">").add("", &r[1..]);
            } else {
                l.add("", r);
            }
            l.fill("", ' ', w - 2).add(DIM, " |");
            f.push(l.take());
        }

        let status: String = self.status().chars().take(w - 6).collect();
        l.add(DIM, "+-- ")
            .add("", &status)
            .add(DIM, " ")
            .fill(DIM, '-', w - 1)
            .add(DIM, "+");
        f.push(l.take());
        (f, (top + row, col + 2))
    }

    fn status(&self) -> String {
        let Some(u) = self.usage else {
            return self.model.to_string();
        };
        let ctx = if u.input >= 1000 {
            format!("{:.1}k", f64::from(u.input) / 1000.0)
        } else {
            u.input.to_string()
        };
        format!(
            "{} | {ctx} ctx, {}% cached",
            self.model,
            u.cached * 100 / u.input.max(1)
        )
    }
}
