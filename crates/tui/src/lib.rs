mod input;
mod render;

use agent::proto::{Command, Decision, Event, Mode, StopReason, Usage};
use crossterm::event::{self as ct, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use input::Input;
use render::{Line, Term, clip, str_width, wrap};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep_until};

const FRAME: Duration = Duration::from_millis(33);
const SPIN: Duration = Duration::from_millis(100);
const DIM: &str = "2";
const YELLOW: &str = "33";
/// Approval bodies longer than this are cut; the model's call is still shown in full later.
const DETAIL_ROWS: usize = 12;
/// Each pixel is `██`, so strokes line up exactly.
const LOGO: [(&str, &str); 7] = [
    ("1", "  ██        ██    ██"),
    ("1", "  ██              ██"),
    ("1", "  ██████    ██  ██████"),
    ("1", "  ██    ██  ██    ██"),
    ("1", "  ██    ██  ██    ██"),
    ("1", "  ██████    ██      ████"),
    (DIM, "  01100010 01101001 01110100"),
];

/// Runs the terminal UI until the user quits or the agent goes away.
pub async fn run(
    cmds: UnboundedSender<Command>,
    events: UnboundedReceiver<Event>,
    model: &str,
    mode: Mode,
    notices: &[String],
) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), ct::EnableBracketedPaste)?;
    let r = event_loop(cmds, events, model, mode, notices).await;
    let _ = execute!(io::stdout(), ct::DisableBracketedPaste);
    let _ = terminal::disable_raw_mode();
    r
}

async fn event_loop(
    cmds: UnboundedSender<Command>,
    mut events: UnboundedReceiver<Event>,
    model: &str,
    mode: Mode,
    notices: &[String],
) -> io::Result<()> {
    // Asked before the reader thread starts, which would otherwise swallow the reply.
    let (_, height) = terminal::size()?;
    let used = crossterm::cursor::position().map_or(height, |p| p.1 + 1);
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
        auto: mode == Mode::Auto,
        tool: None,
        approval: None,
        section: None,
    };
    ui.logo();
    for n in notices {
        let mut l = Line::new(ui.color);
        ui.done
            .push(l.add(YELLOW, "!").add("", &format!(" {n}")).take());
    }
    let mut term = Term::default();
    term.pin(height, used)?;
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
    auto: bool,
    /// The running tool's name and summary, shown just above the box.
    tool: Option<(Box<str>, Box<str>)>,
    approval: Option<Approval>,
    /// Whether the last scrollback rows were tool lines (`true`) or model text (`false`),
    /// so a blank row can separate the two.
    section: Option<bool>,
}

struct Approval {
    id: Box<str>,
    name: Box<str>,
    summary: Box<str>,
    detail: Box<str>,
}

/// Control characters could move the cursor and break the layout.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Ui {
    fn width(&self) -> usize {
        self.cols.max(30)
    }

    fn logo(&mut self) {
        if self.cols < 40 {
            return;
        }
        let mut l = Line::new(self.color);
        for (sgr, row) in LOGO {
            self.done.push(l.add(sgr, row).take());
        }
        let dir = std::env::current_dir().unwrap_or_default();
        let home = std::env::var_os("HOME").unwrap_or_default();
        let dir = match dir.strip_prefix(&home) {
            Ok(rest) if !home.is_empty() => format!("~/{}", rest.display()),
            _ => dir.display().to_string(),
        };
        let info = format!(
            "  bit {} | {} | {dir}",
            env!("CARGO_PKG_VERSION"),
            self.model
        );
        self.done
            .extend([String::new(), l.add(DIM, clip(&info, self.width())).take()]);
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
                if let Some(a) = &self.approval {
                    let decision = match k.code {
                        KeyCode::Char('y') => Decision::Yes,
                        KeyCode::Char('n') | KeyCode::Esc => Decision::No,
                        KeyCode::Char('a') => Decision::Always,
                        _ => return true,
                    };
                    let id = a.id.clone();
                    let _ = cmds.send(Command::Approve { id, decision });
                    self.approval = None;
                } else if k.code == KeyCode::BackTab {
                    self.auto = !self.auto;
                    let mode = if self.auto { Mode::Auto } else { Mode::Ask };
                    let _ = cmds.send(Command::SetMode(mode));
                } else if k.code == KeyCode::Enter {
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
        self.user(&text);
        let _ = cmds.send(Command::Submit(text));
        self.busy = Some(Instant::now());
    }

    /// A user message in scrollback, typed now or replayed from a resumed session.
    fn user(&mut self, text: &str) {
        self.end_text();
        let mut l = Line::new(self.color);
        self.done.push(String::new());
        for (i, line) in text.lines().enumerate() {
            let line = clean(line);
            for (j, r) in wrap(&line, self.width() - 2).into_iter().enumerate() {
                let mark = if i + j == 0 { ">" } else { " " };
                l.add("34;1", mark).add("1", &format!(" {}", &line[r]));
                self.done.push(l.take());
            }
        }
        self.done.push(String::new());
        self.section = None;
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Text(t) => {
                if !t.is_empty() {
                    self.section(false);
                }
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
            Event::User(t) => self.user(&t),
            Event::Notice(n) => {
                self.end_text();
                let mut l = Line::new(self.color);
                if n.starts_with("error:") {
                    l.add(YELLOW, "!").add("", &format!(" {}", clean(&n)));
                } else {
                    l.add(DIM, &format!("  {}", clean(&n)));
                }
                self.done.push(l.take());
            }
            Event::Approval {
                id,
                name,
                summary,
                detail,
            } => {
                self.end_text();
                self.approval = Some(Approval {
                    id,
                    name,
                    summary,
                    detail,
                });
            }
            Event::ToolStart { name, summary, .. } => {
                self.end_text();
                self.section(true);
                self.tool = Some((name, summary));
            }
            Event::ToolEnd { ok, preview, .. } => {
                if let Some((name, summary)) = self.tool.take() {
                    self.tool_line(&name, &summary, ok, &preview);
                }
            }
            Event::TurnEnd(r) => {
                self.end_text();
                self.tool = None;
                self.approval = None;
                let secs = self.busy.take().map_or(0.0, |t| t.elapsed().as_secs_f32());
                let mut l = Line::new(self.color);
                match r {
                    StopReason::Done => l.add(DIM, &format!("  ({secs:.1}s)")),
                    StopReason::Cancelled => l.add(YELLOW, "!").add("", " cancelled"),
                    StopReason::Error(e) => l.add("31", "!").add("", &format!(" {}", clean(&e))),
                };
                self.done.push(l.take());
            }
        }
    }

    fn end_text(&mut self) {
        if !self.partial.is_empty() {
            self.flush(true);
        }
    }

    fn section(&mut self, tool: bool) {
        if self.section == Some(!tool) {
            self.done.push(String::new());
        }
        self.section = Some(tool);
    }

    /// `* read src/a.rs  [28 lines]`, with the summary cut to fit one row.
    fn tool_line(&mut self, name: &str, summary: &str, ok: bool, preview: &str) {
        let w = self.width();
        let preview = clean(preview);
        let preview = clip(&preview, w / 2);
        let room = w.saturating_sub(str_width(name) + str_width(preview) + 7);
        let summary = clean(summary);
        let mut l = Line::new(self.color);
        if ok {
            l.add("32", "*");
        } else {
            l.add("31", "!");
        }
        l.add("", " ")
            .add("1", name)
            .add("", &format!(" {}", clip(&summary, room)))
            .add(DIM, &format!("  [{preview}]"));
        self.done.push(l.take());
    }

    fn spinner(&self) -> &'static str {
        let ms = self.busy.map_or(0, |t| t.elapsed().as_millis());
        ["|", "/", "-", "\\"][(ms / 100 % 4) as usize]
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
        let mut l = Line::new(self.color);
        if let Some((name, summary)) = &self.tool {
            let room = w.saturating_sub(str_width(name) + 14);
            l.add(YELLOW, self.spinner())
                .add("", " ")
                .add("1", name)
                .add("", &format!(" {}", clip(&clean(summary), room)))
                .add(DIM, "  [running]");
            f.push(l.take());
        }
        f.push(String::new());
        if let Some(a) = &self.approval {
            return self.approval_box(a, f);
        }

        l.add(DIM, "+");
        if let Some(t) = self.busy {
            let e = t.elapsed();
            l.add(DIM, "-- ")
                .add(YELLOW, self.spinner())
                .add("", &format!(" working {:.1}s", e.as_secs_f32()))
                .add(DIM, " ");
            let right = " ctrl-c cancels --+";
            l.fill(DIM, '-', w - right.len()).add(DIM, right);
        } else {
            if self.quit_armed {
                l.add(DIM, "-- ")
                    .add(YELLOW, "ctrl-c again to quit")
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

        f.push(self.status_border(DIM));
        (f, (top + row, col + 2))
    }

    /// The box while a tool waits for approval: the call in the top border, what it would
    /// change inside, and the choices.
    fn approval_box(&self, a: &Approval, mut f: Vec<String>) -> (Vec<String>, (usize, usize)) {
        let w = self.width();
        let mut l = Line::new(self.color);
        let room = w.saturating_sub(str_width(&a.name) + 12);
        l.add(YELLOW, "+-- ")
            .add("33;1", "? ")
            .add("1", &a.name)
            .add("", &format!(" {} ", clip(&clean(&a.summary), room)))
            .fill(YELLOW, '-', w - 1)
            .add(YELLOW, "+");
        f.push(l.take());
        let row = |l: &mut Line, f: &mut Vec<String>, sgr: &str, s: &str| {
            l.add(YELLOW, "| ")
                .add(sgr, clip(s, w - 4))
                .fill("", ' ', w - 2)
                .add(YELLOW, " |");
            f.push(l.take());
        };
        let lines: Vec<String> = a.detail.lines().map(clean).collect();
        for s in lines.iter().take(DETAIL_ROWS) {
            let sgr = match s.get(..2) {
                Some("- ") => "31",
                Some("+ ") => "32",
                _ => "",
            };
            row(&mut l, &mut f, sgr, s);
        }
        if lines.len() > DETAIL_ROWS {
            let more = format!("... {} more lines", lines.len() - DETAIL_ROWS);
            row(&mut l, &mut f, DIM, &more);
        }
        row(&mut l, &mut f, "", "");
        let at = f.len();
        row(
            &mut l,
            &mut f,
            "1",
            "[y]es  [n]o  [a]lways for this session",
        );
        f.push(self.status_border(YELLOW));
        (f, (at, w - 2))
    }

    fn status_border(&self, sgr: &str) -> String {
        let w = self.width();
        let status = self.status();
        let mut l = Line::new(self.color);
        l.add(sgr, "+-- ")
            .add("", clip(&status, w - 6))
            .add(sgr, " ")
            .fill(sgr, '-', w - 1)
            .add(sgr, "+");
        l.take()
    }

    fn status(&self) -> String {
        let mode = if self.auto { "auto" } else { "ask" };
        let Some(u) = self.usage else {
            return format!("{mode} | {}", self.model);
        };
        let ctx = if u.input >= 1000 {
            format!("{:.1}k", f64::from(u.input) / 1000.0)
        } else {
            u.input.to_string()
        };
        format!(
            "{mode} | {} | {ctx} ctx, {}% cached",
            self.model,
            u.cached * 100 / u.input.max(1)
        )
    }
}
