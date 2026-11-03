mod input;
mod markdown;
mod render;

use agent::proto::{Command, Decision, Event, Mode, StopReason, Usage};
use crossterm::event::{self as ct, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use input::Input;
use markdown::Markdown;
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
/// Rows of the model picker shown at once.
const PICK_ROWS: usize = 10;
/// Last rows of a command's output shown under its tool line.
const TAIL_ROWS: usize = 4;
/// Slash commands: name, arguments, help.
const COMMANDS: [(&str, &str, &str); 4] = [
    ("/clear", "", "start over; the model forgets this conversation"),
    ("/compact", "", "summarize the conversation to free up context"),
    ("/login", "", "sign in with a subscription instead of an API key"),
    ("/model", "[id]", "pick the model, or set it to id"),
];
const CHOICES: [(&str, Decision); 3] = [
    ("[y]es", Decision::Yes),
    ("[n]o", Decision::No),
    ("[a]lways for this session", Decision::Always),
];
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
    (provider, model): (&str, &str),
    providers: &[agent::Provider],
    mode: Mode,
    notices: &[String],
) -> io::Result<()> {
    let mut ui = Ui {
        provider: provider.into(),
        model: model.into(),
        providers: providers.into(),
        color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        cols: terminal::size()?.0.into(),
        auto: mode == Mode::Auto,
        ..Ui::default()
    };
    ui.logo();
    for n in notices {
        ui.event(Event::Notice(n.as_str().into()));
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), ct::EnableBracketedPaste)?;
    // Without the kitty keyboard protocol, terminals send Shift-Enter as plain Enter.
    let kitty = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if kitty {
        let flags = ct::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        execute!(io::stdout(), ct::PushKeyboardEnhancementFlags(flags))?;
    }
    let r = event_loop(ui, cmds, events).await;
    if kitty {
        let _ = execute!(io::stdout(), ct::PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), ct::DisableBracketedPaste);
    let _ = terminal::disable_raw_mode();
    r
}

async fn event_loop(
    mut ui: Ui,
    cmds: UnboundedSender<Command>,
    mut events: UnboundedReceiver<Event>,
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
    let mut term = Term::default();
    term.pin(height, used)?;
    let mut last = Instant::now() - FRAME;
    let mut dirty = true;
    loop {
        if dirty && last.elapsed() >= FRAME {
            if ui.wipe {
                term.wipe(terminal::size()?.1)?;
                ui.wipe = false;
            }
            let (frame, split, cursor) = ui.frame();
            term.draw(&ui.done, &frame, split, cursor)?;
            ui.done.clear();
            last = Instant::now();
            dirty = false;
        }
        let wake = if dirty { Some(last + FRAME) } else { ui.busy.map(|_| last + SPIN) };
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

#[derive(Default)]
struct Ui {
    /// The provider requests go to, and its model: empty until the user picks one.
    provider: Box<str>,
    model: Box<str>,
    /// Every provider, with what the model picker offers for each.
    providers: Vec<agent::Provider>,
    picker: Option<Picker>,
    /// The marked row of the slash-command list.
    menu: usize,
    /// The project's files for `@` completion; read on the first `@` after each send.
    files: Option<Vec<String>>,
    /// Set by `/clear`: wipe the screen and scrollback before the next frame.
    wipe: bool,
    color: bool,
    cols: usize,
    input: Input,
    /// The unfinished line of model text, as markdown. Its finished rows move to `done` at
    /// once, so the live region stays small however long the reply is.
    partial: String,
    /// How many rows of `partial` are already in `done`.
    shown: usize,
    md: Markdown,
    /// Rows waiting to be written to scrollback.
    done: Vec<String>,
    busy: Option<Instant>,
    usage: Option<Usage>,
    /// Input and cached tokens summed over the turn's requests; `None` once one didn't
    /// report cached tokens.
    cache: (u64, Option<u64>),
    quit_armed: bool,
    auto: bool,
    /// The running tool's name and summary, shown just above the box.
    tool: Option<(Box<str>, Box<str>)>,
    approval: Option<Approval>,
    question: Option<Question>,
    /// Set by `something else`: the next `Enter` answers this `ask` call with the input.
    answering: Option<Box<str>>,
    /// Whether the last scrollback rows were tool lines (`true`) or model text (`false`),
    /// so a blank row can separate the two.
    section: Option<bool>,
}

struct Approval {
    id: Box<str>,
    name: Box<str>,
    summary: Box<str>,
    detail: Box<str>,
    /// The marked row of `CHOICES`.
    at: usize,
}

struct Question {
    id: Box<str>,
    text: Box<str>,
    options: Vec<Box<str>>,
    /// The marked row; one past the options is `something else`.
    at: usize,
}

struct Picker {
    query: String,
    /// The marked row among the models that match `query`.
    at: usize,
    /// Whether picking a model also sends the prompt in the input.
    then_submit: bool,
}

/// The commands a `/word` being typed could mean; none once a space follows the word.
/// As (name, row): the name is what gets filled in.
fn commands(text: &str) -> Vec<(String, String)> {
    let Some(word) = text.strip_prefix('/').filter(|w| !w.contains(char::is_whitespace)) else {
        return Vec::new();
    };
    let found = COMMANDS.iter().filter(|c| c.0[1..].contains(&word.to_lowercase()));
    let row =
        |c: &(&str, &str, &str)| format!("{:<13}{}", format!("{} {}", c.0, c.1).trim_end(), c.2);
    found.map(|c| (c.0.into(), row(c))).collect()
}

/// Up to 8 paths containing `query`, ignoring case: file-name matches first, then shorter.
fn file_matches(files: &[String], query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let name = |f: &str| f.rsplit('/').next().unwrap_or(f).to_lowercase();
    let mut found: Vec<&String> = files.iter().filter(|f| f.to_lowercase().contains(&q)).collect();
    found.sort_by_key(|f| (!name(f).contains(&q), f.len()));
    found.into_iter().take(8).cloned().collect()
}

/// The (provider, model) pairs whose `provider model` contains `query`, ignoring case.
fn search<'p>(providers: &'p [agent::Provider], query: &str) -> Vec<(&'p str, &'p str)> {
    let q = query.to_lowercase();
    let all = providers.iter().flat_map(|p| p.models.iter().map(|m| (p.name.as_str(), m.as_str())));
    all.filter(|(p, m)| format!("{p} {m}").to_lowercase().contains(&q)).collect()
}

/// `j`/`k` and the arrows move `at` within `len` rows. `Enter` gives `Some(true)`, `Esc`
/// `Some(false)`.
fn nav(code: KeyCode, at: &mut usize, len: usize) -> Option<bool> {
    match code {
        KeyCode::Up | KeyCode::Char('k') => *at = at.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => *at = (*at + 1).min(len.saturating_sub(1)),
        KeyCode::Enter => return Some(true),
        KeyCode::Esc => return Some(false),
        _ => {}
    }
    None
}

/// Control characters could move the cursor and break the layout.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
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
        let info = format!("  bit v{} | {} | {dir}", env!("CARGO_PKG_VERSION"), self.model());
        self.done.extend([String::new(), l.add(DIM, clip(&info, self.width())).take()]);
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
                // Works everywhere, mid-turn too: the agent checks the mode before each call.
                if k.code == KeyCode::BackTab {
                    self.auto = !self.auto;
                    let mode = if self.auto { Mode::Auto } else { Mode::Ask };
                    let _ = cmds.send(Command::SetMode(mode));
                } else if self.approval.is_some() {
                    self.approval_key(k.code, cmds);
                } else if self.question.is_some() {
                    self.question_key(k.code, cmds);
                } else if self.picker.is_some() {
                    self.picker_key(k.code, cmds);
                } else if self.in_menu(k.code) {
                    self.menu_key(k.code, cmds);
                } else {
                    match k.code {
                        KeyCode::Esc if self.busy.is_some() => {
                            let _ = cmds.send(Command::Cancel);
                        }
                        // Shift-Enter needs the kitty protocol; Alt-Enter and Ctrl-J work without it.
                        KeyCode::Enter
                            if k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                        {
                            self.input.insert("\n");
                        }
                        KeyCode::Char('j') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.input.insert("\n");
                        }
                        KeyCode::Enter => self.submit(cmds),
                        KeyCode::Up => self.input.recall(true),
                        KeyCode::Down => self.input.recall(false),
                        _ => {
                            self.input.key(k);
                            self.menu = 0;
                            self.load_files();
                        }
                    }
                }
            }
            ct::Event::Paste(s) => match &mut self.picker {
                Some(p) => p.query.push_str(&clean(&s)),
                None => {
                    self.input.insert(&s);
                    self.menu = 0;
                    self.load_files();
                }
            },
            _ => {}
        }
        true
    }

    /// `y`/`n`/`a`, or moving and `Enter`; `Esc` declines.
    fn approval_key(&mut self, code: KeyCode, cmds: &UnboundedSender<Command>) {
        let Some(a) = &mut self.approval else { return };
        let decision = match code {
            KeyCode::Char('y') => Some(Decision::Yes),
            KeyCode::Char('n') => Some(Decision::No),
            KeyCode::Char('a') => Some(Decision::Always),
            code => nav(code, &mut a.at, CHOICES.len())
                .map(|pick| if pick { CHOICES[a.at].1 } else { Decision::No }),
        };
        if let Some(decision) = decision {
            let _ = cmds.send(Command::Approve { id: a.id.clone(), decision });
            self.approval = None;
        }
    }

    /// Typing narrows the search; `Enter` sets the marked model, `Esc` closes the picker.
    fn picker_key(&mut self, code: KeyCode, cmds: &UnboundedSender<Command>) {
        let Some(p) = &mut self.picker else { return };
        match code {
            KeyCode::Char(c) => {
                p.query.push(c);
                p.at = 0;
            }
            KeyCode::Backspace if p.query.pop().is_some() => p.at = 0,
            _ => {}
        }
        let found = search(&self.providers, &p.query);
        let pick = match code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc => {
                nav(code, &mut p.at, found.len())
            }
            _ => None,
        };
        match (pick, found.get(p.at)) {
            (Some(true), Some(&(provider, model))) => {
                let then_submit = p.then_submit;
                let (provider, model) = (provider.into(), model.into());
                self.picker = None;
                self.set_model(provider, model, cmds);
                if then_submit {
                    self.submit(cmds);
                }
            }
            (Some(false), _) => self.picker = None,
            _ => {}
        }
    }

    /// Picks an option by digit or by moving and `Enter`; `something else` or `Esc` hands the
    /// input back for a typed answer.
    fn question_key(&mut self, code: KeyCode, cmds: &UnboundedSender<Command>) {
        let Some(q) = &mut self.question else { return };
        let n = q.options.len();
        let pick = match code {
            KeyCode::Char(c @ '1'..='9') if (c as usize - '0' as usize) <= n => {
                q.at = c as usize - '1' as usize;
                Some(true)
            }
            code => nav(code, &mut q.at, n + 1),
        };
        let Some(pick) = pick else { return };
        let id = q.id.clone();
        match q.options.get(q.at).filter(|_| pick) {
            Some(o) => {
                let _ = cmds.send(Command::Answer { id, text: o.to_string() });
            }
            None => self.answering = Some(id),
        }
        self.question = None;
    }

    fn submit(&mut self, cmds: &UnboundedSender<Command>) {
        let typed = self.input.text.trim().to_owned();
        if typed.is_empty() {
            return;
        }
        if let Some(id) = self.answering.take() {
            let _ = cmds.send(Command::Answer { id, text: self.input.take() });
            return;
        }
        if self.busy.is_some() {
            return;
        }
        if let Some(id) =
            typed.strip_prefix("/model").filter(|r| r.is_empty() || r.starts_with(' '))
        {
            self.input.take();
            return match id.trim() {
                "" => self.pick(false),
                id => {
                    let (p, m) = self.resolve(id);
                    self.set_model(p, m, cmds)
                }
            };
        }
        match typed.as_str() {
            "/clear" => {
                self.input.take();
                let _ = cmds.send(Command::Clear);
                self.done.clear();
                (self.usage, self.section, self.wipe) = (None, None, true);
                self.logo();
                self.event(Event::Notice("conversation cleared with /clear".into()));
            }
            "/login" => {
                self.input.take();
                let _ = cmds.send(Command::Login);
                (self.busy, self.cache) = (Some(Instant::now()), (0, Some(0)));
            }
            _ if self.model.is_empty() => self.pick(true),
            "/compact" => {
                self.input.take();
                let _ = cmds.send(Command::Compact);
                (self.busy, self.usage, self.cache) = (Some(Instant::now()), None, (0, Some(0)));
            }
            _ => {
                let text = self.input.take();
                self.user(&text);
                let _ = cmds.send(Command::Submit(text));
                self.files = None;
                (self.busy, self.cache) = (Some(Instant::now()), (0, Some(0)));
            }
        }
    }

    fn set_model(&mut self, provider: Box<str>, model: Box<str>, cmds: &UnboundedSender<Command>) {
        self.event(Event::Notice(format!("model set to {provider}/{model}").into()));
        (self.provider, self.model) = (provider.clone(), model.clone());
        let _ = cmds.send(Command::SetModel { provider, model });
    }

    /// `/model` takes `provider/model`, or a model id: on the current provider if it lists
    /// it, else on the first that does, else on the current one anyway.
    fn resolve(&self, id: &str) -> (Box<str>, Box<str>) {
        if let Some((p, m)) = id.split_once('/')
            && self.providers.iter().any(|x| x.name == p)
        {
            return (p.into(), m.into());
        }
        let lists = |x: &&agent::Provider| x.models.iter().any(|m| m == id);
        let current = self.providers.iter().filter(|x| *x.name == *self.provider);
        let p = current.chain(&self.providers).find(lists).map(|x| x.name.as_str());
        (p.unwrap_or(&self.provider).into(), id.into())
    }

    /// Whether `code` goes to the slash-command list: it is showing, and the key moves in
    /// it, picks from it or closes it. `Esc` while busy still cancels the turn.
    fn in_menu(&self, code: KeyCode) -> bool {
        let keys = match code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Tab => true,
            KeyCode::Esc => self.busy.is_none(),
            _ => false,
        };
        let arrow = matches!(code, KeyCode::Up | KeyCode::Down);
        let esc = code == KeyCode::Esc && self.input.at_word().is_some();
        keys && !esc && !(arrow && self.input.recalling()) && !self.menu().is_empty()
    }

    /// What the input could be completing: slash commands, or files for an `@` word.
    fn menu(&self) -> Vec<(String, String)> {
        let Some((_, q)) = self.input.at_word() else { return commands(&self.input.text) };
        let found = self.files.as_deref().map_or_else(Vec::new, |f| file_matches(f, q));
        found.into_iter().map(|p| (p.clone(), p)).collect()
    }

    fn menu_key(&mut self, code: KeyCode, cmds: &UnboundedSender<Command>) {
        let menu = self.menu();
        let name = menu[self.menu.min(menu.len() - 1)].0.clone();
        match (self.input.at_word(), code) {
            (Some((start, _)), KeyCode::Tab | KeyCode::Enter) => {
                self.input.replace(start, &format!("@{name} "));
                self.menu = 0;
            }
            (Some(_), code) => {
                nav(code, &mut self.menu, menu.len());
            }
            (None, KeyCode::Tab) => self.input.set(&format!("{name} ")),
            (None, code) => match nav(code, &mut self.menu, menu.len()) {
                Some(true) => {
                    self.input.set(&name);
                    self.submit(cmds);
                }
                Some(false) => self.input.set(""),
                None => {}
            },
        }
    }

    /// Reads the project's files the first time an `@` word appears after a send.
    fn load_files(&mut self) {
        if self.files.is_none() && self.input.at_word().is_some() {
            self.files = Some(agent::files(&std::env::current_dir().unwrap_or_default()));
        }
    }

    fn pick(&mut self, then_submit: bool) {
        if self.providers.iter().all(|p| p.models.is_empty()) {
            let n = "error: bit knows no models. hint: pass --model, or add models to a [providers.<name>] in the config";
            return self.event(Event::Notice(n.into()));
        }
        self.picker = Some(Picker { query: String::new(), at: 0, then_submit });
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
            Event::Usage(u) => {
                self.usage = Some(u);
                let (input, cached) = self.cache;
                self.cache = (
                    input + u64::from(u.input),
                    cached.zip(u.cached).map(|(a, b)| a + u64::from(b)),
                );
            }
            Event::SignedIn(p) => {
                self.providers
                    .iter_mut()
                    .filter(|x| *x.name == *p)
                    .for_each(|x| x.key = Ok("".into()));
            }
            Event::User(t) => {
                self.input.remember(&t);
                self.user(&t);
            }
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
            Event::Ask { id, question, options } => {
                self.end_text();
                self.question = Some(Question { id, text: question, options, at: 0 });
            }
            Event::Approval { id, name, summary, detail } => {
                self.end_text();
                self.approval = Some(Approval { id, name, summary, detail, at: 0 });
            }
            Event::ToolStart { name, summary, .. } => {
                self.end_text();
                self.section(true);
                self.tool = Some((name, summary));
            }
            Event::ToolEnd { ok, preview, output, .. } => {
                if let Some((name, summary)) = self.tool.take() {
                    self.tool_line(&name, &summary, ok, &preview);
                    if &*name == "bash" {
                        self.tail(&output);
                    }
                }
            }
            Event::TurnEnd(r) => {
                self.end_text();
                self.tool = None;
                (self.approval, self.question, self.answering) = (None, None, None);
                let secs = self.busy.take().map_or(0.0, |t| t.elapsed().as_secs_f32());
                let mut l = Line::new(self.color);
                match r {
                    StopReason::Done => {
                        let cached = match self.cache {
                            (input, Some(c)) if input > 0 => {
                                format!(", {}% cached", c * 100 / input)
                            }
                            _ => String::new(),
                        };
                        l.add(DIM, &format!("  ({secs:.1}s{cached})"))
                    }
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
        let (sgr, mark) = if ok { ("32", "*") } else { ("31", "!") };
        l.add(sgr, mark)
            .add("", " ")
            .add("1", name)
            .add("", &format!(" {}", clip(&summary, room)))
            .add(DIM, &format!("  [{preview}]"));
        self.done.push(l.take());
    }

    /// A command's last rows, dim under its tool line. A trailing `[exit N]` is left out:
    /// the tool line already shows it.
    fn tail(&mut self, output: &str) {
        let exit = output.rsplit_once("[exit ").filter(|(_, t)| t.ends_with(']'));
        let lines: Vec<&str> = exit.map_or(output, |(h, _)| h).trim_end().lines().collect();
        let skip = lines.len().saturating_sub(TAIL_ROWS);
        let mut l = Line::new(self.color);
        if skip > 0 {
            self.done.push(l.add(DIM, &format!("  | ... {skip} earlier lines")).take());
        }
        for s in &lines[skip..] {
            self.done
                .push(l.add(DIM, &format!("  | {}", clip(&clean(s), self.width() - 4))).take());
        }
    }

    fn spinner(&self) -> &'static str {
        ["|", "/", "-", "\\"][self.tick() % 4]
    }

    /// An 8-bit window scrolling through "bit" in binary, one bit per tick.
    fn bits(&self) -> String {
        const BITS: &str = "011000100110100101110100";
        BITS.chars().cycle().skip(self.tick() % BITS.len()).take(8).collect()
    }

    /// 100 ms steps since the turn started.
    fn tick(&self) -> usize {
        self.busy.map_or(0, |t| t.elapsed().as_millis() as usize / 100)
    }

    /// Moves finished rows of model text to scrollback; with `all`, the last row too, and
    /// the line is done.
    fn flush(&mut self, all: bool) {
        let rows = self.md.rows(&self.partial, self.width(), self.color);
        let keep = if all { rows.len() } else { rows.len().saturating_sub(1) };
        self.done.extend(rows.into_iter().take(keep).skip(self.shown));
        self.shown = self.shown.max(keep);
        if all {
            self.md.end(&self.partial);
            self.partial.clear();
            self.shown = 0;
        }
    }

    /// The live region: unfinished text, then the input box. Returns the rows, where the box
    /// starts (padding goes between the text and the box) and the typing cursor, if any.
    fn frame(&self) -> (Vec<String>, usize, Option<(usize, usize)>) {
        let w = self.width();
        let mut f = Vec::new();
        if !self.partial.is_empty() {
            f.extend(self.md.rows(&self.partial, w, self.color).into_iter().skip(self.shown));
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
        let split = f.len();
        f.push(String::new());
        // A question, an approval or the picker takes the place of the input box.
        let boxed = match (&self.question, &self.approval, &self.picker) {
            (Some(q), ..) => Some(self.question_box(q, &mut f)),
            (_, Some(a), _) => Some(self.approval_box(a, &mut f)),
            (.., Some(p)) => Some(self.picker_box(p, &mut f)),
            _ => None,
        };
        if let Some(cursor) = boxed {
            let cursor = if self.question.is_some() || self.approval.is_some() {
                None
            } else {
                Some(cursor)
            };
            return (f, split, cursor);
        }

        // Command and file suggestions pop up above the box, like a menu from a bottom bar.
        let menu = self.menu();
        for (i, (_, c)) in menu.iter().enumerate() {
            let (sgr, m) = if i == self.menu.min(menu.len() - 1) { ("1", ">") } else { ("", " ") };
            f.push(l.add(sgr, clip(&format!("{m} {c}"), w)).take());
        }
        l.add(DIM, "+");
        if let Some(t) = self.busy {
            let e = t.elapsed();
            l.add(DIM, "-- ")
                .add(YELLOW, &self.bits())
                .add(
                    "",
                    &match self.answering {
                        Some(_) => " type your answer, then Enter".into(),
                        None => format!(" working {:.1}s", e.as_secs_f32()),
                    },
                )
                .add(DIM, " ");
            let right = " esc cancels --+";
            l.fill(DIM, '-', w - right.len()).add(DIM, right);
        } else {
            if self.quit_armed {
                l.add(DIM, "-- ").add(YELLOW, "ctrl-c again to quit").add(DIM, " ");
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
        (f, split, Some((top + row, col + 2)))
    }

    /// The box for the model's question: the question, then the options and `something else`.
    fn question_box(&self, q: &Question, f: &mut Vec<String>) -> (usize, usize) {
        let w = self.width();
        f.push(self.title("question"));
        let text = clean(&q.text);
        for r in wrap(&text, w - 4) {
            f.push(self.row("1", &text[r]));
        }
        f.push(self.row("", ""));
        let numbered =
            q.options.iter().enumerate().map(|(i, o)| format!("{}. {}", i + 1, clean(o)));
        let rows: Vec<String> = numbered.chain(["something else".into()]).collect();
        let row = self.list(f, rows.iter().map(String::as_str), q.at);
        f.push(self.status_border(YELLOW));
        (row, 2)
    }

    /// The model picker: a search row, then the matching models grouped by provider, scrolled
    /// to keep the mark in view.
    fn picker_box(&self, p: &Picker, f: &mut Vec<String>) -> (usize, usize) {
        f.push(self.title("pick a model"));
        let cursor = (f.len(), 10 + str_width(&p.query));
        f.push(self.row("", &format!("search: {}", clean(&p.query))));
        let found = search(&self.providers, &p.query);
        if found.is_empty() {
            f.push(self.row(DIM, "  no match"));
        }
        // A dim header above each provider's group; the mark is on a model row.
        let (mut rows, mut mark) = (Vec::new(), 0);
        for (i, &(prov, m)) in found.iter().enumerate() {
            if i == 0 || found[i - 1].0 != prov {
                let x = self.providers.iter().find(|x| x.name == prov);
                let key = x.map_or(String::new(), |x| {
                    format!("{}{}", x.key_env, if x.key.is_err() { " (not set)" } else { "" })
                });
                rows.push((DIM, format!("  {prov}  {key}")));
            }
            if i == p.at {
                mark = rows.len();
            }
            let (sgr, c) = if i == p.at { ("1", ">") } else { ("", " ") };
            rows.push((sgr, format!("{c}   {m}")));
        }
        let start = mark.saturating_sub(PICK_ROWS / 2).min(rows.len().saturating_sub(PICK_ROWS));
        for (sgr, r) in rows.iter().skip(start).take(PICK_ROWS) {
            f.push(self.row(sgr, r));
        }
        f.push(self.status_border(YELLOW));
        cursor
    }

    /// The box while a tool waits for approval: the call in the top border, what it would
    /// change inside, and the choices.
    fn approval_box(&self, a: &Approval, f: &mut Vec<String>) -> (usize, usize) {
        let w = self.width();
        let summary = clean(&a.summary);
        let room = w.saturating_sub(str_width(&a.name) + 12);
        f.push(self.title(&format!("{} {}", a.name, clip(&summary, room))));
        let lines: Vec<String> = a.detail.lines().map(clean).collect();
        for s in lines.iter().take(DETAIL_ROWS) {
            let sgr = match s.get(..2) {
                Some("- ") => "31",
                Some("+ ") => "32",
                _ => "",
            };
            f.push(self.row(sgr, s));
        }
        if lines.len() > DETAIL_ROWS {
            f.push(self.row(DIM, &format!("... {} more lines", lines.len() - DETAIL_ROWS)));
        }
        f.push(self.row("", ""));
        let row = self.list(f, CHOICES.iter().map(|c| c.0), a.at);
        f.push(self.status_border(YELLOW));
        (row, 2)
    }

    /// The yellow box's top border: `+-- ? <title> ---+`.
    fn title(&self, title: &str) -> String {
        let w = self.width();
        let mut l = Line::new(self.color);
        l.add(YELLOW, "+-- ").add("33;1", "? ").add("1", clip(title, w - 8)).add("", " ");
        l.fill(YELLOW, '-', w - 1).add(YELLOW, "+").take()
    }

    /// One row inside the yellow box.
    fn row(&self, sgr: &str, s: &str) -> String {
        let w = self.width();
        let mut l = Line::new(self.color);
        l.add(YELLOW, "| ").add(sgr, clip(s, w - 4)).fill("", ' ', w - 2).add(YELLOW, " |").take()
    }

    /// Rows of a list with `>` on the marked one; returns the frame row of the mark.
    fn list<'s>(
        &self,
        f: &mut Vec<String>,
        items: impl Iterator<Item = &'s str>,
        at: usize,
    ) -> usize {
        let mut mark = f.len();
        for (i, s) in items.enumerate() {
            if i == at {
                mark = f.len();
            }
            let (sgr, m) = if i == at { ("1", ">") } else { ("", " ") };
            f.push(self.row(sgr, &format!("{m} {s}")));
        }
        mark
    }

    fn model(&self) -> String {
        match self.model.is_empty() {
            true => "no model".into(),
            false => format!("{}/{}", self.provider, self.model),
        }
    }

    fn status_border(&self, sgr: &str) -> String {
        let w = self.width();
        let mode = if self.auto { "auto" } else { "ask" };
        let ctx = match self.usage.map(|u| u.input) {
            Some(n) if n >= 1000 => format!(" | {:.1}k ctx", f64::from(n) / 1e3),
            Some(n) => format!(" | {n} ctx"),
            None => String::new(),
        };
        let status = format!("{mode} | {}{ctx}", self.model());
        let mut l = Line::new(self.color);
        l.add(sgr, "+-- ")
            .add("", clip(&status, w - 6))
            .add(sgr, " ")
            .fill(sgr, '-', w - 1)
            .add(sgr, "+");
        l.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_runs_without_a_model_and_signing_in_sets_the_key() {
        let p = agent::Provider {
            name: "chatgpt".into(),
            base_url: String::new(),
            key_env: "/login".into(),
            key: Err("error: not signed in".into()),
            models: vec!["m".into()],
        };
        let mut ui = Ui { providers: vec![p], cols: 80, ..Ui::default() };
        let (tx, mut rx) = unbounded_channel();
        ui.input.set("/login");
        ui.submit(&tx);
        assert!(matches!(rx.try_recv(), Ok(Command::Login)), "no model is set, yet it's sent");
        assert!(ui.busy.is_some() && ui.input.text.is_empty());
        ui.event(Event::SignedIn("chatgpt".into()));
        assert!(ui.providers[0].key.is_ok());
    }

    #[test]
    fn file_names_match_before_paths() {
        let files: Vec<String> =
            ["src/lib.rs", "render/x.rs", "src/render.rs", "a/b/render.rs"].map(Into::into).into();
        assert_eq!(
            super::file_matches(&files, "REND"),
            ["src/render.rs", "a/b/render.rs", "render/x.rs"]
        );
    }
}
