/// (provider, id, context window in tokens). OpenCode Go entries are from
/// `opencode.ai/inference/go/v1/models`, ChatGPT ones from the models Codex lists, Anthropic
/// ones from its model list, OpenRouter and Z.ai ones from
/// `openrouter.ai/api/v1/models` (Nov 2026).
const MODELS: &[(&str, &str, u32)] = &[
    ("anthropic", "claude-opus-5-5", 1_000_000),
    ("anthropic", "claude-sonnet-5-5", 1_000_000),
    ("anthropic", "claude-haiku-5-5", 1_000_000),
    ("anthropic", "claude-fable-5-1", 1_000_000),
    ("chatgpt", "gpt-6.1-sol", 500_000),
    ("chatgpt", "gpt-6.1-terra", 500_000),
    ("chatgpt", "gpt-6-sol", 272_000),
    ("chatgpt", "gpt-6-astra", 272_000),
    ("chatgpt", "gpt-6-luna", 272_000),
    ("chatgpt", "gpt-5.6-sol", 272_000),
    ("chatgpt", "gpt-5.6-terra", 272_000),
    ("chatgpt", "gpt-5.6-luna", 272_000),
    ("deepseek", "deepseek-chat", 128_000),
    ("deepseek", "deepseek-reasoner", 128_000),
    ("openai", "gpt-5", 400_000),
    ("openai", "gpt-5-mini", 400_000),
    ("opencode-go", "deepseek-v4.1-flash", 1_000_000),
    ("opencode-go", "deepseek-v4-pro", 1_000_000),
    ("opencode-go", "glm-5.2", 1_000_000),
    ("opencode-go", "glm-5.3", 1_000_000),
    ("opencode-go", "glm-5.3-flash", 1_000_000),
    ("opencode-go", "hy3", 256_000),
    ("opencode-go", "hy4-preview", 1_024_000),
    ("opencode-go", "kimi-k2.7-code", 262_144),
    ("opencode-go", "kimi-k3", 1_048_576),
    ("opencode-go", "kimi-k3.1-pro", 1_048_576),
    ("opencode-go", "mimo-v2.6-flash", 1_048_576),
    ("opencode-go", "mimo-v2.6-pro", 1_048_576),
    ("opencode-go", "minimax-m3.1-flash", 1_000_000),
    ("opencode-go", "longcat-2.0", 1_000_000),
    ("opencode-go", "step-5-preview", 1_000_000),
    ("opencode-go", "beam-free", 1_048_576),
    ("opencode-go", "iceman-contributor-free", 262_144),
    ("openrouter", "anthropic/claude-opus-5.5", 1_000_000),
    ("openrouter", "anthropic/claude-sonnet-5.5", 1_000_000),
    ("openrouter", "openai/gpt-6.1-sol", 1_050_000),
    ("openrouter", "google/gemini-4-argon", 1_048_576),
    ("openrouter", "x-ai/grok-4.7", 500_000),
    ("openrouter", "moonshotai/kimi-k3.1-pro", 1_048_576),
    ("openrouter", "z-ai/glm-5.3", 1_048_576),
    ("openrouter", "deepseek/deepseek-v4.1-flash", 1_048_576),
    ("zai", "glm-5.3", 1_048_576),
    ("zai", "glm-5.3-prime", 1_000_000),
    ("zai", "glm-5.3-flash", 1_048_576),
    ("zai", "glm-5.2", 1_048_576),
];

/// The context window of a built-in model, in tokens.
pub fn context(id: &str) -> Option<u32> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.2)
}

/// The built-in provider that serves `id`.
pub fn provider_of(id: &str) -> Option<&'static str> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.0)
}

/// The built-in model ids for `provider`.
pub fn list(provider: &str) -> impl Iterator<Item = &'static str> {
    MODELS.iter().filter(move |m| m.0 == provider).map(|m| m.1)
}
