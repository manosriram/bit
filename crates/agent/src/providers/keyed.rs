//! Providers that take an API key: from their variable, else from `auth.json`.

use super::{Auth, Spec, reasoning_effort};

pub const DEEPSEEK: Spec = keyed("deepseek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY");

pub const OPENAI: Spec =
    Spec { cache_key: true, ..keyed("openai", "https://api.openai.com/v1", "OPENAI_API_KEY") };

/// https://opencode.ai/v2/docs/console/go/; only its Chat Completions models work. The
/// session header routes the cache to the server that saw it.
pub const OPENCODE_GO: Spec = Spec {
    headers: |session, _| vec![("x-opencode-session", session.into())],
    ..keyed("opencode-go", "https://opencode.ai/inference/go/openai/v1", "OPENCODE_API_KEY")
};

/// The Messages API caches by explicit markers, not a key; `anthropic.rs` places them.
pub const ANTHROPIC: Spec = Spec {
    wire: llm::Api::Anthropic,
    thinking: |level| serde_json::json!({ "output_config": { "effort": level } }),
    ..keyed("anthropic", "https://api.anthropic.com/v1", "ANTHROPIC_API_KEY")
};

/// One key for most vendors' models, named `vendor/model`.
pub const OPENROUTER: Spec =
    keyed("openrouter", "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY");

/// Z.ai's pay-as-you-go API for the GLM models.
pub const ZAI: Spec = Spec {
    thinking: |level| {
        let kind = if level == "none" { "disabled" } else { "enabled" };
        serde_json::json!({ "thinking": { "type": kind } })
    },
    ..keyed("zai", "https://api.z.ai/api/paas/v4", "ZAI_API_KEY")
};

/// Shared API-key defaults; provider-specific wire, cache, headers and thinking stay above.
const fn keyed(name: &'static str, base_url: &'static str, env: &'static str) -> Spec {
    Spec {
        name,
        base_url,
        wire: llm::Api::Chat,
        auth: Auth::Key(env),
        cache_key: false,
        headers: |_, _| Vec::new(),
        thinking: reasoning_effort,
    }
}
