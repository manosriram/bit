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
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep_until};

const FRAME: Duration = Duration::from_millis(33);
const SPIN: Duration = Duration::from_millis(100);
const DIM: &str = "2";
/// Approval bodies longer than this are cut; the model's call is still shown in full later.
const DETAIL_ROWS: usize = 12;
/// Rows of the model picker shown at once.
const PICK_ROWS: usize = 10;
/// Last rows of a command's output shown under its tool line.
const TAIL_ROWS: usize = 4;
/// Slash commands: name, arguments, help.
const COMMANDS: [(&str, &str, &str); 7] = [
    ("/clear", "", "start over; the model forgets this conversation"),
    ("/compact", "", "summarize the conversation to free up context"),
    ("/login", "", "add a provider: paste its API key, or sign in"),
    ("/model", "[id]", "pick the model, or set it to id"),
    ("/sessions", "", "list sessions and resume another one"),
    ("/skills", "[add <repo>]", "show installed skills or add one"),
    ("/thinking", "[level]", "pick how hard the model thinks, or set it to level"),
];
/// The id of the `/thinking` picker's question.
const THINKING: &str = "thinking";
/// The id of the `/sessions` picker's question.
const SESSIONS: &str = "sessions";
const CHOICES: [(&str, Decision); 3] = [
    ("[y]es", Decision::Yes),
    ("[n]o", Decision::No),
    ("[a]lways for this session", Decision::Always),
];
/// The amber logo of `assets/logo-horizontal-*.svg`, one char per pixel: `p` the flag's
/// pole, `f` the flag, `t` the lettering. Two rows share a terminal row as half blocks,
/// which keeps the pixels square.
const LOGO: [&str; 10] = [
    " pfff         ",
    "ppfff         ",
    "p fff         ",
    "p             ",
    "p  t    t  t  ",
    "p  t       t  ",
    "p  ttt  t ttt ",
    "p  t  t t  t  ",
    "p  t  t t  t  ",
    "p  ttt  t   tt",
];
/// The accent, for the logo's flag, the spinner, approvals and warnings: the brand amber,
/// #F2B544, as the nearest xterm-256 colour, so terminals without true colour show it too.
const AMBER: &str = "38;5;215";
/// The logo's pole.
const GREY: &str = "90";

/// Runs the terminal UI until the user quits or the agent goes away.
pub async fn run(
    cmds: UnboundedSender<Command>,
    events: UnboundedReceiver<Event>,
    (provider, model, thinking): (&str, &str, Option<&str>),
    providers: &[agent::Provider],
    mode: Mode,
    notices: &[String],
    (skills_dir, sessions, cwd, session): (PathBuf, PathBuf, PathBuf, Box<str>),
) -> io::Result<()> {
    let mut ui = Ui {
        provider: provider.into(),
        model: model.into(),
        thinking: thinking.map(Into::into),
        providers: providers.into(),
        skills_dir,
        sessions,
        cwd,
        session,
        color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        cols: terminal::size()?.0.into(),
        rows: terminal::size()?.1.into(),
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
                Some(ct::Event::Resize(c, r)) => {
                    ui.cols = c.into();
                    ui.rows = r.into();
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
    /// The provider requests go to, and its model: empty until the user picks one.
    provider: Box<str>,
    model: Box<str>,
    /// The thinking level picked with `/thinking`; used only by models that take it.
    thinking: Option<Box<str>>,
    /// Every provider, with what the model picker offers for each.
    providers: Vec<agent::Provider>,
    /// Where the skills live, scanned when `/skills` is typed.
    skills_dir: PathBuf,
    /// Where the session logs live, for `/sessions`.
    sessions: PathBuf,
    /// The working directory, to show on sessions from other directories.
    cwd: PathBuf,
    /// The current session id, to mark in the `/sessions` list.
    session: Box<str>,
    picker: Option<Picker>,
    /// The marked row of the slash-command list.
    menu: usize,
    /// The project's files for `@` completion; read on the first `@` after each send.
    files: Option<Vec<String>>,
    /// Set by `/clear`: wipe the screen and scrollback before the next frame.
    wipe: bool,
    color: bool,
    cols: usize,
    rows: usize,
    input: Input,
    /// The unfinished line of model text, as markdown. Its finished rows move to `done` at
    /// once, so the live region stays small however long the reply is.
    partial: String,
    /// How many rows of `partial` are already in `done`.
    shown: usize,
    /// The unfinished line of the model's reasoning, drawn grey as plain text; `None` when
    /// the last output wasn't reasoning.
    thought: Option<String>,
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
    /// The running tool's name and summary, shown just above the box, and its diff.
    tool: Option<(Box<str>, Box<str>, Box<str>)>,
    approval: Option<Approval>,
    question: Option<Question>,
    /// Set by `something else`: the next `Enter` answers this `ask` call with the input.
    answering: Option<Box<str>>,
    /// The answer is an API key, with this prompt: drawn as dots, never kept in the history.
    secret: Option<Box<str>>,
    /// Whether the last scrollback rows were tool lines (`true`) or model text (`false`),
    /// so a blank row can separate the two.
    section: Option<bool>,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            provider: Default::default(),
            model: Default::default(),
            thinking: Default::default(),
            providers: Default::default(),
            skills_dir: Default::default(),
            sessions: Default::default(),
            cwd: Default::default(),
            session: Default::default(),
            picker: Default::default(),
            menu: Default::default(),
            files: Default::default(),
            wipe: Default::default(),
            color: Default::default(),
            cols: Default::default(),
            rows: 24,
            input: Default::default(),
            partial: Default::default(),
            shown: Default::default(),
            thought: Default::default(),
            md: Default::default(),
            done: Default::default(),
            busy: Default::default(),
            usage: Default::default(),
            cache: Default::default(),
            quit_armed: Default::default(),
            auto: Default::default(),
            tool: Default::default(),
            approval: Default::default(),
            question: Default::default(),
            answering: Default::default(),
            secret: Default::default(),
            section: Default::default(),
        }
    }
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

/// Red for a removed line, green for an added one.
/// The colour of one logo pixel; `None` for the background.
fn fg(pixel: char) -> Option<&'static str> {
    match pixel {
        'p' => Some(GREY),
        'f' => Some(AMBER),
        't' => Some("39"),
        _ => None,
    }
}

fn diff_sgr(line: &str) -> &'static str {
    match line.get(..2) {
        Some("- ") => "31",
        Some("+ ") => "32",
        _ => "",
    }
}

/// Control characters could move the cursor and break the layout.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// A human-readable relative time like "2h" or "3d".
fn ago(t: SystemTime) -> String {
    let d = SystemTime::now().duration_since(t).unwrap_or_default();
    if d < Duration::from_secs(60) {
        "now".into()
    } else if d < Duration::from_secs(3600) {
        format!("{}m", d.as_secs() / 60)
    } else if d < Duration::from_secs(86400) {
        format!("{}h", d.as_secs() / 3600)
    } else if d < Duration::from_secs(604800) {
        format!("{}d", d.as_secs() / 86400)
    } else {
        format!("{}w", d.as_secs() / 604800)
    }
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
        for pair in LOGO.chunks(2) {
            l.add("", "  ");
            for (top, bottom) in pair[0].chars().zip(pair[1].chars()) {
                // shortcut: no cell stacks two colours, so a full block takes the top's.
                let (sgr, c) = match (fg(top), fg(bottom)) {
                    (None, None) => ("", " "),
                    (Some(t), Some(_)) => (t, "█"),
                    (Some(t), None) => (t, "▀"),
                    (None, Some(b)) => (b, "▄"),
                };
                l.add(sgr, c);
            }
            self.done.push(l.take().trim_end().to_owned());
        }
        self.done.push(l.add(DIM, "  01100010 01101001 01110100").take());
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
                        KeyCode::Tab if self.busy.is_none() => self.next_thinking(cmds),
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
        let picked = q.options.get(q.at).filter(|_| pick).map(|o| o.to_string());
        self.question = None;
        match picked {
            // The `/thinking` picker is the UI's own, not the agent's; typing `/thinking
            // <level>` is its `something else`.
            Some(o) if &*id == THINKING => self.set_thinking(&o, cmds),
            None if &*id == THINKING => {}
            Some(o) if &*id == SESSIONS => {
                let prefix = o.split_whitespace().next().unwrap_or(&o);
                let _ = cmds.send(Command::Resume(prefix.into()));
            }
            None if &*id == SESSIONS => {}
            Some(text) => {
                let _ = cmds.send(Command::Answer { id, text });
            }
            None => self.answering = Some(id),
        }
    }

    fn submit(&mut self, cmds: &UnboundedSender<Command>) {
        let typed = self.input.text.trim().to_owned();
        if typed.is_empty() {
            return;
        }
        if let Some(id) = self.answering.take() {
            let text = if self.secret.take().is_some() {
                let key = self.input.text.clone();
                self.input.set("");
                key
            } else {
                self.input.take()
            };
            let _ = cmds.send(Command::Answer { id, text });
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
        if let Some(level) =
            typed.strip_prefix("/thinking").filter(|r| r.is_empty() || r.starts_with(' '))
        {
            self.input.take();
            return match level.trim() {
                "" => self.ask_thinking(),
                level => self.set_thinking(level, cmds),
            };
        }
        if let Some(rest) =
            typed.strip_prefix("/skills").filter(|r| r.is_empty() || r.starts_with(' '))
        {
            self.input.take();
            return match rest.trim() {
                "" => self.show_skills(),
                arg => match arg.split_once(' ') {
                    Some(("add", repo)) if !repo.trim().is_empty() => {
                        let _ = cmds.send(Command::SkillAdd { repo: repo.trim().into() });
                    }
                    _ => self
                        .event(Event::Notice("usage: /skills or /skills add <owner/repo>".into())),
                },
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
            "/sessions" => {
                self.input.take();
                self.list_sessions();
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
        if let Some(t) = self.thinking.clone().filter(|_| self.effort().is_none()) {
            let n = format!("{} has no `{t}` thinking; it uses its default", self.model);
            self.event(Event::Notice(n.into()));
        }
    }

    /// The levels the current model takes.
    fn levels(&self) -> &'static [&'static str] {
        agent::models::levels(&self.provider, &self.model)
    }

    /// Validates the picker and Tab entry points, with the same notice when unavailable.
    fn available_levels(&mut self) -> Option<&'static [&'static str]> {
        let levels = self.levels();
        if levels.is_empty() {
            self.event(Event::Notice(self.no_levels().into()));
            return None;
        }
        Some(levels)
    }

    /// The picked thinking level, if the current model takes it.
    fn effort(&self) -> Option<&str> {
        self.thinking.as_deref().filter(|t| self.levels().contains(t))
    }

    /// The `/thinking` picker: `default` and the model's levels, from least thinking.
    fn ask_thinking(&mut self) {
        let Some(levels) = self.available_levels() else { return };
        let default = self.default_label();
        let options: Vec<Box<str>> = [default.as_str()]
            .iter()
            .copied()
            .chain(levels.iter().copied())
            .map(Into::into)
            .collect();
        let at = options.iter().position(|o| Some(&**o) == self.effort()).unwrap_or(0);
        let text = format!("How hard should {} think?", self.model).into();
        self.question = Some(Question { id: THINKING.into(), text, options, at });
    }

    /// `/skills`: list installed skills for the human. The model already has them in its prompt.
    fn show_skills(&mut self) {
        let skills = agent::skills::list(&self.skills_dir);
        if skills.is_empty() {
            self.event(Event::Notice(
                "no skills installed. hint: bit skills add <owner/repo>".into(),
            ));
            return;
        }
        let lines: Vec<String> =
            skills.iter().map(|k| format!("  {} — {}", k.name, k.description)).collect();
        for line in lines {
            self.event(Event::Notice(line.into()));
        }
        let n = format!(
            "{} skill{}; the model reads a skill's SKILL.md only when a task matches",
            skills.len(),
            if skills.len() == 1 { "" } else { "s" }
        );
        self.event(Event::Notice(n.into()));
    }

    /// `/sessions`: list sessions and open a picker. The model is not involved.
    fn list_sessions(&mut self) {
        let Ok(entries) = agent::session::list(&self.sessions) else {
            self.event(Event::Notice("error: could not list sessions".into()));
            return;
        };
        if entries.is_empty() {
            self.event(Event::Notice("no sessions — start one with bit".into()));
            return;
        }
        let mut options = Vec::new();
        for e in entries {
            let id = &e.id[..e.id.len().min(8)];
            let preview = if e.preview.is_empty() { "(no preview)".into() } else { e.preview };
            let cwd =
                if e.cwd == self.cwd { "".to_string() } else { format!("  {}", e.cwd.display()) };
            let current = if e.id == *self.session { "  ← current" } else { "" };
            let line =
                format!("{}  {:<50} {}{}{}", id, clip(&preview, 50), ago(e.modified), cwd, current);
            options.push(line.into_boxed_str());
        }
        self.question = Some(Question {
            id: SESSIONS.into(),
            text: "Pick a session to resume".into(),
            options,
            at: 0,
        });
    }

    /// `Tab`: the model's next thinking level, wrapping around to `default`. Quiet, since
    /// the status line shows it and stepping through would fill the scrollback.
    fn next_thinking(&mut self, cmds: &UnboundedSender<Command>) {
        let Some(levels) = self.available_levels() else { return };
        let at = self.effort().and_then(|e| levels.iter().position(|l| *l == e));
        self.thinking = levels.get(at.map_or(0, |i| i + 1)).map(|&l| l.into());
        let _ = cmds.send(Command::SetThinking(self.thinking.clone()));
    }

    /// `default`, with the level it means when bit knows it: `default (medium)`.
    fn default_label(&self) -> String {
        match agent::models::default_level(&self.provider, &self.model) {
            Some(d) => format!("default ({d})"),
            None => "default".into(),
        }
    }

    fn no_levels(&self) -> String {
        match self.model.is_empty() {
            true => "error: no model is set. hint: pick one with /model first".into(),
            false => format!("error: bit knows no thinking levels for {}", self.model()),
        }
    }

    /// Sets the level, or `default` for the provider's own.
    fn set_thinking(&mut self, level: &str, cmds: &UnboundedSender<Command>) {
        // The picker's first row reads `default (high)` when the default is known.
        let level = if level.starts_with("default") { "default" } else { level };
        let levels = self.levels();
        if level != "default" && !levels.contains(&level) {
            let n = match levels {
                [] => self.no_levels(),
                l => format!("error: {} takes thinking {}, or default", self.model, l.join(", ")),
            };
            return self.event(Event::Notice(n.into()));
        }
        self.thinking = (level != "default").then(|| level.into());
        let _ = cmds.send(Command::SetThinking(self.thinking.clone()));
        let n = match &self.thinking {
            Some(l) => format!("thinking set to {l}"),
            None => match agent::models::default_level(&self.provider, &self.model) {
                Some(d) => format!("thinking set to the model's default ({d})"),
                None => "thinking set to the model's default".into(),
            },
        };
        self.event(Event::Notice(n.into()));
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
                    // A blank row between the reasoning and the answer.
                    if self.end_thought() {
                        self.done.push(String::new());
                    }
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
            Event::Thinking(t) => {
                // Text before it is finished; reasoning starts its own block.
                if !self.partial.is_empty() {
                    self.flush(true);
                }
                if self.thought.is_none() {
                    self.section(false);
                }
                let mut line = self.thought.take().unwrap_or_default();
                for c in t.chars() {
                    match c {
                        '\n' => {
                            let rows = self.thought_rows(&line);
                            self.done.extend(rows);
                            line.clear();
                        }
                        '\t' => line.push_str("    "),
                        c if c.is_control() => {}
                        c => line.push(c),
                    }
                }
                self.thought = Some(line);
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
                    l.add(AMBER, "!").add("", &format!(" {}", clean(&n)));
                } else {
                    l.add(DIM, &format!("  {}", clean(&n)));
                }
                self.done.push(l.take());
            }
            Event::Ask { id, question, options } => {
                self.end_text();
                self.question = Some(Question { id, text: question, options, at: 0 });
            }
            Event::Secret { id, prompt } => {
                (self.answering, self.secret) = (Some(id), Some(prompt));
            }
            Event::Approval { id, name, summary, detail } => {
                self.end_text();
                self.approval = Some(Approval { id, name, summary, detail, at: 0 });
            }
            Event::ToolStart { name, summary, diff, .. } => {
                self.end_text();
                self.section(true);
                self.tool = Some((name, summary, diff));
            }
            Event::ToolEnd { ok, preview, output, .. } => {
                if let Some((name, summary, diff)) = self.tool.take() {
                    self.tool_line(&name, &summary, ok, &preview);
                    if &*name == "bash" || matches!(&*name, "read" | "grep" | "glob") {
                        self.tail(&output);
                    } else if ok {
                        self.diff(&diff);
                    }
                }
            }
            Event::TurnEnd(r) => {
                self.end_text();
                self.tool = None;
                (self.approval, self.question, self.answering) = (None, None, None);
                // A cancelled key prompt must not leave the key in the box.
                if self.secret.take().is_some() {
                    self.input.set("");
                }
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
                    StopReason::Cancelled => l.add(AMBER, "!").add("", " cancelled"),
                    StopReason::Error(e) => l.add("31", "!").add("", &format!(" {}", clean(&e))),
                };
                self.done.push(l.take());
            }
        }
    }

    fn end_text(&mut self) {
        self.end_thought();
        if !self.partial.is_empty() {
            self.flush(true);
        }
    }

    /// Moves the last line of reasoning to scrollback; whether there was reasoning going.
    fn end_thought(&mut self) -> bool {
        let Some(line) = self.thought.take() else { return false };
        if !line.is_empty() {
            let rows = self.thought_rows(&line);
            self.done.extend(rows);
        }
        true
    }

    /// One line of reasoning, grey and indented, wrapped to the width; an empty line is one
    /// blank row.
    fn thought_rows(&self, line: &str) -> Vec<String> {
        let mut l = Line::new(self.color);
        let rows = wrap(line, self.width() - 4);
        rows.into_iter().map(|r| l.add(DIM, &format!("  {}", &line[r])).take()).collect()
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

    /// An edit's `-`/`+` lines under its tool line, cut like an approval's.
    fn diff(&mut self, diff: &str) {
        let lines: Vec<String> = diff.lines().map(clean).collect();
        let mut l = Line::new(self.color);
        for s in lines.iter().take(DETAIL_ROWS) {
            self.done.push(l.add(diff_sgr(s), &format!("  {}", clip(s, self.width() - 2))).take());
        }
        if lines.len() > DETAIL_ROWS {
            let more = format!("  ... {} more lines", lines.len() - DETAIL_ROWS);
            self.done.push(l.add(DIM, &more).take());
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
        if let Some(t) = self.thought.as_ref().filter(|t| !t.is_empty()) {
            f.extend(self.thought_rows(t));
        }
        let mut l = Line::new(self.color);
        if let Some((name, summary, _)) = &self.tool {
            let room = w.saturating_sub(str_width(name) + 14);
            l.add(AMBER, self.spinner())
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
        // A question or an approval is picked from, so it has no typing cursor.
        if let Some(cursor) = boxed {
            let typing = self.question.is_none() && self.approval.is_none();
            return (f, split, typing.then_some(cursor));
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
                // No animation over a key prompt: nothing is running while it's typed.
                .add(AMBER, &if self.secret.is_some() { String::new() } else { self.bits() })
                .add(
                    "",
                    &match (&self.answering, &self.secret) {
                        (Some(_), Some(p)) => p.to_string(),
                        (Some(_), None) => " type your answer, then Enter".into(),
                        (None, _) => format!(" working {:.1}s", e.as_secs_f32()),
                    },
                )
                .add(DIM, " ");
            let right = " esc cancels --+";
            l.fill(DIM, '-', w - right.len()).add(DIM, right);
        } else {
            if self.quit_armed {
                l.add(DIM, "-- ").add(AMBER, "ctrl-c again to quit").add(DIM, " ");
            }
            l.fill(DIM, '-', w - 1).add(DIM, "+");
        }
        f.push(l.take());

        let masked = self.secret.is_some().then(|| self.input.masked());
        let (rows, (row, col)) = masked.as_ref().unwrap_or(&self.input).layout(w - 4);
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
        f.push(self.status_border(AMBER));
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
        f.push(self.status_border(AMBER));
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
            f.push(self.row(diff_sgr(s), s));
        }
        if lines.len() > DETAIL_ROWS {
            f.push(self.row(DIM, &format!("... {} more lines", lines.len() - DETAIL_ROWS)));
        }
        f.push(self.row("", ""));
        let row = self.list(f, CHOICES.iter().map(|c| c.0), a.at);
        f.push(self.status_border(AMBER));
        (row, 2)
    }

    /// The yellow box's top border: `+-- ? <title> ---+`.
    fn title(&self, title: &str) -> String {
        let w = self.width();
        let mut l = Line::new(self.color);
        l.add(AMBER, "+-- ")
            .add(&format!("{AMBER};1"), "? ")
            .add("1", clip(title, w - 8))
            .add("", " ");
        l.fill(AMBER, '-', w - 1).add(AMBER, "+").take()
    }

    /// One row inside the yellow box.
    fn row(&self, sgr: &str, s: &str) -> String {
        let w = self.width();
        let mut l = Line::new(self.color);
        l.add(AMBER, "| ").add(sgr, clip(s, w - 4)).fill("", ' ', w - 2).add(AMBER, " |").take()
    }

    /// Rows of a list with `>` on the marked one; returns the frame row of the mark.
    /// If the list is taller than the remaining screen space, it scrolls to keep the
    /// mark in view.
    fn list<'s>(
        &self,
        f: &mut Vec<String>,
        items: impl Iterator<Item = &'s str>,
        at: usize,
    ) -> usize {
        let items: Vec<&str> = items.collect();
        let n = items.len();
        // Leave one row for the status border that follows the list.
        let max = self.rows.saturating_sub(f.len() + 1).max(1);
        let start = if n <= max {
            0
        } else {
            at.saturating_sub(max / 2).min(n.saturating_sub(max))
        };
        let mut mark = f.len();
        for (i, s) in items.iter().enumerate().skip(start).take(max) {
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
            false => {
                let default = agent::models::default_level(&self.provider, &self.model);
                // A model with levels always shows one, so it's clear there's one to change.
                let level = match (self.effort(), default) {
                    (Some(t), _) => format!(" · {t}"),
                    (None, Some(d)) => format!(" · {d} (default)"),
                    (None, None) if !self.levels().is_empty() => " · default".into(),
                    (None, None) => String::new(),
                };
                format!("{}/{}{level}", self.provider, self.model)
            }
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
    fn login_provider_selection_hides_cursor_until_typing_resumes() {
        let mut ui = Ui { cols: 80, ..Ui::default() };
        let (tx, mut rx) = unbounded_channel();
        assert!(ui.frame().2.is_some(), "the prompt has a typing cursor");
        ui.event(Event::Ask {
            id: "login".into(),
            question: "Sign in to which provider?".into(),
            options: vec!["openai".into()],
        });
        let (frame, _, cursor) = ui.frame();
        assert!(cursor.is_none(), "no typing cursor over the provider selection");
        assert!(frame.iter().any(|r| r.contains("> 1. openai")));

        ui.question_key(KeyCode::Down, &tx);
        let (frame, _, cursor) = ui.frame();
        assert!(cursor.is_none());
        assert!(frame.iter().any(|r| r.contains("> something else")));

        ui.question_key(KeyCode::Up, &tx);
        ui.question_key(KeyCode::Enter, &tx);
        assert!(matches!(rx.try_recv(), Ok(Command::Answer { id, text })
            if &*id == "login" && text == "openai"));
        assert!(ui.frame().2.is_some(), "restore the cursor after selection");
    }

    #[test]
    fn a_typed_login_answer_restores_the_cursor() {
        let mut ui = Ui { cols: 80, ..Ui::default() };
        let (tx, _) = unbounded_channel();
        ui.event(Event::Ask {
            id: "login".into(),
            question: "Sign in to which provider?".into(),
            options: vec!["openai".into()],
        });
        assert!(ui.frame().2.is_none());
        ui.question_key(KeyCode::Esc, &tx);
        assert_eq!(ui.answering.as_deref(), Some("login"));
        assert!(ui.frame().2.is_some(), "typed answers need a cursor");
    }

    #[test]
    fn thinking_takes_only_the_models_levels_and_shows_in_the_status() {
        let mut ui = Ui {
            cols: 80,
            provider: "opencode-go".into(),
            model: "glm-5.2".into(),
            ..Ui::default()
        };
        let (tx, mut rx) = unbounded_channel();
        let notice = |ui: &mut Ui| ui.done.drain(..).next_back().unwrap_or_default();

        ui.input.set("/thinking low");
        ui.submit(&tx);
        assert!(notice(&mut ui).contains("glm-5.2 takes thinking high, max, or default"));
        assert!(rx.try_recv().is_err());

        ui.input.set("/thinking max");
        ui.submit(&tx);
        assert!(matches!(rx.try_recv(), Ok(Command::SetThinking(Some(l))) if &*l == "max"));
        assert!(ui.status_border(DIM).contains("opencode-go/glm-5.2 · max"));

        // The picker marks the current level; `1` is `default`.
        ui.input.set("/thinking");
        ui.submit(&tx);
        let q = ui.question.as_ref().unwrap();
        assert_eq!((q.options.len(), q.at), (3, 2));
        ui.question_key(KeyCode::Char('1'), &tx);
        assert!(matches!(rx.try_recv(), Ok(Command::SetThinking(None))));
        assert!(ui.question.is_none() && ui.answering.is_none());
        assert!(ui.status_border(DIM).contains("opencode-go/glm-5.2 · default "));

        // Tab steps through the levels and back to default, without notices.
        let tab = |ui: &mut Ui| ui.key(ct::Event::Key(KeyCode::Tab.into()), &tx);
        ui.done.clear();
        for want in [Some("high"), Some("max"), None, Some("high")] {
            tab(&mut ui);
            assert!(matches!(rx.try_recv(), Ok(Command::SetThinking(l)) if l.as_deref() == want));
        }
        assert!(ui.done.is_empty());
        ui.input.set("/thi");
        tab(&mut ui);
        assert_eq!(ui.input.text, "/thinking ", "an open menu still completes");
        assert!(rx.try_recv().is_err());
        ui.input.set("");
        ui.busy = Some(Instant::now());
        tab(&mut ui);
        assert!(rx.try_recv().is_err(), "nothing changes mid-turn");
        ui.busy = None;

        // The default shows when bit knows it, and `off` is offered where the model has it.
        ui.thinking = None;
        ui.set_model("chatgpt".into(), "gpt-6-luna".into(), &tx);
        assert!(ui.status_border(DIM).contains("gpt-6-luna · medium (default)"));
        ui.input.set("/thinking");
        ui.submit(&tx);
        let q = ui.question.take().unwrap();
        assert_eq!(&*q.options[0], "default (medium)");
        assert_eq!(&*q.options[1], "off");
        ui.input.set("/thinking off");
        ui.submit(&tx);
        assert!(ui.status_border(DIM).contains("gpt-6-luna · off"));
        while rx.try_recv().is_ok() {}

        // A model without the level runs at its default, and the pick is kept for later.
        ui.thinking = Some("high".into());
        ui.set_model("opencode-go".into(), "kimi-k3".into(), &tx);
        assert!(notice(&mut ui).contains("kimi-k3 has no `high` thinking"));
        assert!(ui.status_border(DIM).contains("kimi-k3 · default "));
        ui.set_model("opencode-go".into(), "kimi-k2.7-code".into(), &tx);
        assert!(!ui.status_border(DIM).contains('·'), "no levels, nothing to show");
        ui.set_model("opencode-go".into(), "glm-5.3".into(), &tx);
        assert!(ui.status_border(DIM).contains("glm-5.3 · high"));

        // Picker and Tab reject unavailable levels identically, without sending commands.
        while rx.try_recv().is_ok() {}
        for model in ["", "unknown"] {
            ui.model = model.into();
            ui.done.clear();
            ui.ask_thinking();
            let notice = std::mem::take(&mut ui.done);
            ui.next_thinking(&tx);
            assert_eq!(ui.done, notice);
            let expected = if model.is_empty() { "no model is set" } else { "no thinking levels" };
            assert!(notice[0].contains(expected));
            assert!(ui.question.is_none() && rx.try_recv().is_err());
            assert_eq!(ui.thinking.as_deref(), Some("high"));
        }
    }

    #[test]
    fn thinking_is_grey_and_set_apart_from_the_answer() {
        let mut ui = Ui { cols: 40, color: true, ..Ui::default() };
        ui.event(Event::Thinking("Need the ".into()));
        ui.event(Event::Thinking("file.\nThen".into()));
        assert_eq!(ui.done, ["\x1b[2m  Need the file.\x1b[0m"]);
        let (frame, ..) = ui.frame();
        assert!(frame[0].contains("  Then"), "the unfinished line is live");
        ui.event(Event::Text("Done.\n".into()));
        assert_eq!(ui.done[1..3], ["\x1b[2m  Then\x1b[0m".to_string(), String::new()]);
        assert!(ui.done[3].contains("Done.") && !ui.done[3].contains("\x1b[2m"));
        assert!(ui.thought.is_none());
    }

    #[test]
    fn a_pasted_key_is_hidden_and_never_kept() {
        let mut ui = Ui { cols: 80, busy: Some(Instant::now()), ..Ui::default() };
        let (tx, mut rx) = unbounded_channel();
        ui.event(Event::Secret { id: "key".into(), prompt: "paste your key".into() });
        ui.input.set("sk-secret");
        let (frame, ..) = ui.frame();
        assert!(!frame.iter().any(|r| r.contains("sk-secret")), "the key is never drawn");
        assert!(frame.iter().any(|r| r.contains("•••••••••")));
        assert!(frame.iter().any(|r| r.contains("-- paste your key")), "the prompt is on the box");
        assert!(
            !frame.iter().any(|r| r.contains(&ui.bits())),
            "no bit animation over a key prompt"
        );
        ui.submit(&tx);
        assert!(matches!(rx.try_recv(), Ok(Command::Answer { id, text })
            if &*id == "key" && text == "sk-secret"));
        ui.input.recall(true);
        assert!(ui.input.text.is_empty(), "Up doesn't bring the key back");

        // Cancelled mid-paste, the half-typed key goes too.
        ui.event(Event::Secret { id: "key".into(), prompt: "paste your key".into() });
        ui.input.set("sk-half");
        ui.event(Event::TurnEnd(StopReason::Done));
        assert!(ui.input.text.is_empty() && ui.secret.is_none());
    }

    /// Scrollback after an `edit` with this diff ends `ok` or not.
    fn edit_rows(diff: &str, ok: bool) -> Vec<String> {
        let mut ui = Ui { cols: 80, ..Ui::default() };
        let (id, name) = ("e1".into(), "edit".into());
        let summary = "a.rs".into();
        ui.event(Event::ToolStart { id, name, summary, diff: diff.into() });
        let preview = if ok { "ok" } else { "error: no match" }.into();
        ui.event(Event::ToolEnd { id: "e1".into(), ok, preview, output: "".into() });
        ui.done
    }

    #[test]
    fn a_finished_edit_shows_its_diff_and_a_failed_one_does_not() {
        let rows = edit_rows("- old\n+ new\n", true);
        assert_eq!(rows, ["* edit a.rs  [ok]", "  - old", "  + new"]);
        assert_eq!(edit_rows("- old\n+ new\n", false).len(), 1);
        let long: String = (0..20).map(|i| format!("+ {i}\n")).collect();
        let rows = edit_rows(&long, true);
        assert_eq!((rows.len(), rows.last().unwrap().as_str()), (14, "  ... 8 more lines"));
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

    #[test]
    fn long_lists_scroll_to_keep_mark_visible() {
        let ui = Ui { cols: 80, rows: 8, ..Ui::default() };
        let mut f = vec!["title".into(), "prompt".into(), "".into()];
        let items: Vec<String> = (0..20).map(|i| format!("option {i}")).collect();
        let mark = ui.list(&mut f, items.iter().map(String::as_str), 15);
        // title + prompt + blank + 4 list rows (rows 8 - 3 already used - 1 border).
        assert_eq!(f.len(), 7);
        // The mark should be on option 15, centered in the visible window (items 13-16).
        assert!(f[mark].contains("> option 15"));
        // Earlier options are not rendered.
        assert!(!f.iter().any(|r| r.contains("option 0")));
        assert!(!f.iter().any(|r| r.contains("option 12")));
        // Later options are not rendered.
        assert!(!f.iter().any(|r| r.contains("option 17")));
    }
}
