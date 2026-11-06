//! Lua plugins (minimal-plugins.md): `before_<tool>` and `after_<tool>` hooks and
//! `tool_<name>` tools in `~/.config/bit/plugins/*.lua`. A hook's parameter names say what
//! it gets; its return value says what happens.

use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, VmState};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

/// A `before_` hook holds up a tool call; an `after_` hook may run a formatter or tests.
const BEFORE: Duration = Duration::from_secs(2);
const AFTER: Duration = Duration::from_secs(60);
const MEMORY: usize = 4 << 20;
const MAX_OUTPUT: usize = 1 << 20;
const MAX_JSON_DEPTH: usize = 64;
/// What every hook may ask for besides its tool's own fields.
const COMMON: [&str; 3] = ["cwd", "mode", "headless"];
/// What `---@param` may say.
const TYPES: [&str; 6] = ["string", "number", "integer", "boolean", "array", "object"];

/// Every plugin file, and which of their hooks run for which tool.
#[derive(Default)]
pub struct Plugins {
    files: Vec<(String, Lua)>,
    /// `before_bash` → its hooks, in load order.
    routes: HashMap<String, Vec<Hook>>,
    /// Each tool's fields: the built-ins' from `tools::SCHEMAS`, then the plugins' own.
    fields: HashMap<String, Vec<String>>,
    /// The `tool_` schemas, comma-separated, for the request prefix.
    schemas: String,
}

struct Hook {
    file: usize,
    f: Function,
    params: Vec<String>,
}

/// A file's hooks, or why it doesn't load.
pub type Loaded = Result<Vec<String>, String>;

/// What the `before_` hooks decided. A rewrite edits the arguments in place.
#[derive(Debug, PartialEq)]
pub enum Before {
    Pass,
    Allow,
    /// The tool result the model gets instead.
    Block(String),
}

/// The fields every hook can ask for.
pub struct Fields {
    pub cwd: PathBuf,
    /// `ask` or `auto`.
    pub mode: &'static str,
    /// No user is present (`bit -p`), so `ask` returns "".
    pub headless: bool,
}

/// What a hook needs from the agent, which owns the screen.
pub enum Req {
    Print(String),
    Ask { question: String, options: Vec<String>, reply: oneshot::Sender<String> },
}

/// The hook running now, for the API functions and the deadline check.
struct Call {
    file: String,
    to: Option<UnboundedSender<Req>>,
    cwd: PathBuf,
    headless: bool,
    deadline: Mutex<Instant>,
}

impl Plugins {
    /// Loads every `*.lua` in `dir`, alphabetically. A missing folder means no plugins; any
    /// broken file is an error, so a guard never silently goes missing.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let (p, files) = Self::read(dir)?;
        match files.into_iter().find_map(|(n, r)| r.err().map(|e| format!("✗ {n}: {e}"))) {
            Some(e) => Err(e),
            None => Ok(p),
        }
    }

    /// Each file with its hooks, or why it fails to load: for `bit plugins`.
    pub fn check(dir: &Path) -> Result<Vec<(String, Loaded)>, String> {
        Ok(Self::read(dir)?.1)
    }

    /// Loads every file it can, then checks each hook against every tool, the plugins' own
    /// included, so a guard may live in another file than its tool.
    fn read(dir: &Path) -> Result<(Self, Vec<(String, Loaded)>), String> {
        let mut p = Self { fields: tool_fields(), ..Self::default() };
        let mut out: Vec<_> = files(dir)?
            .into_iter()
            .map(|(name, path)| (name.clone(), p.add(&name, &path).map(|()| Vec::new())))
            .collect();
        for (key, hooks) in &p.routes {
            for h in hooks {
                if let Err(e) = p.check_hook(key, &h.params)
                    && let Some((_, r @ Ok(_))) = out.iter_mut().find(|f| f.0 == p.files[h.file].0)
                {
                    *r = Err(e);
                }
            }
        }
        for (name, r) in &mut out {
            if let Ok(hooks) = r {
                *hooks = p.hooks_of(name);
            }
        }
        Ok((p, out))
    }

    fn hooks_of(&self, name: &str) -> Vec<String> {
        let mine = |hooks: &Vec<Hook>| hooks.iter().any(|h| self.files[h.file].0 == name);
        let mut keys: Vec<String> =
            self.routes.iter().filter(|(_, h)| mine(h)).map(|(k, _)| k.clone()).collect();
        keys.sort();
        keys
    }

    pub fn has(&self, hook: &str, tool: &str) -> bool {
        self.routes.contains_key(&format!("{hook}{tool}"))
    }

    /// The `tool_` schemas, comma-separated; empty if there are none.
    pub fn schemas(&self) -> &str {
        &self.schemas
    }

    /// The names of the plugins' own tools.
    pub fn tools(&self) -> Vec<String> {
        let names = self.routes.keys().filter_map(|k| k.strip_prefix("tool_"));
        names.map(str::to_owned).collect()
    }

    /// Runs one file and takes its hooks and tools, or none of them if anything is wrong.
    fn add(&mut self, name: &str, path: &Path) -> Result<(), String> {
        let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (lua, getlocal) = open(name, &src).map_err(|e| first_line(&e))?;
        let file = self.files.len();
        let mut found = Vec::new();
        for pair in lua.globals().pairs::<String, mlua::Value>() {
            let (key, v) = pair.map_err(|e| first_line(&e))?;
            if let mlua::Value::Function(f) = v
                && ["before_", "after_", "tool_"].iter().any(|p| key.starts_with(p))
            {
                found.push((key, f));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let (mut hooks, mut tools) = (Vec::new(), Vec::new());
        for (key, f) in found {
            let params = (1..=f.info().num_params)
                .map(|i| getlocal.call::<String>((f.clone(), i)))
                .collect::<mlua::Result<Vec<_>>>()
                .map_err(|e| first_line(&e))?;
            if let Some(tool) = key.strip_prefix("tool_") {
                if self.fields.contains_key(tool) {
                    return Err(format!("{key}: there's already a tool named `{tool}`"));
                }
                let line = f.info().line_defined.unwrap_or(0);
                let schema = schema(tool, &params, &src, line)?;
                let own = params.iter().filter(|p| !COMMON.contains(&p.as_str())).cloned();
                tools.push((tool.to_owned(), own.collect(), schema));
            }
            hooks.push((key, Hook { file, f, params }));
        }
        for (tool, fields, schema) in tools {
            self.fields.insert(tool, fields);
            if !self.schemas.is_empty() {
                self.schemas.push(',');
            }
            self.schemas += &schema;
        }
        for (key, h) in hooks {
            self.routes.entry(key).or_default().push(h);
        }
        self.files.push((name.into(), lua));
        Ok(())
    }

    fn check_hook(&self, key: &str, params: &[String]) -> Result<(), String> {
        let (after, tool) = match (key.strip_prefix("before_"), key.strip_prefix("after_")) {
            (Some(t), _) => (false, t),
            (_, Some(t)) => (true, t),
            _ => return Ok(()), // `tool_`, checked as it loaded
        };
        let Some(fields) = self.fields.get(tool) else {
            let mut tools: Vec<&str> = self.fields.keys().map(String::as_str).collect();
            tools.sort();
            return Err(format!("{key}: no tool `{tool}` (available: {})", tools.join(", ")));
        };
        let mut ok: Vec<&str> = fields.iter().map(String::as_str).chain(COMMON).collect();
        if after {
            ok.extend(["output", "ok"]);
        }
        match params.iter().find(|p| !ok.contains(&p.as_str())) {
            Some(p) => Err(format!("{key}: no field `{p}` (available: {})", ok.join(", "))),
            None => Ok(()),
        }
    }

    /// Runs the plugin tool `tool`. Whatever it returns is the result; an error is a failed
    /// result the model can read.
    pub fn run_tool(
        &self,
        tool: &str,
        args: &str,
        fx: &Fields,
        to: &UnboundedSender<Req>,
    ) -> tools::Output {
        let Some(h) = self.routes.get(&format!("tool_{tool}")).and_then(|h| h.first()) else {
            return tools::Output { ok: false, content: format!("error: no tool `{tool}`") };
        };
        let v: Value = serde_json::from_str(args).unwrap_or_default();
        let r = self.call(h, &v, None, fx, to, AFTER).and_then(|r| match r {
            mlua::Value::Nil => Ok(String::new()),
            mlua::Value::String(s) => Ok(s.to_string_lossy()),
            t @ mlua::Value::Table(_) => to_json(t).map(|j| j.to_string()),
            other => other.to_string().map_err(|e| first_line(&e)),
        });
        match r {
            Ok(content) => tools::Output { ok: true, content },
            Err(e) => tools::Output { ok: false, content: format!("error: {e}") },
        }
    }

    /// Runs the `before_<tool>` hooks in order. A block wins over an allow, which wins over
    /// no opinion; an error or a timeout blocks. Each allow and rewrite is noted in `acted`,
    /// for the tool line.
    pub fn before(
        &self,
        tool: &str,
        args: &mut String,
        fx: &Fields,
        to: &UnboundedSender<Req>,
        acted: &mut Vec<String>,
    ) -> Before {
        let Some(hooks) = self.routes.get(&format!("before_{tool}")) else { return Before::Pass };
        let mut v: Value = serde_json::from_str(args).unwrap_or_default();
        let (mut verdict, mut changed) = (Before::Pass, false);
        for h in hooks {
            let file = &self.files[h.file].0;
            let fail = |e: &str| {
                Before::Block(format!(
                    "error: blocked by {file}, which failed: {e}. hint: tell the user; the plugin needs fixing"
                ))
            };
            match self.call(h, &v, None, fx, to, BEFORE) {
                Err(e) => return fail(&e),
                Ok(mlua::Value::Nil | mlua::Value::Boolean(false)) => {}
                Ok(mlua::Value::Boolean(true)) => {
                    verdict = Before::Allow;
                    acted.push(format!("allowed by {file}"));
                }
                Ok(mlua::Value::String(s)) => {
                    let why = s.to_string_lossy();
                    return Before::Block(format!("error: blocked by {file}. hint: {why}"));
                }
                Ok(mlua::Value::Table(t)) => match self.rewrite(tool, &mut v, t) {
                    Ok(()) => {
                        changed = true;
                        acted.push(format!("rewritten by {file}"));
                    }
                    Err(e) => return fail(&e),
                },
                Ok(r) => return fail(&format!("before_{tool} returned a {}", r.type_name())),
            }
        }
        if changed {
            *args = v.to_string();
        }
        verdict
    }

    fn rewrite(&self, tool: &str, args: &mut Value, t: Table) -> Result<(), String> {
        if !args.is_object() {
            *args = Value::Object(Default::default());
        }
        for pair in t.pairs::<String, mlua::Value>() {
            let (k, v) = pair.map_err(|e| first_line(&e))?;
            if !self.fields[tool].contains(&k) {
                return Err(format!("it rewrote `{k}`, which isn't a field of {tool}"));
            }
            args[k] = to_json(v)?;
        }
        Ok(())
    }

    /// Runs the `after_<tool>` hooks in order. A string is appended to the output; a table's
    /// `output` replaces it, which is noted in `acted`. A failing hook leaves the output alone
    /// and says so on screen.
    pub fn after(
        &self,
        tool: &str,
        args: &str,
        out: &mut tools::Output,
        fx: &Fields,
        to: &UnboundedSender<Req>,
        acted: &mut Vec<String>,
    ) {
        let Some(hooks) = self.routes.get(&format!("after_{tool}")) else { return };
        let v: Value = serde_json::from_str(args).unwrap_or_default();
        for h in hooks {
            let file = &self.files[h.file].0;
            match self.call(h, &v, Some(out), fx, to, AFTER) {
                Ok(mlua::Value::String(s)) => {
                    out.content += &format!("\n[{file}] {}", s.to_string_lossy());
                }
                Ok(mlua::Value::Table(t)) => {
                    if let Ok(Some(s)) = t.get::<Option<String>>("output") {
                        out.content = s;
                        acted.push(format!("output replaced by {file}"));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = to.send(Req::Print(format!("{file}: after_{tool} failed: {e}")));
                }
            }
        }
    }

    /// Calls `h` with the values its parameter names ask for, under a deadline.
    fn call(
        &self,
        h: &Hook,
        args: &Value,
        out: Option<&tools::Output>,
        fx: &Fields,
        to: &UnboundedSender<Req>,
        budget: Duration,
    ) -> Result<mlua::Value, String> {
        let (file, lua) = &self.files[h.file];
        lua.set_app_data(Call {
            file: file.clone(),
            to: Some(to.clone()),
            cwd: fx.cwd.clone(),
            headless: fx.headless,
            deadline: Mutex::new(Instant::now() + budget),
        });
        let arg = |p: &str| -> mlua::Result<mlua::Value> {
            Ok(match (p, out) {
                ("cwd", _) => {
                    mlua::Value::String(lua.create_string(fx.cwd.to_string_lossy().as_ref())?)
                }
                ("mode", _) => mlua::Value::String(lua.create_string(fx.mode)?),
                ("headless", _) => mlua::Value::Boolean(fx.headless),
                ("output", Some(o)) => mlua::Value::String(lua.create_string(&o.content)?),
                ("ok", Some(o)) => mlua::Value::Boolean(o.ok),
                _ => to_lua(lua, &args[p])?,
            })
        };
        let argv = h.params.iter().map(|p| arg(p)).collect::<mlua::Result<mlua::MultiValue>>();
        argv.and_then(|a| h.f.call(a)).map_err(|e| first_line(&e))
    }
}

/// The schema of `tool_<name>`, from the `---` lines right above it: plain ones describe
/// the tool, `---@param name type [description]` types a parameter (`string` if not
/// given), and a `?` after the type makes it optional.
fn schema(name: &str, params: &[String], src: &str, line: usize) -> Result<String, String> {
    let above: Vec<&str> = src.lines().take(line.saturating_sub(1)).map(str::trim).collect();
    let doc = above
        .into_iter()
        .rev()
        .take_while(|l| l.starts_with("---") && !l.trim_matches('-').is_empty());
    let mut doc: Vec<&str> = doc.map(|l| l[3..].trim()).collect();
    doc.reverse();
    let (mut desc, mut typed) = (Vec::new(), HashMap::new());
    for l in doc {
        let Some(rest) = l.strip_prefix("@param") else {
            desc.push(l);
            continue;
        };
        let mut w = rest.split_whitespace();
        let (Some(p), Some(t)) = (w.next(), w.next()) else {
            return Err(format!("tool_{name}: write ---@param <name> <type> [description]"));
        };
        typed.insert(p, (t, w.collect::<Vec<_>>().join(" ")));
    }
    if desc.is_empty() {
        return Err(format!("tool_{name}: describe it in a --- comment right above it"));
    }
    let (mut props, mut required) = (serde_json::Map::new(), Vec::new());
    for p in params.iter().filter(|p| !COMMON.contains(&p.as_str())) {
        let (t, text) = typed.remove(p.as_str()).unwrap_or(("string", String::new()));
        let (t, optional) = t.strip_suffix('?').map_or((t, false), |t| (t, true));
        if !TYPES.contains(&t) {
            let ok = TYPES.join(", ");
            return Err(format!("tool_{name}: {p} has type `{t}` (available: {ok})"));
        }
        let mut s = json!({ "type": t });
        if !text.is_empty() {
            s["description"] = text.into();
        }
        props.insert(p.clone(), s);
        if !optional {
            required.push(p.as_str());
        }
    }
    if let Some(p) = typed.keys().next() {
        return Err(format!("tool_{name}: @param {p} isn't one of its parameters"));
    }
    let parameters = json!({ "type": "object", "properties": props, "required": required });
    let f = json!({ "name": name, "description": desc.join(" "), "parameters": parameters });
    Ok(json!({ "type": "function", "function": f }).to_string())
}

fn files(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("error: {}: {e}", dir.display())),
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "lua"))
        .map(|p| (p.file_name().unwrap_or_default().to_string_lossy().into_owned(), p))
        .collect();
    out.sort();
    Ok(out)
}

/// `bash` → `["command", "timeout"]`, for every built-in tool.
fn tool_fields() -> HashMap<String, Vec<String>> {
    let schemas: Value = serde_json::from_str(tools::SCHEMAS).unwrap_or_default();
    let tools = schemas.as_array().into_iter().flatten().map(|s| &s["function"]);
    let fields = |f: &Value| {
        let props = f["parameters"]["properties"].as_object();
        props.into_iter().flat_map(|p| p.keys().cloned()).collect()
    };
    tools.map(|f| (f["name"].as_str().unwrap_or("").to_owned(), fields(f))).collect()
}

/// A Lua state with only `string`, `table`, `math`, `utf8` and the API, running `src`.
/// Returns it with `debug.getlocal`, kept back to read parameter names.
fn open(name: &str, src: &str) -> mlua::Result<(Lua, Function)> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::DEBUG;
    // SAFETY: `debug` is removed from the globals before any plugin code runs.
    let lua = unsafe { Lua::unsafe_new_with(libs, LuaOptions::new()) };
    lua.set_memory_limit(MEMORY)?;
    let g = lua.globals();
    let getlocal = g.get::<Table>("debug")?.get::<Function>("getlocal")?;
    // `load` takes bytecode, which can crash the VM; the rest reach the filesystem.
    for k in ["debug", "load", "loadfile", "dofile"] {
        g.raw_set(k, mlua::Nil)?;
    }
    api(&lua)?;
    lua.set_hook(HookTriggers::new().every_nth_instruction(1000), |lua, _| {
        match lua.app_data_ref::<Call>() {
            Some(c) if Instant::now() > *c.deadline.lock().unwrap() => {
                Err(mlua::Error::runtime("timed out"))
            }
            _ => Ok(VmState::Continue),
        }
    })?;
    // The top level gets a `before_` budget too, so a stray loop can't hang startup.
    lua.set_app_data(Call {
        file: name.into(),
        to: None,
        cwd: std::env::current_dir().unwrap_or_default(),
        headless: true,
        deadline: Mutex::new(Instant::now() + BEFORE),
    });
    lua.load(src).set_name(format!("@{name}")).set_mode(ChunkMode::Text).exec()?;
    Ok((lua, getlocal))
}

/// `run`, `read`, `ask` and `print`.
fn api(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    let run = lua.create_function(|lua, (cmd, opts): (mlua::Value, Option<Table>)| {
        let c = lua.app_data_ref::<Call>().ok_or_else(|| mlua::Error::runtime("no call"))?;
        let mut p = match &cmd {
            mlua::Value::String(s) => {
                let mut p = Command::new("sh");
                p.arg("-c").arg(s.to_str()?.as_ref());
                p
            }
            mlua::Value::Table(t) => {
                let argv: Vec<String> = t.sequence_values().collect::<mlua::Result<_>>()?;
                let (prog, rest) =
                    argv.split_first().ok_or_else(|| mlua::Error::runtime("run{}: empty"))?;
                let mut p = Command::new(prog);
                p.args(rest);
                p
            }
            _ => return Err(mlua::Error::runtime("run: pass a string or a table")),
        };
        let deadline = *c.deadline.lock().unwrap();
        let limit = opts.map(|o| o.get::<Option<f64>>("timeout")).transpose()?.flatten();
        let deadline = match limit {
            Some(s) if s.is_finite() && s >= 0.0 => {
                // Clamp before conversion so even enormous finite values cannot overflow.
                deadline.min(Instant::now() + Duration::from_secs_f64(s.min(AFTER.as_secs_f64())))
            }
            Some(_) => {
                return Err(mlua::Error::runtime("run: timeout must be finite and non-negative"));
            }
            None => deadline,
        };
        let cwd = c.cwd.clone();
        drop(c);
        let (out, code) = run(p.current_dir(cwd), deadline).map_err(mlua::Error::runtime)?;
        Ok((out, code))
    })?;
    let read = lua.create_function(|lua, path: String| {
        let cwd = lua.app_data_ref::<Call>().map(|c| c.cwd.clone()).unwrap_or_default();
        Ok(match std::fs::read_to_string(cwd.join(&path)) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("{path}: {e}"))),
        })
    })?;
    let ask = lua.create_function(|lua, (question, options): (String, Option<Vec<String>>)| {
        let c = lua.app_data_ref::<Call>().ok_or_else(|| mlua::Error::runtime("no call"))?;
        let Some(to) = c.to.clone().filter(|_| !c.headless) else { return Ok(String::new()) };
        let (reply, answer) = oneshot::channel();
        let started = Instant::now();
        let _ = to.send(Req::Ask { question, options: options.unwrap_or_default(), reply });
        let text = answer.blocking_recv().unwrap_or_default();
        // The user's thinking time doesn't count against the hook.
        *c.deadline.lock().unwrap() += started.elapsed();
        Ok(text)
    })?;
    let print = lua.create_function(|lua, args: mlua::Variadic<mlua::Value>| {
        let c = lua.app_data_ref::<Call>().ok_or_else(|| mlua::Error::runtime("no call"))?;
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect::<mlua::Result<_>>()?;
        let text: String = parts
            .join("\t")
            .chars()
            .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
            .collect();
        let text: Vec<&str> = text.lines().take(20).collect();
        let line = format!("[{}] {}", c.file, text.join("\n"));
        match &c.to {
            Some(to) => drop(to.send(Req::Print(line))),
            None => eprintln!("{line}"),
        }
        Ok(())
    })?;
    g.raw_set("run", run)?;
    g.raw_set("read", read)?;
    g.raw_set("ask", ask)?;
    g.raw_set("print", print)
}

/// Runs a process group under a deadline, with bounded stdout followed by stderr.
fn run(p: &mut Command, deadline: Instant) -> Result<(String, i32), String> {
    let mut child = p
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("run: {e}"))?;
    let group = child.id() as i32;
    let result = (|| {
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done) = (false, false);
        let mut status = None;
        loop {
            if Instant::now() >= deadline {
                return Err("run: timed out".into());
            }
            if status.is_none() {
                status = child.try_wait().map_err(|e| format!("run: {e}"))?;
                if status.is_some() {
                    // Background descendants must not outlive a completed command.
                    tools::kill_process_group(group);
                }
            }
            let mut buf = [0; 8192];
            for (stream, bytes, done) in [
                (&mut stdout as &mut dyn Read, &mut out, &mut out_done),
                (&mut stderr as &mut dyn Read, &mut err, &mut err_done),
            ] {
                if *done {
                    continue;
                }
                match stream.read(&mut buf) {
                    Ok(0) => *done = true,
                    Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(format!("run: {e}")),
                }
            }
            if out.len() + err.len() > MAX_OUTPUT {
                return Err(format!("run: output exceeds {MAX_OUTPUT} bytes"));
            }
            if let Some(status) = status.filter(|_| out_done && err_done) {
                let text =
                    String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err);
                return Ok((text, status.code().unwrap_or(-1)));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    })();
    tools::kill_process_group(group);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn nonblocking(stream: &impl AsRawFd) -> Result<(), String> {
    // SAFETY: these are live pipe descriptors owned by run; fcntl does not retain them.
    let result = unsafe {
        let flags = libc::fcntl(stream.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            flags
        } else {
            libc::fcntl(stream.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
        }
    };
    if result < 0 { Err(format!("run: {}", std::io::Error::last_os_error())) } else { Ok(()) }
}

fn to_lua(lua: &Lua, v: &Value) -> mlua::Result<mlua::Value> {
    Ok(match v {
        Value::Null => mlua::Nil,
        Value::Bool(b) => mlua::Value::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => mlua::Value::Integer(i),
            None => mlua::Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => mlua::Value::String(lua.create_string(s)?),
        Value::Array(a) => {
            let items = a.iter().map(|x| to_lua(lua, x)).collect::<mlua::Result<Vec<_>>>()?;
            mlua::Value::Table(lua.create_sequence_from(items)?)
        }
        Value::Object(o) => {
            let t = lua.create_table()?;
            for (k, x) in o {
                t.raw_set(k.as_str(), to_lua(lua, x)?)?;
            }
            mlua::Value::Table(t)
        }
    })
}

fn to_json(v: mlua::Value) -> Result<Value, String> {
    let mut remaining = MAX_OUTPUT;
    json_value(v, 0, &mut remaining)
}

fn json_value(v: mlua::Value, depth: usize, remaining: &mut usize) -> Result<Value, String> {
    if depth > MAX_JSON_DEPTH {
        return Err("table is cyclic or nested too deeply (limit: 64)".into());
    }
    // Charge each node as well as strings, bounding expansion of shared tables.
    let size = 64
        + match &v {
            mlua::Value::String(s) => s.as_bytes().len(),
            _ => 0,
        };
    *remaining = remaining.checked_sub(size).ok_or("table exceeds JSON conversion size limit")?;
    Ok(match v {
        mlua::Value::Nil => Value::Null,
        mlua::Value::Boolean(b) => b.into(),
        mlua::Value::Integer(i) => i.into(),
        mlua::Value::Number(n) => {
            serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
        }
        mlua::Value::String(s) => s.to_string_lossy().into(),
        mlua::Value::Table(t) if t.raw_len() > 0 => {
            let items = t.sequence_values::<mlua::Value>().map(|x| x.map_err(|e| first_line(&e)));
            Value::Array(
                items
                    .map(|x| x.and_then(|v| json_value(v, depth + 1, remaining)))
                    .collect::<Result<_, _>>()?,
            )
        }
        mlua::Value::Table(t) => {
            let mut m = serde_json::Map::new();
            for pair in t.pairs::<String, mlua::Value>() {
                let (k, x) = pair.map_err(|e| first_line(&e))?;
                *remaining = remaining
                    .checked_sub(k.len())
                    .ok_or("table exceeds JSON conversion size limit")?;
                m.insert(k, json_value(x, depth + 1, remaining)?);
            }
            Value::Object(m)
        }
        other => return Err(format!("a {} can't be a tool argument", other.type_name())),
    })
}

/// mlua's errors carry a traceback; the first line says what went wrong.
fn first_line(e: &mlua::Error) -> String {
    let s = e.to_string();
    s.lines().next().unwrap_or("").trim_start_matches("runtime error: ").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    /// Writes `files` into a fresh directory and loads it.
    fn load(name: &str, files: &[(&str, &str)]) -> Result<Plugins, String> {
        let dir = std::env::temp_dir().join(format!("bit-plugins-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, src) in files {
            std::fs::write(dir.join(f), src).unwrap();
        }
        Plugins::load(&dir)
    }

    fn fields(mode: &'static str, headless: bool) -> Fields {
        Fields { cwd: std::env::temp_dir(), mode, headless }
    }

    fn before(p: &Plugins, tool: &str, args: &str) -> (Before, String, UnboundedReceiver<Req>) {
        let (to, rx) = unbounded_channel();
        let mut a = args.to_owned();
        (p.before(tool, &mut a, &fields("auto", true), &to, &mut Vec::new()), a, rx)
    }

    #[test]
    fn run_rejects_invalid_timeouts_without_panicking() {
        let src = "--- Run with a timeout.\nfunction tool_timeout(value) return run('printf ok', { timeout = value }) end";
        let p = load("invalid-timeouts", &[("a.lua", src)]).unwrap();
        let (to, _rx) = unbounded_channel();
        for value in ["-1", "0/0", "math.huge", "-math.huge", "'invalid'"] {
            // JSON cannot represent NaN or infinity, so construct these in Lua.
            let lua = &p.files[0].1;
            lua.load(format!(
                "function tool_timeout() return run('printf ok', {{ timeout = {value} }}) end"
            ))
            .exec()
            .unwrap();
            let f = lua.globals().get::<Function>("tool_timeout").unwrap();
            let err = f.call::<mlua::Value>(()).unwrap_err().to_string();
            assert!(err.contains("timeout") || err.contains("number"), "{err}");
        }
        // The registered function is unchanged and ordinary, zero and huge values do not panic.
        for (args, ok) in
            [(r#"{"value":0.5}"#, true), (r#"{"value":1e300}"#, true), (r#"{"value":0}"#, false)]
        {
            let out = p.run_tool("timeout", args, &fields("auto", true), &to);
            assert_eq!(out.ok, ok, "{}", out.content);
        }
    }

    #[test]
    fn json_conversion_rejects_cycles_depth_and_exponential_expansion() {
        let lua = Lua::new();
        for src in [
            "local t = {}; t.self = t; return t",
            "local a,b = {},{}; a[1]=b; b[1]=a; return a",
            "local t = {}; for i=1,100 do t={t} end; return t",
            "local t = {1}; for i=1,30 do t={t,t} end; return t",
        ] {
            assert!(to_json(lua.load(src).eval().unwrap()).is_err(), "{src}");
        }
        // Sharing a child without a cycle is valid and is copied into JSON.
        let v = lua.load("local child = {x=1}; return {a=child,b=child}").eval().unwrap();
        assert_eq!(to_json(v).unwrap(), json!({"a":{"x":1},"b":{"x":1}}));
        let p = load(
            "cycle-rewrite",
            &[("a.lua", "function before_bash() local t={}; t.self=t; return {command=t} end")],
        )
        .unwrap();
        assert!(matches!(before(&p,"bash","{}").0, Before::Block(s) if s.contains("cyclic")));
    }

    fn shell(command: &str, budget: Duration) -> Result<(String, i32), String> {
        run(Command::new("sh").arg("-c").arg(command), Instant::now() + budget)
    }

    fn assert_dead(pid: i32) {
        for _ in 0..100 {
            // SAFETY: signal zero only checks whether the given process exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("descendant {pid} survived");
    }

    #[test]
    fn run_kills_background_descendants_when_the_parent_exits() {
        let started = Instant::now();
        let (out, code) = shell("sleep 30 & echo $!", Duration::from_secs(2)).unwrap();
        assert_eq!(code, 0);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_dead(out.trim().parse().unwrap());
    }

    #[test]
    fn run_timeout_kills_the_process_group() {
        let path =
            std::env::temp_dir().join(format!("bit-plugin-timeout-{}.pid", std::process::id()));
        let cmd = format!("sleep 30 & echo $! > '{}'; wait", path.display());
        let started = Instant::now();
        assert_eq!(shell(&cmd, Duration::from_millis(200)).unwrap_err(), "run: timed out");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_dead(pid.trim().parse().unwrap());
    }

    #[test]
    fn run_bounds_combined_output_and_preserves_stream_order() {
        assert_eq!(
            shell("printf out; printf err >&2; exit 3", BEFORE).unwrap(),
            ("outerr".into(), 3)
        );
        let err =
            shell("head -c 600000 /dev/zero; head -c 600000 /dev/zero >&2", BEFORE).unwrap_err();
        assert!(err.contains("output exceeds"), "{err}");
        // A command producing output forever is stopped by the same cap.
        assert!(shell("yes x", BEFORE).unwrap_err().contains("output exceeds"));
    }

    #[test]
    fn the_guide_loads() {
        let p =
            load("guide", &[("example.lua", include_str!("../../../docs/plugins/example.lua"))])
                .unwrap();
        let rm = r#"{"command":"rm -rf build"}"#;
        assert!(matches!(before(&p, "bash", rm).0, Before::Block(_)), "headless rm is blocked");
        assert_eq!(before(&p, "bash", r#"{"command":"npm run perform"}"#).0, Before::Pass);
        assert_eq!(before(&p, "bash", r#"{"command":"git status"}"#).0, Before::Allow);
        let (v, args, _) = before(&p, "bash", r#"{"command":"git log -3"}"#);
        assert_eq!((v, args.as_str()), (Before::Pass, r#"{"command":"git --no-pager log -3"}"#));
        let deploy = r#"{"function":{"description":"Restart a Kubernetes deployment on the homelab cluster.","name":"deploy","parameters":{"properties":{"namespace":{"description":"Defaults to \"default\"","type":"string"},"service":{"description":"Deployment name, e.g. \"api\"","type":"string"}},"required":["service"],"type":"object"}},"type":"function"}"#;
        assert!(p.schemas().starts_with(&format!("{deploy},")), "{}", p.schemas());
        assert!(p.schemas().contains(
            r#""limit":{"description":"How many, default 20","type":"integer"}},"required":[]"#
        ));
        assert!(matches!(before(&p, "deploy", r#"{"service":"api"}"#).0, Before::Block(_)));
    }

    #[test]
    fn plugin_tools_return_results_and_take_guards_from_other_files() {
        let tools = "--- Say hi.\n---@param n integer\nfunction tool_hi(n, cwd) \
            if n == 0 then error('zero') end \
            if n == 1 then return { n = n, ok = true } end \
            if n == 2 then return nil end \
            return 'hi ' .. n end";
        let p = load(
            "tools",
            &[
                ("a.lua", "function before_hi(n) if n == 9 then return 'no 9' end end"),
                ("b.lua", tools),
            ],
        )
        .unwrap();
        assert_eq!(p.tools(), ["hi"]);
        assert!(p.schemas().contains(r#""properties":{"n":{"type":"integer"}},"required":["n"]"#));
        let (to, _rx) = unbounded_channel();
        let run = |args: &str| {
            let o = p.run_tool("hi", args, &fields("ask", false), &to);
            (o.ok, o.content)
        };
        assert_eq!(run(r#"{"n":3}"#), (true, "hi 3".into()));
        assert_eq!(run(r#"{"n":1}"#), (true, r#"{"n":1,"ok":true}"#.into()));
        assert_eq!(run(r#"{"n":2}"#), (true, String::new()));
        assert_eq!(run(r#"{"n":0}"#), (false, "error: b.lua:3: zero".into()));
        assert!(
            matches!(before(&p, "hi", r#"{"n":9}"#).0, Before::Block(w) if w.ends_with("no 9"))
        );
    }

    #[test]
    fn bad_tools_stop_the_load() {
        let bad = |src: &str| load("badtool", &[("a.lua", src)]).err().unwrap();
        assert!(bad("--- x\nfunction tool_bash() end").contains("already a tool named `bash`"));
        assert!(bad("function tool_x() end").contains("tool_x: describe it"));
        let e = bad("--- x\n---@param n int\nfunction tool_x(n) end");
        assert!(e.contains("n has type `int` (available: string, number"), "{e}");
        assert!(
            bad("--- x\n---@param m string\nfunction tool_x(n) end").contains("@param m isn't")
        );
        let e = bad("--- x\nfunction tool_x(n) end\nfunction after_x(m) end");
        assert!(e.contains("after_x: no field `m` (available: n, cwd"), "{e}");
    }

    #[test]
    fn a_string_blocks_with_the_reason_and_the_first_block_wins() {
        let p = load(
            "block",
            &[
                ("a.lua", "function before_bash() return true end"),
                ("b.lua", "function before_bash(command) return 'no ' .. command end"),
                ("c.lua", "function before_bash() return 'c says no' end"),
            ],
        )
        .unwrap();
        let (v, _, _) = before(&p, "bash", r#"{"command":"ls"}"#);
        assert_eq!(v, Before::Block("error: blocked by b.lua. hint: no ls".into()));
    }

    #[test]
    fn errors_and_endless_loops_block() {
        let p = load(
            "fail",
            &[("a.lua", "function before_edit() error('boom') end function before_write() while true do end end")],
        )
        .unwrap();
        let Before::Block(why) = before(&p, "edit", "{}").0 else { panic!() };
        assert!(why.contains("a.lua:1: boom"), "{why}");
        let t = Instant::now();
        let Before::Block(why) = before(&p, "write", "{}").0 else { panic!() };
        assert!(why.contains("timed out") && t.elapsed() < BEFORE * 2, "{why}");
    }

    #[test]
    fn typos_stop_the_load_and_name_the_options() {
        let e = load("typo", &[("a.lua", "function before_bash(comand) end")]).err().unwrap();
        assert!(
            e.starts_with("✗ a.lua: before_bash: no field `comand` (available: command"),
            "{e}"
        );
        let e = load("tool", &[("a.lua", "function after_bsh() end")]).err().unwrap();
        assert!(e.contains("no tool `bsh` (available: ask, bash,"), "{e}");
        let e = load("syntax", &[("a.lua", "function before_bash(")]).err().unwrap();
        assert!(e.starts_with("✗ a.lua: "), "{e}");
        assert!(load("io", &[("a.lua", "io.open('x')")]).is_err(), "no io library");
        assert!(load("none", &[]).unwrap().routes.is_empty());
    }

    #[test]
    fn a_table_rewrites_only_the_tools_fields() {
        let p = load(
            "rewrite",
            &[("a.lua", "function before_bash() return { timeout = 5 } end function before_read() return { nope = 1 } end")],
        )
        .unwrap();
        let (v, args, _) = before(&p, "bash", r#"{"command":"ls"}"#);
        assert_eq!((v, args.as_str()), (Before::Pass, r#"{"command":"ls","timeout":5}"#));
        assert!(matches!(before(&p, "read", "{}").0, Before::Block(w) if w.contains("`nope`")));
        let (to, _rx) = unbounded_channel();
        let (mut args, mut acted) = (r#"{"command":"ls"}"#.to_owned(), Vec::new());
        p.before("bash", &mut args, &fields("ask", false), &to, &mut acted);
        assert_eq!(acted, ["rewritten by a.lua"]);
    }

    #[test]
    fn after_hooks_append_replace_and_survive_errors() {
        let p = load(
            "after",
            &[
                ("a.lua", "function after_bash(output, ok) return { output = output:upper() } end"),
                (
                    "b.lua",
                    "function after_bash(command, ok) return command .. ' ' .. tostring(ok) end",
                ),
                ("c.lua", "function after_bash() print('hi', 1) error('bad') end"),
            ],
        )
        .unwrap();
        let (to, mut rx) = unbounded_channel();
        let mut out = tools::Output { ok: true, content: "done".into() };
        let mut acted = Vec::new();
        p.after("bash", r#"{"command":"ls"}"#, &mut out, &fields("ask", false), &to, &mut acted);
        assert_eq!(acted, ["output replaced by a.lua"], "an append isn't noted; a replace is");
        assert_eq!(out.content, "DONE\n[b.lua] ls true");
        let Ok(Req::Print(s)) = rx.try_recv() else { panic!() };
        assert_eq!(s, "[c.lua] hi\t1");
        let Ok(Req::Print(s)) = rx.try_recv() else { panic!() };
        assert!(s.starts_with("c.lua: after_bash failed: c.lua:1: bad"), "{s}");
    }

    #[test]
    fn ask_and_run_reach_the_user_and_the_shell() {
        let src = "function before_bash(command) \
            local out, code = run('echo hi; exit 3') \
            local name = run{ 'printf', '%s', 'x' } \
            local text, err = read('/nonexistent') \
            if ask('ok?', { 'yes', 'no' }) ~= 'yes' then return out .. code .. name .. err end end";
        let p = Arc::new(load("ask", &[("a.lua", src)]).unwrap());
        // Headless, ask returns "".
        let Before::Block(why) = before(&p, "bash", "{}").0 else { panic!() };
        assert!(
            why.ends_with("hint: hi\n3x/nonexistent: No such file or directory (os error 2)"),
            "{why}"
        );
        // With a user, the question goes out and the answer comes back.
        let (to, mut rx) = unbounded_channel();
        let q = p.clone();
        let h = std::thread::spawn(move || {
            q.before("bash", &mut "{}".into(), &fields("ask", false), &to, &mut Vec::new())
        });
        let Some(Req::Ask { question, options, reply }) = rx.blocking_recv() else { panic!() };
        assert_eq!((question.as_str(), options.len()), ("ok?", 2));
        reply.send("yes".into()).unwrap();
        assert_eq!(h.join().unwrap(), Before::Pass);
    }
}
