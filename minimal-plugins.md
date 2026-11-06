# Minimal plugins

The smallest build of the design in `PLUGINS.md` that still works the same way for a
plugin author: a `.lua` file, three hooks, parameter names as inputs, return values as
outputs, a routing table. Anything that can be added later without changing a plugin
already written is cut.

## What a user does

1. Asks bit: "make a plugin that asks me before any rm". Or writes the file by hand.
2. Restarts bit: `bit -c`.

That's all: no config, trust prompt, permission line or install step. The file lands in
`~/.config/bit/plugins/`, and every file there loads.

## What stays

| Piece | Why it can't go |
|---|---|
| Lua 5.4 in the binary (`mlua`) | "works on any machine" |
| `before_<tool>`, `after_<tool>`, `tool_<name>` | the three hooks |
| parameter names = inputs, checked at startup | the core idea; a typo must fail loudly |
| return values: `nil`, string, `true`, table | block, approve, rewrite, append, replace |
| routing table | one lookup per tool call |
| fail closed: an error or timeout in `before_` blocks | guards must never fail open |
| `run`, `read`, `ask`, `print` | the least a useful hook needs |
| one prompt bullet, `bit plugins`, `bit plugins guide` | so bit can write and check plugins |

## What's cut

Each item comes back without changing an existing plugin.

| Cut | Instead, for now | Add back when |
|---|---|---|
| project plugins (`.bit/plugins/`) | global only. A global plugin can check `cwd` to act per project | someone wants plugins shared through a repo |
| trust prompts, hashes, `trust.json` | not needed: only the user's own folder loads | with project plugins |
| permissions (`---@allow`) | none: it's the user's own code, like `[bash] allow` | with project plugins |
| `write`, `env`, `json`, `tool`, `model` | `run` covers them (`run{"printenv","X"}`, `run{"rg",…}`) | a real plugin needs one |
| `bit try` | `bit plugins` loads and checks; the model tests by restarting | testing by restarting hurts |
| `---@readonly` | custom tools always ask in `ask` mode, like `bash` | a read-only tool annoys someone |
| `exit_code` | `ok`, and the `[exit N]` trailer in `output` | — |
| `require`, `lib/` | one file per plugin | plugins get big |
| `--skip-plugin`, `--trust-plugins` | `--no-plugins` | — |
| separate budgets | one deadline: 2 s for `before_`, 60 s otherwise | — |

## Components

Four places change. Rough total: ~400 lines of Rust.

| Where | What | ~Lines |
|---|---|---|
| `agent/src/plugins.rs` (new) | load, validate, routing table, the three calls, the API | 300 |
| `agent/src/run.rs` | three call sites around `tools::exec` | 40 |
| `agent/src/context.rs` | append `tool_` schemas after `tools::SCHEMAS` | 5 |
| `cmd` | load at startup, `bit plugins`, `bit plugins guide`, `--no-plugins` | 40 |
| `agent/src/prompt.md` | the plugins bullet | 1 |
| `Cargo.toml` | `mlua = { features = ["lua54", "vendored", "send"] }` | 1 |

### `plugins.rs`

```rust
pub struct Plugins {
    states: Vec<(String, Lua)>,                 // (file name, its own Lua state)
    routes: HashMap<String, Vec<Hook>>,         // "before_bash" → hooks, in load order
    schemas: String,                            // tool_ schemas, joined, for the prefix
}

struct Hook { state: usize, f: Function, params: Vec<String> } // params in declared order

pub enum Before { Pass, Allow(String), Block(String) } // Allow names the file; a rewrite edits `args`

impl Plugins {
    pub fn load(dir: &Path) -> Result<Self, String>;          // any error stops startup
    // `acted` collects "rewritten by x.lua" and "output replaced by x.lua" for the tool line
    pub fn before(&self, tool: &str, args: &mut String, cx: &Fields, to: &Sender<Req>, acted: &mut Vec<String>) -> Before;
    pub fn after(&self, tool: &str, args: &str, out: &mut tools::Output, cx: &Fields, to: &Sender<Req>, acted: &mut Vec<String>);
    pub fn run_tool(&self, tool: &str, args: &str, cx: &Fields, to: &Sender<Req>) -> tools::Output;
    pub fn schemas(&self) -> &str;
}
```

`Fields` is `cwd`, `mode` and `headless`, the three every hook can ask for.

**Loading**, per `*.lua` file in alphabetical order:

1. A new state with `string`, `table`, `math`, `utf8` and `debug`. Keep `debug.getlocal`
   and `debug.getinfo` in Rust, then set the global `debug` to nil. Set a 4 MB memory
   limit.
2. Register `run`, `read`, `ask`, `print`.
3. Load the file as text (`ChunkMode::Text`) and run it.
4. For each global function named `before_*`, `after_*` or `tool_*`:
   - read its parameter names (`getinfo(f,"u").nparams`, then `getlocal(f, i)`);
   - `before_x`/`after_x`: `x` must be a built-in tool or some plugin's `tool_x`, and each
     name must be one of that tool's fields (read from `tools::SCHEMAS`), `cwd`, `mode`
     or `headless`, plus `output` and `ok` for `after_`;
   - `tool_x`: `x` must not be taken. Build its schema from the `---` lines above the
     function: plain lines are the description, `---@param name type` sets a type
     (string by default, `?` means optional);
   - push `Hook { state, f, params }` onto `routes[name]`.
5. Any error stops startup with `✗ <file>: <what> (available: …)`.

`tool_` descriptions come from the source text: read the lines above `linedefined`,
which `getinfo(f, "S")` gives.

**Calling** a hook: build its arguments from its declared names, in order, from the tool
arguments and `Fields`; missing ones are `nil`. Run it under the deadline:

- an instruction-count hook every 1,000 instructions errors past the deadline;
- every call runs on `spawn_blocking`, because the runtime is single-threaded (`rt`, no
  `rt-multi-thread`) and a busy Lua loop would freeze the TUI. One `Mutex` around
  `Plugins` is enough, since tool calls run one at a time (§5).

**The return value:**

| Hook | `nil` | string | `true` | table | error or timeout |
|---|---|---|---|---|---|
| `before_` | next plugin | `Block`, stop | `Allow`, continue | merge into `args`, continue | `Block("plugin x.lua failed: …")` |
| `after_` | — | append `[x.lua] text` | — | `output` field replaces it | notice, unchanged |
| `tool_` | empty result | the result | `tostring` | `tostring` | error result |

`before_` ends as `Block` if anything blocked, else `Allow` if anything allowed, else
`Pass`.

### The API

| Function | Implementation |
|---|---|
| `run(cmd [, {timeout=s}])` → `output, code` | `std::process::Command`: a table is argv, a string goes to `sh -c`. stderr merged. Poll `try_wait` against the deadline and kill on timeout. |
| `read(path)` → `text` or `nil, err` | `std::fs::read_to_string`, relative to `cwd` |
| `ask(q [, options])` → answer | see below; `""` when headless |
| `print(...)` | `Event::Notice("[x.lua] …")` with control characters stripped, at most 20 lines |

`run` kills only `sh`, not its children. That's a known limit: when a stray grandchild
matters, use the process-group kill the bash tool has.

`ask` is the one tricky part. The hook runs on a blocking thread, but answers arrive on
the agent's `replies` channel. Lua's `ask` sends `(question, options, oneshot::Sender)`
on an mpsc and waits with `blocking_recv`. In `run.rs`, the agent `select!`s between the
hook's `JoinHandle` and that mpsc. A request goes through the existing `Agent::ask` path
(`Event::Ask`, then `answer(replies, id)`), and the reply is sent back. Time spent waiting
pushes the deadline back by the same amount.

### `run.rs`

Around today's dispatch (`run.rs:188` and `:213`):

```rust
let fields = Fields { cwd, mode, headless: self.a.print };
match plugins.before(name, &mut args, &fields) {      // first, every mode
    Before::Block(why) => out = blocked(&why),        // error: blocked by … hint: …
    Before::Allow => out = exec(name, &args).await,
    Before::Pass => { /* today's policy.check → maybe Approval → exec */ }
}
plugins.after(name, &args, &mut out, &fields);
```

`exec` tries `plugins.run_tool` first and falls back to `tools::exec`. A rewritten `args` is
what the Approval box shows and what runs; the log keeps the model's own call, so the
history stays exactly what the model sent. `tools::summary` falls back to the tool name,
so replays work without the plugin.

### `cmd`

- At startup: `Plugins::load(config_dir/plugins)` unless `--no-plugins`. An error prints
  and exits 1. A missing folder means no plugins.
- `bit plugins`: load, then print each file with its hooks and tools, or its error. Exits
  1 on an error. This is how the model checks its own work.
- `bit plugins guide`: prints `docs/plugins/example.lua` (`include_str!`), which covers
  only what the binary has. A test loads it, so the guide can't drift into an example
  that doesn't load.

### The prompt bullet

> Plugins: .lua files in ~/.config/bit/plugins/ that guard tool calls (before_bash…),
> check results (after_edit…) or add tools (tool_<name>). When the user wants bit to
> always or never do something, offer one. Run `bit plugins guide` before writing one
> and `bit plugins` after. They load at startup: tell the user to run `bit -c`.

## Tests

`.lua` fixtures in `agent/testdata/plugins/`, one scripted session each:

1. A `before_bash` string blocks, and the model gets the reason.
2. A `before_bash` that errors, or loops forever, blocks within ~2 s.
3. A typo'd parameter or `before_bsh` fails `load` and names the valid options.
4. Two plugins: the first block wins; a `true` doesn't override a later block.
5. A table rewrites `command`; `true` runs without an approval in `ask` mode.
6. `after_edit` appends `[x.lua] …`; `{ output = … }` replaces.
7. `tool_x` with `---@param n integer` has that schema in the prefix, byte-identical
   across turns, and its result reaches the model.
8. `ask` returns `""` in print mode and the answer in a scripted TUI session.
9. `size.sh` and `rss.sh` still pass.

## Build order

1. `plugins.rs` with `before_`, `run` and `print`, plus the `run.rs` hook-up and
   `--no-plugins`. A guard works.
2. `after_`.
3. `tool_` and the prefix.
4. `ask`.
5. `bit plugins`, `bit plugins guide`, the prompt bullet. Bit can now write plugins.

Each step ships on its own. Phase 1 (steps 1, 2, 4, 5) and phase 2 (step 3) are done.

Phase 2 notes:
- `---@param` types are `string`, `number`, `integer`, `boolean`, `array`, `object`.
  A description is required. Parameters named `cwd`, `mode` or `headless` are filled
  by bit and left out of the schema.
- Hooks are checked after every file loads, so `before_x` may sit in another file than
  `tool_x`.
- A custom tool's approval box shows its raw arguments (`tools::detail` falls back to
  them).
- `bit --print-prompt` still shows only the built-in tools: it runs before plugins load.
