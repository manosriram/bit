<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-horizontal-dark.svg">
    <img alt="bit" src="assets/logo-horizontal-light.svg" width="192">
  </picture>
</p>

bit is a terminal coding agent that is cheap to run, safe to extend, and small enough to
run anywhere.

<p align="center">
  <a href="assets/demo.mp4"><img alt="bit fixing a failing test, compacting, then writing a plugin from one sentence that asks before rm and blocks a delete" src="assets/demo.gif" width="800"></a>
</p>

## Why

- **Cheap to run.** The request prefix (model, tools, system prompt) is byte-identical on
  every turn, and history is only ever appended, so provider prompt caches keep hitting,
  even after a resume. Each turn ends with the share of input that was cached.
- **Safe to extend.** Plugins are sandboxed Lua: no `io`, `os` or `require`, 4 MB of
  memory, a deadline on every hook. A guard is a few lines you can read before you trust
  it, and one that breaks blocks the call instead of letting it through.
- **Runs anywhere.** One binary of about 3.2 MB with Lua built in, and ripgrep its only
  dependency: it works over SSH, in a container, or in CI with `bit -p`.
- **Light on memory.** About 7 MB idle and 12 MB at the peak of a long turn, per session,
  even with 50 skills installed. See [Memory](#memory).
- **Small.** A few thousand lines in five crates. You can read all of it.

bit leaves out MCP, LSP and sub-agents on purpose: each would cost the properties above.

## Install

```
curl -fsSL https://raw.githubusercontent.com/manosriram/bit/main/install.sh | sh
```

Installs the latest release to `~/.local/bin` (macOS and Linux, arm64 and x86_64), and
[ripgrep](https://github.com/BurntSushi/ripgrep) if it's missing.

From source:

```
cargo build --release      # target/release/bit
```

`grep` and `glob` need ripgrep (`rg`) on `PATH`.

## Use

On first run, type `/login` and pick a provider to add its key (see [Providers](#providers)).

```
bit                start a session
bit -c             continue the last session in this directory
bit -r <id>        resume a session by id or id prefix
bit -p "<prompt>"  run one turn and exit: the reply on stdout, exit 1 if it fails
--model <id>       model to use (default: the last one picked with /model)
--mode ask|auto    ask before write, edit and bash (default ask), or don't
--no-plugins       start without plugins
bit plugins        check that every plugin loads, and list its hooks and tools
bit plugins guide  print the plugin guide
bit skills         list installed skills
bit skills add <repo>    install a skill from a git repo
bit skills remove <name> remove a skill
bit skills update [<name>] pull the latest skill(s)
```

In `-p` mode nobody can approve, so write, edit and bash are declined unless `--mode auto`
or `[bash] allow` permits them.

Keys: `Enter` send, `Esc` cancel the turn, `Ctrl-C` twice quit, `Tab` toggle
ask/auto (the switch at the left of the status line, amber in auto), `Shift-Tab` next
thinking level, `Up`/`Down` earlier prompts, `Option-Left`/`Right` move by word,
`Option-Backspace` or `Ctrl-W` delete a word, `/` commands:

```
/login             add a provider: paste its API key, or sign in
/model [id]        pick the model, or set it to id
/thinking [level]  pick how hard the model thinks, or set it to level
/sessions          list sessions and resume another one
/skills [add <repo>]  show installed skills or add one
/compact           summarize the conversation to free up context
/undo              put back the files the last turn edited; the model forgets that turn
/clear             start over; the model forgets this conversation
/exit              quit bit
```

`/thinking` offers the levels the current model takes (glm-5.2 on opencode-go, for one,
takes only `high` and `max`), `off` where the model can skip thinking, and `default`, with
the level it means when known (`default (medium)`). The pick shows in the status line, is
kept for the next session, and goes only to models that take it; the rest use their default.

`/undo` restores each file the last turn changed with `edit` or `write` and deletes the ones
it created, then drops the turn from the history. It can't undo `bash` or plugin tools, and
says so when the turn ran one. It goes back one turn, within the running session.

## Providers

`/model` lists the models of every provider and switches to the one you pick; bit
starts on your last pick. Without one, it starts on `provider` in the config, or the first
provider whose key is set:

| Provider | `/login` takes | Or export |
|---|---|---|
| `deepseek` | API key | `DEEPSEEK_API_KEY` |
| `openai` | API key | `OPENAI_API_KEY` |
| `anthropic` | API key | `ANTHROPIC_API_KEY` |
| `opencode-go` | API key | `OPENCODE_API_KEY` |
| `openrouter` | API key | `OPENROUTER_API_KEY` |
| `zai` | API key | `ZAI_API_KEY` |
| `chatgpt` | a browser sign-in to your ChatGPT plan (Plus, Pro, ...) | |
| `ollama` | nothing: its models are the ones pulled on the local server | |

To add one, type `/login` and pick it. For an API key, paste it (it's hidden as you type);
for `chatgpt`, sign in on the page that opens in your browser. bit saves either in
`~/.config/bit/auth.json`, owner-only, and refreshes the ChatGPT sign-in itself. Or export
the key's variable, which wins over a saved key. More in
[docs/providers.md](docs/providers.md).

Everything bit keeps is in `~/.config/bit/` (or `$XDG_CONFIG_HOME/bit/`): `config.toml`,
`auth.json`, `model` (the last pick), `sessions/`, `plugins/` and `skills/`.

## Config

`~/.config/bit/config.toml`, optional:

```toml
model = "kimi-k3"
provider = "opencode-go"

[providers.local]                 # any OpenAI-compatible endpoint; /login takes its key
base_url = "http://localhost:8080/v1"
api_key_env = "LOCAL_API_KEY"
models = ["qwen3-coder"]          # what /model offers for it

[providers.openai]                # extra models for a built-in provider
models = ["gpt-5.1"]

[bash]
allow = ["cargo test", "git status"]   # run without asking
```

## Tools

`read`, `write`, `edit`, `bash`, `grep`, `glob`, plus any a plugin adds. Long output is
cut to its head and tail, and the full text is saved to a file the model can page through.

## Plugins

A plugin is a Lua file in `~/.config/bit/plugins/`. Lua 5.4 is built into bit, so nothing
needs installing. The easiest way to get one is to ask bit ("ask me before any rm"): it
reads the guide, writes the file and checks it. Plugins load at startup, so restart with
`bit -c` afterwards.

A function's name says when it runs, its parameter names say what it gets, and its return
value says what happens:

```lua
-- Before every bash call, in every mode, ahead of bit's own approval.
function before_bash(command, mode)
  if command:match("^git push") and mode == "auto" then
    return "Pushing needs a human."                -- block; the model reads why
  end
  if command == "git status" then return true end  -- run without asking
end

-- After every edit: a string is added to what the model sees.
function after_edit(path, ok)
  if ok and path:match("%.go$") then
    local out = run{ "gofmt", "-l", path }
    if out ~= "" then return path .. " isn't gofmt-formatted" end
  end
end

--- Restart a deployment. (The --- lines describe the tool to the model.)
---@param service string
function tool_deploy(service)
  local out, code = run{ "kubectl", "rollout", "restart", "deployment/" .. service }
  if code ~= 0 then error(out) end
  return out
end
```

- **Hooks:** `before_<tool>` can block (return a string), allow (`true`) or rewrite the
  arguments (a table). `after_<tool>` can add to the output (a string) or replace it
  (`{ output = ... }`). `tool_<name>` adds a tool, which asks for approval in ask mode like
  `bash`. Hooks work on plugin tools too.
- **You see what they change:** when a plugin skips your approval, rewrites a call or
  replaces its output, the tool line says so: `[1 line · allowed by guard.lua]`.
- **API:** `run(cmd)`, `read(path)`, `ask(question, options)` and `print(...)`.
- **Safe by default:** a typo in a name or parameter stops startup and lists the valid
  ones. An error or a timeout (2 s) in a `before_` hook blocks the call. Each plugin gets
  its own Lua state with 4 MB of memory and no `io`, `os` or `require`.

`bit plugins guide` prints [docs/plugins/example.lua](docs/plugins/example.lua): every
feature, as one working plugin.

### How plugins fit into bit

```
  ~/.config/bit/plugins/*.lua
            │
            │  at startup: one Lua state per file; hook names and
            │  parameter names are checked, or bit doesn't start
            ▼
  routing table                       request prefix
    before_bash → a.lua, b.lua          built-in tools
    after_edit  → a.lua                 + tool_ schemas  (fixed for the session)
    tool_deploy → ops.lua
            │
            │  on every tool call the model makes
            ▼
  ┌─ before_<tool> hooks ─────────────── 2 s each, on a background thread
  │    "reason" ─────────────────────────► blocked: the model gets the reason
  │    { field = … }                       arguments rewritten
  │    true                                approval skipped
  │    nil                                 no opinion
  │    error or timeout ─────────────────► blocked: plugins fail closed
  ▼
  approval  (ask mode, unless a hook returned true or [bash] allow covers it)
  ▼
  run the tool: a built-in, or the plugin's tool_<name>
  ▼
  ┌─ after_<tool> hooks ──────────────── 60 s each
  │    "text"                              added to the output
  │    { output = … }                      output replaced
  ▼
  result ──► model

  Any hook can ask() and print(): bit puts the question or the note on screen
  (ask() returns "" under bit -p) and hands the answer back to the hook.
```

## Sessions

Each conversation is a JSONL log in `~/.config/bit/sessions/<id>.jsonl`, written
after every step, so a resumed session sends the same bytes a running one would.

## Memory

Ten bit sessions running at once, each through one turn of 150 tool calls (2 MB bash
outputs, reads of a 12 MB file, greps, writes, edits). Memory is per bit process, except
the last column, which adds up all ten:

| Scenario | Idle | Peak (avg) | Peak (worst) | All 10 at peak |
|---|---:|---:|---:|---:|
| No skills, 128k context window | 7.0 MB | 12.1 MB | 12.4 MB | 121 MB |
| 50 skills, 128k window | 7.2 MB | 12.4 MB | 12.6 MB | 124 MB |
| 50 skills, 1M window | 7.2 MB | 25.0 MB | 26.9 MB | 250 MB |

Measured on an Apple A18 Pro against a local mock provider, so it tests memory, not speed
or cost. Method and how to rerun it: [docs/benchmark.md](docs/benchmark.md).

## License

MIT, see [LICENSE](LICENSE).
