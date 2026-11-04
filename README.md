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

- **Small.** A few thousand lines in five crates. You can read all of it.
- **Lean.** One binary of about 2.7 MB, running on a single-threaded tokio runtime.
- **Cheap to run.** The request prefix (model, tools, system prompt) is byte-identical on
  every turn, and history is only ever appended, so provider prompt caches keep hitting.
  The session id routes requests to the same cache, even after a resume, and on Anthropic
  bit places the cache markers itself.

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
```

In `-p` mode nobody can approve, so write, edit and bash are declined unless `--mode auto`
or `[bash] allow` permits them.

Keys: `Enter` send, `Esc` cancel the turn, `Ctrl-C` twice quit, `Shift-Tab` toggle
ask/auto, `Tab` next thinking level, `Up`/`Down` earlier prompts, `/` commands:

```
/login             add a provider: paste its API key, or sign in
/model [id]        pick the model, or set it to id
/thinking [level]  pick how hard the model thinks, or set it to level
/compact           summarize the conversation to free up context
/clear             start over; the model forgets this conversation
```

`/thinking` offers the levels the current model takes (glm-5.2 on opencode-go, for one,
takes only `high` and `max`), `off` where the model can skip thinking, and `default`, with
the level it means when known (`default (medium)`). The pick shows in the status line, is
kept for the next session, and goes only to models that take it; the rest use their default.

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

To add one, type `/login` and pick it. For an API key, paste it (it's hidden as you type);
for `chatgpt`, sign in on the page that opens in your browser. bit saves either in
`~/.config/bit/auth.json`, owner-only, and refreshes the ChatGPT sign-in itself. Or export
the key's variable, which wins over a saved key. More in
[docs/providers.md](docs/providers.md).

Everything bit keeps is in `~/.config/bit/` (or `$XDG_CONFIG_HOME/bit/`): `config.toml`,
`auth.json`, `model` (the last pick) and `sessions/`.

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

`read`, `write`, `edit`, `bash`, `grep`, `glob`. Long output is cut to its head and tail,
and the full text is saved to a file the model can page through.

## Sessions

Each conversation is a JSONL log in `~/.config/bit/sessions/<id>.jsonl`, written
after every step, so a resumed session sends the same bytes a running one would.

## License

MIT, see [LICENSE](LICENSE).
