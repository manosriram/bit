use crate::skills::Skill;
use serde_json::value::RawValue;

const PROMPT: &str = include_str!("prompt.md");

pub struct Context {
    /// Request head up to and including the system message. Fixed while the model is, so
    /// the provider's prompt cache hits on every turn.
    prefix: String,
    pub(crate) model: String,
    /// The working directory and its AGENTS.md, read once so the prefix stays fixed.
    project: String,
    /// Installed skills, read once at startup so the prefix stays fixed.
    skills: Vec<Skill>,
    pub(crate) cache_key: Option<String>,
    /// The tool schemas: the built-ins, then the plugins' own.
    tools: String,
    /// The model can't call tools, so none are sent: the session is chat only.
    chat_only: bool,
    /// Fields that set the thinking level, from the provider's spec; none for its default.
    thinking: Option<serde_json::Value>,
    /// Input tokens of the last request: how full the window is. 0 until one is sent.
    pub(crate) tokens: u32,
    pub(crate) messages: Vec<Box<RawValue>>,
}

impl Context {
    /// `model` may be empty until the user picks one. `cache_key` is sent as
    /// `prompt_cache_key`, for providers that route their cache by it. `messages` is the
    /// history of a resumed session.
    pub fn new(model: &str, cache_key: Option<&str>, messages: Vec<Box<RawValue>>) -> Self {
        let mut cx = Self {
            prefix: String::new(),
            model: String::new(),
            project: project(),
            skills: Vec::new(),
            cache_key: cache_key.map(str::to_owned),
            tools: tools::SCHEMAS.into(),
            chat_only: false,
            thinking: None,
            tokens: 0,
            messages,
        };
        cx.set_model(model);
        // A crash can cut a turn short; mend it the same way a cancel would.
        // A lone user message is kept: on load it can only be a compaction summary.
        cx.answer_open("error: bit stopped before this call finished");
        cx
    }

    /// Adds installed skills to the prompt. Called once, before the first request, so the
    /// prefix stays fixed for the session.
    pub fn add_skills(&mut self, skills: Vec<Skill>) {
        if skills.is_empty() {
            return;
        }
        self.skills = skills;
        self.set_model(&self.model.clone());
    }

    pub(crate) fn set_model(&mut self, model: &str) {
        let mut head = serde_json::json!({"model": model, "stream": true, "stream_options": {"include_usage": true}});
        if let Some(k) = &self.cache_key {
            head["prompt_cache_key"] = k.as_str().into();
        }
        for (k, v) in self.thinking.iter().flat_map(|t| t.as_object()).flatten() {
            head[k] = v.clone();
        }
        let mut prefix = head.to_string();
        prefix.pop();
        if !self.chat_only {
            prefix.push_str(",\"tools\":");
            prefix.push_str(&self.tools);
        }
        prefix.push_str(",\"messages\":[");
        // Models take their name from the history otherwise, and keep the old one after a switch.
        let mut prompt =
            format!("{PROMPT}{}{}", self.project, crate::skills::section(&self.skills));
        if !model.is_empty() {
            prompt +=
                &format!("\nYour model id is {model}. If asked which model you are, say {model}.");
        }
        if self.chat_only {
            prompt += "\nThis model can't call tools, so you have none in this session: answer from the conversation alone, and ask the user to paste any file or output you need.";
        }
        prefix.push_str(msg("system", &prompt).get());
        (self.prefix, self.model) = (prefix, model.into());
    }

    /// Adds the plugins' tools, comma-separated schemas, after the built-ins. Called once,
    /// before the first request, so the prefix stays fixed for the session.
    pub fn add_tools(&mut self, schemas: &str) {
        if schemas.is_empty() {
            return;
        }
        let base = tools::SCHEMAS.trim_end().strip_suffix(']').unwrap_or(tools::SCHEMAS);
        self.tools = format!("{base},\n{schemas}\n]");
        self.set_model(&self.model.clone());
    }

    /// Sends no tools, for a model that can't call them.
    pub fn set_chat_only(&mut self, on: bool) {
        self.chat_only = on;
        self.set_model(&self.model.clone());
    }

    /// Sets the thinking fields, which are part of the fixed prefix.
    pub(crate) fn set_thinking(&mut self, fields: Option<serde_json::Value>) {
        self.thinking = fields;
        self.set_model(&self.model.clone());
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

/// The working directory, and the project's instructions if it has an AGENTS.md.
fn project() -> String {
    let dir = std::env::current_dir().unwrap_or_default();
    let mut s = format!("\nThe working directory is {}.\n", dir.display());
    if let Ok(md) = std::fs::read_to_string(dir.join("AGENTS.md")) {
        s += &format!(
            "\nFollow the project's instructions from AGENTS.md:\n<agents_md>\n{}\n</agents_md>\n",
            md.trim()
        );
    }
    s
}

pub(crate) fn msg(role: &str, content: &str) -> Box<RawValue> {
    RawValue::from_string(serde_json::json!({"role": role, "content": content}).to_string())
        .expect("json! is valid JSON")
}

pub(crate) fn tool_msg(id: &str, content: &str) -> Box<RawValue> {
    let m = serde_json::json!({"role": "tool", "tool_call_id": id, "content": content});
    RawValue::from_string(m.to_string()).expect("json! is valid JSON")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_chat_only_model_gets_no_tools() {
        let mut cx = super::Context::new("m", None, Vec::new());
        assert!(cx.prefix().contains(r#""tools":"#));
        cx.set_chat_only(true);
        assert!(!cx.prefix().contains(r#""tools":"#), "{}", cx.prefix());
        cx.set_chat_only(false);
        assert!(cx.prefix().contains(r#""tools":"#));
    }
}
