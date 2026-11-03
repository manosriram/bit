/// (provider, id, context window in tokens, thinking levels from least to most, the level the
/// provider uses when none is sent). The levels and defaults are the ones the provider's own
/// model list gives (OpenRouter's for ChatGPT and OpenAI; Anthropic's docs for Claude); no
/// levels means bit can't set it, an empty default that no source states one. `off` is the
/// providers' `none`: thinking switched off. Z.ai's `off` and `high` turn GLM thinking off
/// and on. OpenCode Go entries are from
/// `opencode.ai/inference/go/v1/models`, ChatGPT ones from the models Codex lists, Anthropic
/// ones from its model list, OpenRouter and Z.ai ones from
/// `openrouter.ai/api/v1/models` (Nov 2026).
const FIVE: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const SIX: &[&str] = &["off", "low", "medium", "high", "xhigh", "max"];

#[allow(clippy::type_complexity)]
const MODELS: &[(&str, &str, u32, &[&str], &str)] = &[
    ("anthropic", "claude-opus-5-5", 1_000_000, FIVE, "medium"),
    ("anthropic", "claude-sonnet-5-5", 1_000_000, FIVE, "high"),
    ("anthropic", "claude-haiku-5-5", 1_000_000, FIVE, "medium"),
    ("anthropic", "claude-fable-5-1", 1_000_000, FIVE, "high"),
    ("chatgpt", "gpt-6.1-sol", 500_000, FIVE, "medium"),
    ("chatgpt", "gpt-6.1-terra", 500_000, SIX, "medium"),
    ("chatgpt", "gpt-6-sol", 272_000, SIX, "medium"),
    ("chatgpt", "gpt-6-astra", 272_000, FIVE, "medium"),
    ("chatgpt", "gpt-6-luna", 272_000, SIX, "medium"),
    ("chatgpt", "gpt-5.6-sol", 272_000, SIX, "medium"),
    ("chatgpt", "gpt-5.6-terra", 272_000, SIX, "medium"),
    ("chatgpt", "gpt-5.6-luna", 272_000, SIX, "medium"),
    ("deepseek", "deepseek-chat", 128_000, &[], ""),
    ("deepseek", "deepseek-reasoner", 128_000, &[], ""),
    ("openai", "gpt-5", 400_000, &["minimal", "low", "medium", "high"], "medium"),
    ("openai", "gpt-5-mini", 400_000, &["minimal", "low", "medium", "high"], "medium"),
    ("opencode-go", "deepseek-v4.1-flash", 1_000_000, &["low", "high", "max"], ""),
    ("opencode-go", "deepseek-v4-pro", 1_000_000, &["high", "max"], ""),
    ("opencode-go", "glm-5.2", 1_000_000, &["high", "max"], ""),
    ("opencode-go", "glm-5.3", 1_000_000, &["low", "high", "max"], ""),
    ("opencode-go", "glm-5.3-flash", 1_000_000, &["low", "high", "max"], ""),
    ("opencode-go", "hy3", 256_000, &["off", "low", "high"], ""),
    ("opencode-go", "hy4-preview", 1_024_000, &["off", "high"], ""),
    ("opencode-go", "kimi-k2.7-code", 262_144, &[], ""),
    ("opencode-go", "kimi-k3", 1_048_576, &["max"], ""),
    ("opencode-go", "kimi-k3.1-pro", 1_048_576, &["low", "high", "max"], "max"),
    ("opencode-go", "mimo-v2.6-flash", 1_048_576, &[], ""),
    ("opencode-go", "mimo-v2.6-pro", 1_048_576, &[], ""),
    ("opencode-go", "minimax-m3.1-flash", 1_000_000, FIVE, ""),
    ("opencode-go", "longcat-2.0", 1_000_000, &[], ""),
    ("opencode-go", "step-5-preview", 1_000_000, &["low", "medium", "high"], ""),
    ("opencode-go", "beam-free", 1_048_576, FIVE, ""),
    ("opencode-go", "iceman-contributor-free", 262_144, &[], ""),
    ("openrouter", "anthropic/claude-opus-5.5", 1_000_000, FIVE, "high"),
    ("openrouter", "anthropic/claude-sonnet-5.5", 1_000_000, FIVE, "high"),
    ("openrouter", "openai/gpt-6.1-sol", 1_050_000, FIVE, "medium"),
    ("openrouter", "google/gemini-4-argon", 1_048_576, &["low", "medium", "high"], "medium"),
    ("openrouter", "x-ai/grok-4.7", 500_000, &["low", "medium", "high", "xhigh"], "high"),
    ("openrouter", "moonshotai/kimi-k3.1-pro", 1_048_576, &["off", "low", "high", "max"], "max"),
    ("openrouter", "z-ai/glm-5.3", 1_048_576, &["low", "high", "max"], "max"),
    (
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
        1_048_576,
        &["off", "low", "high", "max"],
        "high",
    ),
    ("zai", "glm-5.3", 1_048_576, &[], ""),
    ("zai", "glm-5.3-prime", 1_000_000, &[], ""),
    ("zai", "glm-5.3-flash", 1_048_576, &[], ""),
    ("zai", "glm-5.2", 1_048_576, &["off", "high"], ""),
];

/// The context window of a built-in model, in tokens.
pub fn context(id: &str) -> Option<u32> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.2)
}

/// The built-in provider that serves `id`.
pub fn provider_of(id: &str) -> Option<&'static str> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.0)
}

/// The thinking levels `provider` takes for `id`, least first; empty for an unknown model.
pub fn levels(provider: &str, id: &str) -> &'static [&'static str] {
    MODELS.iter().find(|m| m.0 == provider && m.1 == id).map_or(&[], |m| m.3)
}

/// The thinking level `provider` uses for `id` when none is sent, if known.
pub fn default_level(provider: &str, id: &str) -> Option<&'static str> {
    MODELS.iter().find(|m| m.0 == provider && m.1 == id).map(|m| m.4).filter(|d| !d.is_empty())
}

/// The built-in model ids for `provider`.
pub fn list(provider: &str) -> impl Iterator<Item = &'static str> {
    MODELS.iter().filter(move |m| m.0 == provider).map(|m| m.1)
}
