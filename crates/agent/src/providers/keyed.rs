//! Providers that take an API key: from their variable, else from `auth.json`.

use super::{Auth, Spec};

pub const DEEPSEEK: Spec = Spec {
    name: "deepseek",
    base_url: "https://api.deepseek.com/v1",
    wire: llm::Api::Chat,
    auth: Auth::Key("DEEPSEEK_API_KEY"),
    cache_key: false,
    headers: |_, _| Vec::new(),
};

pub const OPENAI: Spec = Spec {
    name: "openai",
    base_url: "https://api.openai.com/v1",
    wire: llm::Api::Chat,
    auth: Auth::Key("OPENAI_API_KEY"),
    cache_key: true,
    headers: |_, _| Vec::new(),
};

/// https://opencode.ai/v2/docs/console/go/; only its Chat Completions models work. The
/// session header routes the cache to the server that saw it.
pub const OPENCODE_GO: Spec = Spec {
    name: "opencode-go",
    base_url: "https://opencode.ai/inference/go/openai/v1",
    wire: llm::Api::Chat,
    auth: Auth::Key("OPENCODE_API_KEY"),
    cache_key: false,
    headers: |session, _| vec![("x-opencode-session", session.into())],
};
