use serde_json::value::RawValue;

const PROMPT: &str = include_str!("prompt.md");

pub struct Context {
    /// Request head up to and including the system message. Fixed for the session, so the
    /// provider's prompt cache hits on every turn.
    prefix: String,
    pub(crate) messages: Vec<Box<RawValue>>,
}

impl Context {
    pub fn new(model: &str) -> Self {
        let head = serde_json::json!({"model": model, "stream": true, "stream_options": {"include_usage": true}});
        let mut prefix = head.to_string();
        prefix.pop();
        prefix.push_str(",\"tools\":");
        prefix.push_str(tools::SCHEMAS);
        prefix.push_str(",\"messages\":[");
        prefix.push_str(msg("system", PROMPT).get());
        Self {
            prefix,
            messages: Vec::new(),
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
