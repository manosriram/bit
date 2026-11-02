pub mod models;
mod sse;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::fmt;

pub struct Client {
    http: reqwest::Client,
    base_url: Box<str>,
    api_key: Box<str>,
    headers: Box<[(Box<str>, Box<str>)]>,
}

#[derive(Debug)]
pub struct Completion {
    /// The assistant message in wire format, ready to append to the history.
    pub message: Box<RawValue>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON object text; `{}` when the model sent none.
    pub args: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(from = "WireUsage")]
pub struct Usage {
    pub input: u32,
    pub cached: u32,
    pub output: u32,
}

#[derive(Debug)]
pub enum Error {
    Http(reqwest::Error),
    Status(u16, Box<str>),
    Api(Box<str>),
    Json(serde_json::Error),
    /// The stream ended before `[DONE]`.
    Disconnected,
}

impl Client {
    /// `headers` are sent with every request, after the auth and content-type headers.
    pub fn new(base_url: &str, api_key: &str, headers: Vec<(Box<str>, Box<str>)>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
            headers: headers.into(),
        }
    }

    /// `body` is a complete Chat Completions request with `"stream": true` and
    /// `"stream_options": {"include_usage": true}`.
    pub async fn stream(
        &self,
        body: String,
        on_text: impl FnMut(&str),
    ) -> Result<Completion, Error> {
        let mut req = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&*self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        for (k, v) in &self.headers {
            req = req.header(&**k, &**v);
        }
        let mut res = req.body(body).send().await?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            return Err(Error::Status(status, res.text().await.unwrap_or_default().into()));
        }
        let mut acc = Acc::new(on_text);
        while let Some(chunk) = res.chunk().await? {
            acc.feed(&chunk)?;
        }
        acc.finish()
    }
}

/// Gathers one streamed reply into a `Completion`.
struct Acc<F> {
    sse: sse::Sse,
    on_text: F,
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    usage: Usage,
    done: bool,
}

impl<F: FnMut(&str)> Acc<F> {
    fn new(on_text: F) -> Self {
        Self {
            sse: sse::Sse::default(),
            on_text,
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            usage: Usage::default(),
            done: false,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), Error> {
        // Taken out for the call, so `data` can borrow the rest of `self`.
        let mut sse = std::mem::take(&mut self.sse);
        let r = sse.feed(chunk, |d| self.data(d));
        self.sse = sse;
        r
    }

    fn data(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.done || data == b"[DONE]" {
            self.done = true;
            return Ok(());
        }
        let c: Chunk = serde_json::from_slice(data)?;
        if let Some(e) = c.error {
            return Err(Error::Api(e.get().into()));
        }
        for d in c.choices.into_iter().map(|c| c.delta) {
            if let Some(s) = d.content.as_deref() {
                (self.on_text)(s);
                self.text.push_str(s);
            }
            self.reasoning.push_str(d.reasoning_content.as_deref().unwrap_or(""));
            for t in d.tool_calls {
                if self.calls.len() <= t.index {
                    self.calls.resize(t.index + 1, ToolCall::default());
                }
                let call = &mut self.calls[t.index];
                if let Some(id) = t.id.filter(|id| !id.is_empty()) {
                    call.id = id.into_owned();
                }
                let f = t.function;
                // Some providers repeat the name in every chunk; it is never split.
                if let Some(name) = f.name.filter(|_| call.name.is_empty()) {
                    call.name = name.into_owned();
                }
                call.args.push_str(f.arguments.as_deref().unwrap_or(""));
            }
        }
        self.usage = c.usage.unwrap_or(self.usage);
        Ok(())
    }

    fn finish(mut self) -> Result<Completion, Error> {
        if !self.done {
            return Err(Error::Disconnected);
        }
        for c in &mut self.calls {
            if c.args.trim().is_empty() {
                c.args = "{}".into();
            }
        }
        let has_calls = !self.calls.is_empty();
        let message = serde_json::value::to_raw_value(&Message {
            role: "assistant",
            content: (!self.text.is_empty() || !has_calls).then_some(&*self.text),
            reasoning_content: (has_calls && !self.reasoning.is_empty())
                .then_some(&*self.reasoning),
            tool_calls: self
                .calls
                .iter()
                .map(|c| WireCall {
                    id: &c.id,
                    kind: "function",
                    function: WireFn { name: &c.name, arguments: &c.args },
                })
                .collect(),
        })?;
        Ok(Completion { message, tool_calls: self.calls, usage: self.usage })
    }
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireCall<'a>>,
}

#[derive(Serialize)]
struct WireCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    function: WireFn<'a>,
}

#[derive(Serialize)]
struct WireFn<'a> {
    name: &'a str,
    arguments: &'a str,
}

#[derive(Deserialize)]
struct Chunk<'a> {
    #[serde(default, borrow)]
    choices: Vec<Choice<'a>>,
    usage: Option<Usage>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Choice<'a> {
    #[serde(default, borrow)]
    delta: Delta<'a>,
}

#[derive(Deserialize, Default)]
struct Delta<'a> {
    // Borrowed unless the text has JSON escapes, so most deltas are not copied.
    #[serde(borrow)]
    content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    reasoning_content: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    tool_calls: Vec<CallDelta<'a>>,
}

#[derive(Deserialize)]
struct CallDelta<'a> {
    #[serde(default)]
    index: usize,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    function: FnDelta<'a>,
}

#[derive(Deserialize, Default)]
struct FnDelta<'a> {
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    arguments: Option<Cow<'a, str>>,
}

/// OpenAI reports cached tokens in `prompt_tokens_details.cached_tokens`; DeepSeek reports
/// them as `prompt_cache_hit_tokens`.
#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    prompt_cache_hit_tokens: Option<u32>,
    prompt_tokens_details: Option<Details>,
}

#[derive(Deserialize)]
struct Details {
    cached_tokens: Option<u32>,
}

impl From<WireUsage> for Usage {
    fn from(w: WireUsage) -> Self {
        let cached =
            w.prompt_cache_hit_tokens.or(w.prompt_tokens_details.and_then(|d| d.cached_tokens));
        Usage { input: w.prompt_tokens, cached: cached.unwrap_or(0), output: w.completion_tokens }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Status(code, body) => write!(f, "status {code}: {body}"),
            Error::Api(e) => write!(f, "api: {e}"),
            Error::Json(e) => write!(f, "bad response: {e}"),
            Error::Disconnected => f.write_str("stream ended early"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Rate limits, server errors and dropped connections often pass; anything else won't.
    pub fn retryable(&self) -> bool {
        matches!(self, Error::Status(429 | 500..=599, _) | Error::Http(_) | Error::Disconnected)
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Out = Result<(String, Box<str>, Usage), String>;

    fn feed_all<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Out {
        let mut text = String::new();
        let mut acc = Acc::new(|s: &str| text.push_str(s));
        for p in parts {
            acc.feed(p).map_err(|e| e.to_string())?;
        }
        let c = acc.finish().map_err(|e| e.to_string())?;
        Ok((text, c.message.get().into(), c.usage))
    }

    /// Every two-way split, and a byte-at-a-time feed, must match the unsplit result.
    fn run(data: &[u8]) -> Out {
        let want = feed_all([data]);
        for at in 0..=data.len() {
            assert_eq!(feed_all([&data[..at], &data[at..]]), want, "split at {at}");
        }
        assert_eq!(feed_all(data.chunks(1)), want, "byte at a time");
        want
    }

    #[test]
    fn utf8_split() {
        let (text, message, _) = run(include_bytes!("../testdata/utf8.sse")).unwrap();
        assert_eq!(text, "héllo 世界 🦀\n\"ok\"");
        assert_eq!(&*message, r#"{"role":"assistant","content":"héllo 世界 🦀\n\"ok\""}"#);
    }

    #[test]
    fn openai_usage_only_final_chunk() {
        let (text, _, usage) = run(include_bytes!("../testdata/openai.sse")).unwrap();
        assert_eq!(text, "Hi there");
        assert_eq!(usage, Usage { input: 1200, cached: 1024, output: 3 });
    }

    #[test]
    fn deepseek_usage() {
        let (_, _, usage) = run(include_bytes!("../testdata/deepseek.sse")).unwrap();
        assert_eq!(usage, Usage { input: 900, cached: 832, output: 2 });
    }

    #[test]
    fn tool_call_deltas_by_index() {
        let data = include_bytes!("../testdata/tools.sse");
        let (text, message, _) = run(data).unwrap();
        assert_eq!(text, "Let me look.");
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"Let me look.","reasoning_content":"Need the file.","tool_calls":[{"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}},{"id":"call_b","type":"function","function":{"name":"glob","arguments":"{}"}}]}"#
        );
        let mut acc = Acc::new(|_: &str| {});
        acc.feed(data).unwrap();
        let calls = acc.finish().unwrap().tool_calls;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn disconnect() {
        assert_eq!(
            run(include_bytes!("../testdata/disconnect.sse")),
            Err("stream ended early".into())
        );
    }

    #[test]
    fn api_error() {
        assert!(run(include_bytes!("../testdata/error.sse")).unwrap_err().starts_with("api: "));
    }
}
