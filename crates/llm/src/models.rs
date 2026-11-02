/// (provider, id, context window in tokens). OpenCode Go entries are from
/// `opencode.ai/inference/go/v1/models` (Nov 2026).
const MODELS: &[(&str, &str, u32)] = &[
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
];

/// The context window of a built-in model, in tokens.
pub fn context(id: &str) -> Option<u32> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.2)
}

/// The built-in model ids for `provider`.
pub fn list(provider: &str) -> impl Iterator<Item = &'static str> {
    MODELS.iter().filter(move |m| m.0 == provider).map(|m| m.1)
}
