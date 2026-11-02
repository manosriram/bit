use serde_json::value::RawValue;

const PROMPT: &str = include_str!("prompt.md");

pub struct Context {
    /// Request head up to and including the system message. Fixed for the session, so the
    /// provider's prompt cache hits on every turn.
    prefix: String,
    pub(crate) messages: Vec<Box<RawValue>>,
}

impl Context {
    /// `cache_key` is sent as `prompt_cache_key`, for providers that route their cache by it.
    /// `messages` is the history of a resumed session.
    pub fn new(model: &str, cache_key: Option<&str>, messages: Vec<Box<RawValue>>) -> Self {
        let mut head = serde_json::json!({"model": model, "stream": true, "stream_options": {"include_usage": true}});
        if let Some(k) = cache_key {
            head["prompt_cache_key"] = k.into();
        }
        let mut prefix = head.to_string();
        prefix.pop();
        prefix.push_str(",\"tools\":");
        prefix.push_str(tools::SCHEMAS);
        prefix.push_str(",\"messages\":[");
        prefix.push_str(msg("system", PROMPT).get());
        let mut cx = Self { prefix, messages };
        // A crash can cut a turn short; mend it the same way a cancel would.
        cx.repair(0, "error: bit stopped before this call finished");
        cx
    }

    /// The §4 rules for a turn that started at `start` and didn't finish: a user message with
    /// no reply goes, and every tool call left without a result gets `why` as its result.
    pub(crate) fn repair(&mut self, start: usize, why: &str) {
        if self.messages.len() == start + 1 {
            self.messages.truncate(start);
            return;
        }
        let field = |m: &RawValue, k: &str| -> serde_json::Value {
            serde_json::from_str::<serde_json::Value>(m.get())
                .map_or_else(|_| Default::default(), |v| v[k].clone())
        };
        let Some(i) = self
            .messages
            .iter()
            .rposition(|m| field(m, "role") == "assistant")
        else {
            return;
        };
        let answered: Vec<serde_json::Value> = self.messages[i + 1..]
            .iter()
            .map(|m| field(m, "tool_call_id"))
            .collect();
        let calls = field(&self.messages[i], "tool_calls");
        for c in calls.as_array().into_iter().flatten() {
            if let Some(id) = c["id"]
                .as_str()
                .filter(|id| !answered.iter().any(|a| a == id))
            {
                self.messages.push(tool_msg(id, why));
            }
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Concatenates the stored pieces; nothing is serialized again.
    pub(crate) fn body(&self) -> String {
        let len = self
            .messages
            .iter()
            .map(|m| m.get().len() + 1)
            .sum::<usize>();
        let mut body = String::with_capacity(self.prefix.len() + len + 2);
        body.push_str(&self.prefix);
        for m in &self.messages {
            body.push(',');
            body.push_str(m.get());
        }
        body.push_str("]}");
        body
    }
}

pub(crate) fn msg(role: &str, content: &str) -> Box<RawValue> {
    serde_json::value::to_raw_value(&serde_json::json!({"role": role, "content": content}))
        .expect("strings always serialize")
}

pub(crate) fn tool_msg(id: &str, content: &str) -> Box<RawValue> {
    serde_json::value::to_raw_value(
        &serde_json::json!({"role": "tool", "tool_call_id": id, "content": content}),
    )
    .expect("strings always serialize")
}
