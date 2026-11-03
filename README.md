<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <img alt="bit" src="assets/logo-light.svg" width="220">
  </picture>
</p>

bit is a terminal coding agent with strong focus on tiny core, size, and memory footprint.

<p align="center">
  <a href="assets/demo.mp4"><img alt="bit fixing a failing test, compacting, and resuming by id" src="assets/demo.gif" width="800"></a>
</p>

## Why

- **Small.** About 3k lines in five crates, with hard per-crate line budgets
  enforced in CI. You can read all of it.
- **Lean.** One binary of about 2.5 MB, running on a single-threaded tokio runtime.
- **Cheap to run.** The request prefix (model, tools, system prompt) is byte-identical on
  every turn, and history is only ever appended, so provider prompt caches keep hitting.
  The session id routes requests to the same cache, even after a resume.

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

```
bit                start a session
bit -c             continue the last session in this directory
bit -r <id>        resume a session by id or id prefix
bit -p "<prompt>"  run one turn and exit: the reply on stdout, exit 1 if it fails
--model <id>       model to use (default: the last one picked with /model)
--mode ask|auto    ask before write, edit and bash (default ask), or don't
```

In `-p` mode nobody can approve, so write, edit and bash are declined unless `--mode auto`
or `[bash] allow` permits them.

Keys: `Enter` send, `Esc` cancel the turn, `Ctrl-C` twice quit, `Shift-Tab` toggle
ask/auto, `Up`/`Down` earlier prompts, `/` commands.

## Providers

`/model` lists the models of every provider and switches to the one you pick; bit
starts on your last pick. Without one, it starts on `provider` in the config, or the first
provider whose key is set:

| Provider | Key |
|---|---|
| `deepseek` | `DEEPSEEK_API_KEY` |
| `openai` | `OPENAI_API_KEY` |
| `opencode-go` | `OPENCODE_API_KEY` |

## Config

`~/.config/bit/config.toml`, optional:

```toml
model = "kimi-k3"
provider = "opencode-go"

[providers.local]                 # any OpenAI-compatible endpoint
base_url = "http://localhost:8080/v1"
api_key_env = "LOCAL_API_KEY"
models = ["qwen3-coder"]          # what /model offers for it

[providers.openai]                # extra models for a built-in provider
models = ["gpt-5.1"]

[bash]
allow = ["cargo test", "git status"]   # run without asking
```

## Tools

`read`, `write`, `edit`, `bash`, `grep`, `glob`. Long output is cut to its head and tail,
and the full text is saved to a file the model can page through.

## Sessions

Each conversation is a JSONL log in `~/.local/share/bit/sessions/<id>.jsonl`, written
after every step, so a resumed session sends the same bytes a running one would.
