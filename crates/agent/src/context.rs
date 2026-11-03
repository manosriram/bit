use serde_json::value::RawValue;

const PROMPT: &str = include_str!("prompt.md");

pub struct Context {
    /// Request head up to and including the system message. Fixed while the model is, so
    /// the provider's prompt cache hits on every turn.
    prefix: String,
    pub(crate) model: String,
    cache_key: Option<String>,
    /// Input tokens of the last request: how full the window is. 0 until one is sent.
    pub(crate) tokens: u32,
    pub(crate) messages: Vec<Box<RawValue>>,
}

impl Context {
    /// `model` may be empty until the user picks one. `cache_key` is sent as
    /// `prompt_cache_key`, for providers that route their cache by it. `messages` is the
    /// history of a resumed session.
    pub fn new(model: &str, cache_key: Option<&str>, messages: Vec<Box<RawValue>>) -> Self {
        let (prefix, cache_key) = (String::new(), cache_key.map(str::to_owned));
        let mut cx = Self { prefix, model: String::new(), cache_key, tokens: 0, messages };
        cx.set_model(model);
        // A crash can cut a turn short; mend it the same way a cancel would.
        // A lone user message is kept: on load it can only be a compaction summary.
        cx.answer_open("error: bit stopped before this call finished");
        cx
    }

    pub(crate) fn set_model(&mut self, model: &str) {
        let mut head = serde_json::json!({"model": model, "stream": true, "stream_options": {"include_usage": true}});
        if let Some(k) = &self.cache_key {
            head["prompt_cache_key"] = k.as_str().into();
        }
        let mut prefix = head.to_string();
        prefix.pop();
        prefix.push_str(",\"tools\":");
        prefix.push_str(tools::SCHEMAS);
        prefix.push_str(",\"messages\":[");
        prefix.push_str(msg("system", PROMPT).get());
        (self.prefix, self.model) = (prefix, model.into());
    }

    /// The §4 rules for a turn that started at `start` and didn't finish: a user message with
    /// no reply goes, and every tool call left without a result gets `why` as its result.
    pub(crate) fn repair(&mut self, start: usize, why: &str) {
        if self.messages.len() == start + 1 {
            self.messages.truncate(start);
            return;
        }
        self.answer_open(why);
    }

    /// Gives every tool call of the last assistant message that has no result `why` as one.
    fn answer_open(&mut self, why: &str) {
        let field = |m: &RawValue, k: &str| -> serde_json::Value {
            serde_json::from_str::<serde_json::Value>(m.get())
                .map_or_else(|_| Default::default(), |v| v[k].clone())
        };
        let Some(i) = self.messages.iter().rposition(|m| field(m, "role") == "assistant") else {
            return;
        };
        let answered: Vec<serde_json::Value> =
            self.messages[i + 1..].iter().map(|m| field(m, "tool_call_id")).collect();
        let calls = field(&self.messages[i], "tool_calls");
        for c in calls.as_array().into_iter().flatten() {
            if let Some(id) = c["id"].as_str().filter(|id| !answered.iter().any(|a| a == id)) {
                self.messages.push(tool_msg(id, why));
            }
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Concatenates the stored pieces; nothing is serialized again.
    pub(crate) fn body(&self) -> String {
        self.messages.iter().fold(self.prefix.clone(), |b, m| b + "," + m.get()) + "]}"
    }

    /// The request that asks for a summary: the history plus `ask`. Tools stay on, since
    /// turning them off makes providers drop them from the prompt and miss the cache.
    pub(crate) fn summary_body(&self, ask: &RawValue) -> String {
        let body = self.body();
        format!("{},{}]}}", &body[..body.len() - 2], ask.get())
    }

    /// Whether the last request used more than 70% of the model's context window.
    pub(crate) fn full(&self) -> bool {
        let window = llm::models::context(&self.model).unwrap_or(128_000);
        u64::from(self.tokens) * 10 > u64::from(window) * 7
    }
}

pub(crate) fn msg(role: &str, content: &str) -> Box<RawValue> {
    RawValue::from_string(serde_json::json!({"role": role, "content": content}).to_string())
        .expect("json! is valid JSON")
}

pub(crate) fn tool_msg(id: &str, content: &str) -> Box<RawValue> {
    let m = serde_json::json!({"role": "tool", "tool_call_id": id, "content": content});
    RawValue::from_string(m.to_string()).expect("json! is valid JSON")
}
