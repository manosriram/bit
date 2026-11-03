# Providers

bit has these providers built in:

| Provider | Models | How you add it |
|---|---|---|
| `deepseek` | `deepseek-chat`, `deepseek-reasoner` | API key |
| `openai` | `gpt-5`, `gpt-5-mini` | API key |
| `anthropic` | `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`, `claude-fable-5-1` | API key |
| `opencode-go` | the OpenCode Go models (`kimi-k3`, `glm-5.3`, ...) | API key |
| `chatgpt` | `gpt-6.1-sol`, `gpt-6-sol`, ... | your ChatGPT plan (Plus, Pro, ...) |

## Add a provider

In bit, type:

```
/login
```

and pick the provider.

- **API key:** paste the key and press Enter. It's hidden as you type.
- **`chatgpt`:** sign in on the page that opens in your browser. If no browser opens, bit
  prints the link.

bit saves the key or sign-in in `~/.config/bit/auth.json`, readable only by you, and the
provider works right away. Pick one of its models with `/model`.

Run `/login` again with a new key to replace one.

## Or use environment variables

A key in the environment wins over a saved one. Useful in scripts and CI:

```sh
export DEEPSEEK_API_KEY=sk-...
export OPENAI_API_KEY=sk-...
export ANTHROPIC_API_KEY=sk-ant-...
export OPENCODE_API_KEY=...
```

## Any other OpenAI-compatible provider

Add it to `~/.config/bit/config.toml` under a name of your choice. OpenRouter, for example:

```toml
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
models = ["moonshotai/kimi-k3"]
```

Restart bit, then `/login` and pick `openrouter` to paste its key (or
`export OPENROUTER_API_KEY=sk-or-...`).

Any provider that serves the Chat Completions API (`POST <base_url>/chat/completions`)
works this way: Groq, Mistral, xAI, Together, Fireworks, a local server, and so on.

## Check

Type `/model`. A provider without a key shows `(not set)` next to it.
