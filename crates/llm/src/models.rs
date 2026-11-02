use crate::Usage;
use serde::Deserialize;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    #[serde(default = "default_context")]
    pub context: u32,
    pub input: f64,
    pub cached: f64,
    pub output: f64,
}

fn default_context() -> u32 {
    128_000
}

const fn p(context: u32, input: f64, cached: f64, output: f64) -> Price {
    Price { context, input, cached, output }
}

/// OpenCode Go prices are from `opencode.ai/inference/go/v1/models` (Nov 2026).
const MODELS: &[(&str, &str, Price)] = &[
    ("deepseek", "deepseek-chat", p(128_000, 0.28, 0.028, 0.42)),
    ("deepseek", "deepseek-reasoner", p(128_000, 0.28, 0.028, 0.42)),
    ("openai", "gpt-5", p(400_000, 1.25, 0.125, 10.0)),
    ("openai", "gpt-5-mini", p(400_000, 0.25, 0.025, 2.0)),
    ("opencode-go", "deepseek-v4.1-flash", p(1_000_000, 0.15, 0.003, 0.6)),
    ("opencode-go", "deepseek-v4-pro", p(1_000_000, 0.66, 0.022, 1.98)),
    ("opencode-go", "glm-5.2", p(1_000_000, 1.4, 0.26, 4.4)),
    ("opencode-go", "glm-5.3", p(1_000_000, 1.4, 0.26, 4.4)),
    ("opencode-go", "glm-5.3-flash", p(1_000_000, 0.15, 0.03, 0.5)),
    ("opencode-go", "hy3", p(256_000, 0.14, 0.035, 0.58)),
    ("opencode-go", "hy4-preview", p(1_024_000, 0.834, 0.042, 2.501)),
    ("opencode-go", "kimi-k2.7-code", p(262_144, 0.95, 0.19, 4.0)),
    ("opencode-go", "kimi-k3", p(1_048_576, 3.0, 0.3, 15.0)),
    ("opencode-go", "kimi-k3.1-pro", p(1_048_576, 2.0, 0.2, 10.0)),
    ("opencode-go", "mimo-v2.6-flash", p(1_048_576, 0.14, 0.0028, 0.28)),
    ("opencode-go", "mimo-v2.6-pro", p(1_048_576, 0.435, 0.003625, 0.87)),
    ("opencode-go", "minimax-m3.1-flash", p(1_000_000, 0.15, 0.015, 0.6)),
    ("opencode-go", "longcat-2.0", p(1_000_000, 0.3, 0.006, 1.2)),
    ("opencode-go", "step-5-preview", p(1_000_000, 1.0, 0.1, 2.7)),
    ("opencode-go", "beam-free", p(1_048_576, 0.0, 0.0, 0.0)),
    ("opencode-go", "iceman-contributor-free", p(262_144, 0.0, 0.0, 0.0)),
];

pub fn find(id: &str) -> Option<Price> {
    MODELS.iter().find(|m| m.1 == id).map(|m| m.2)
}

/// The built-in model ids for `provider`.
pub fn list(provider: &str) -> impl Iterator<Item = &'static str> {
    MODELS.iter().filter(move |m| m.0 == provider).map(|m| m.1)
}

impl Price {
    pub fn cost(&self, u: &Usage) -> f64 {
        let fresh = u.input.saturating_sub(u.cached);
        (f64::from(fresh) * self.input
            + f64::from(u.cached) * self.cached
            + f64::from(u.output) * self.output)
            / 1e6
    }
}
