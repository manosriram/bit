mod anthropic;
mod chat;
pub mod models;
mod responses;
mod sse;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::fmt;

pub struct Client {
    http: reqwest::Client,
    base_url: Box<str>,
    api_key: Box<str>,
    headers: Box<[(Box<str>, Box<str>)]>,
    api: Api,
}

/// The wire format a provider speaks. Each has an adapter module that translates the request
/// and parses the stream; the history is always in Chat Completions form.
#[derive(Clone, Copy)]
pub enum Api {
    /// Chat Completions (`chat.rs`): every provider but one.
    Chat,
    /// The Responses API (`responses.rs`): ChatGPT subscriptions.
    Responses,
    /// The Messages API (`anthropic.rs`): Anthropic.
    Anthropic,
}

/// A piece of the reply as it streams in.
#[derive(Debug, PartialEq)]
pub enum Part<'a> {
    Text(&'a str),
    /// The model's reasoning, for providers that send it; never part of the reply's text.
    Thinking(&'a str),
}

pub struct Completion {
    /// The assistant message in wire format, ready to append to the history.
    pub message: Box<RawValue>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Default, Clone)]
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
    /// `None` when the provider didn't report cached tokens.
    pub cached: Option<u32>,
    pub output: u32,
}

#[derive(Debug)]
pub enum Error {
    Http(reqwest::Error),
    /// The status, the body, and the seconds `Retry-After` asked for, if any.
    Status(u16, Box<str>, Option<u64>),
    Api(Box<str>),
    Json(serde_json::Error),
    /// The stream ended before its end marker.
    Disconnected,
}

impl Client {
    /// `headers` are sent with every request, after the auth and content-type headers.
    pub fn new(
        base_url: &str,
        api_key: &str,
        headers: Vec<(Box<str>, Box<str>)>,
        api: Api,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
            headers: headers.into(),
            api,
        }
    }

    /// `body` is a complete Chat Completions request with `"stream": true` and
    /// `"stream_options": {"include_usage": true}`.
    pub async fn stream(
        &self,
        body: String,
        on_part: impl FnMut(Part),
    ) -> Result<Completion, Error> {
        let (path, body) = match self.api {
            Api::Chat => ("chat/completions", chat::body(body)?),
            Api::Responses => ("responses", responses::body(&body)?),
            Api::Anthropic => ("messages", anthropic::body(&body)?),
        };
        let req = self.http.post(format!("{}/{path}", self.base_url));
        let mut req = match self.api {
            Api::Anthropic => {
                req.header("x-api-key", &*self.api_key).header("anthropic-version", "2026-10-21")
            }
            _ => req.bearer_auth(&*self.api_key),
        }
        .header(reqwest::header::CONTENT_TYPE, "application/json");
        for (k, v) in &self.headers {
            req = req.header(&**k, &**v);
        }
        let mut res = req.body(body).send().await?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            let after = res.headers().get("retry-after");
            let after = after.and_then(|v| v.to_str().ok()?.trim().parse().ok());
            return Err(Error::Status(status, res.text().await.unwrap_or_default().into(), after));
        }
        let mut acc = Acc::new(on_part, self.api);
        while let Some(chunk) = res.chunk().await? {
            acc.feed(&chunk)?;
        }
        acc.finish()
    }
}

/// Gathers one streamed reply into a `Completion`.
struct Acc<F> {
    sse: sse::Sse,
    on_part: F,
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    /// Anthropic's reply blocks, in order, to be sent back as they came.
    blocks: Vec<serde_json::Value>,
    usage: Usage,
    done: bool,
    api: Api,
}

impl<F: FnMut(Part)> Acc<F> {
    fn new(on_part: F, api: Api) -> Self {
        Self {
            sse: sse::Sse::default(),
            on_part,
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            blocks: Vec::new(),
            usage: Usage::default(),
            done: false,
            api,
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
        if self.done {
            return Ok(());
        }
        match self.api {
            Api::Chat => chat::data(self, data),
            Api::Responses => responses::data(self, data),
            Api::Anthropic => anthropic::data(self, data),
        }
    }

    /// Model text, shown as it arrives and kept for the message.
    fn text(&mut self, s: &str) {
        (self.on_part)(Part::Text(s));
        self.text.push_str(s);
    }

    /// Reasoning, shown as it arrives and kept for the message's `reasoning_content`.
    fn thinking(&mut self, s: &str) {
        self.show_thinking(s);
        self.reasoning.push_str(s);
    }

    /// Reasoning that is only shown: a summary, or Claude's, which goes back in its blocks.
    fn show_thinking(&mut self, s: &str) {
        if !s.is_empty() {
            (self.on_part)(Part::Thinking(s));
        }
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
            content_blocks: &self.blocks,
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
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    content_blocks: &'a [serde_json::Value],
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

/// OpenAI reports cached tokens in `prompt_tokens_details.cached_tokens`; DeepSeek reports
/// them as `prompt_cache_hit_tokens`. The aliases are the Responses API's names.
#[derive(Deserialize)]
struct WireUsage {
    #[serde(alias = "input_tokens")]
    prompt_tokens: u32,
    #[serde(alias = "output_tokens")]
    completion_tokens: u32,
    prompt_cache_hit_tokens: Option<u32>,
    #[serde(alias = "input_tokens_details")]
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
        Usage { input: w.prompt_tokens, cached, output: w.completion_tokens }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Status(code, body, _) => write!(f, "status {code}: {body}"),
            Error::Api(e) => write!(f, "api: {e}"),
            Error::Json(e) => write!(f, "bad response: {e}"),
            Error::Disconnected => f.write_str("stream ended early"),
        }
    }
}

impl Error {
    /// Rate limits, server errors and dropped connections often pass; anything else won't.
    pub fn retryable(&self) -> bool {
        matches!(self, Error::Status(429 | 500..=599, ..) | Error::Http(_) | Error::Disconnected)
    }

    /// The seconds the server asked to wait before a retry, if it said.
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            Error::Status(_, _, after) => *after,
            _ => None,
        }
    }

    /// Whether the provider refused the request as longer than the model's context window.
    pub fn too_long(&self) -> bool {
        let (Error::Status(400 | 413, b, _) | Error::Api(b)) = self else { return false };
        let b = b.to_ascii_lowercase();
        ["context length", "context_length", "context window", "too long", "too many tokens"]
            .iter()
            .any(|p| b.contains(p))
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

    #[test]
    fn tells_a_context_overflow_from_other_refusals() {
        let status = |code, body: &str| Error::Status(code, body.into(), None);
        assert!(status(400, "This model's maximum context length is 128000 tokens").too_long());
        assert!(status(400, "prompt is too long: 250000 tokens > 200000 maximum").too_long());
        assert!(Error::Api(r#"{"code":"context_length_exceeded"}"#.into()).too_long());
        assert!(!status(400, "invalid tool schema").too_long());
        assert!(!status(500, "context length").too_long());
        assert_eq!(Error::Status(429, "".into(), Some(7)).retry_after(), Some(7));
    }

    type Out = Result<(String, Box<str>, Usage), String>;

    fn feed_all<'a>(parts: impl IntoIterator<Item = &'a [u8]>, api: Api) -> Out {
        let mut text = String::new();
        let mut acc = Acc::new(
            |p: Part| match p {
                Part::Text(s) => text.push_str(s),
                Part::Thinking(s) => text.push_str(&format!("<{s}>")),
            },
            api,
        );
        for p in parts {
            acc.feed(p).map_err(|e| e.to_string())?;
        }
        let c = acc.finish().map_err(|e| e.to_string())?;
        Ok((text, c.message.get().into(), c.usage))
    }

    /// Every two-way split, and a byte-at-a-time feed, must match the unsplit result.
    fn run(data: &[u8]) -> Out {
        run_as(data, Api::Chat)
    }

    fn run_as(data: &[u8], api: Api) -> Out {
        let want = feed_all([data], api);
        for at in 0..=data.len() {
            assert_eq!(feed_all([&data[..at], &data[at..]], api), want, "split at {at}");
        }
        assert_eq!(feed_all(data.chunks(1), api), want, "byte at a time");
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
        assert_eq!(usage, Usage { input: 1200, cached: Some(1024), output: 3 });
    }

    #[test]
    fn deepseek_usage() {
        let (_, _, usage) = run(include_bytes!("../testdata/deepseek.sse")).unwrap();
        assert_eq!(usage, Usage { input: 900, cached: Some(832), output: 2 });
        let none: Usage =
            serde_json::from_str(r#"{"prompt_tokens":5,"completion_tokens":1}"#).unwrap();
        assert_eq!(none.cached, None);
    }

    #[test]
    fn thinking_comes_from_either_field_and_summaries_are_only_shown() {
        // OpenRouter's `reasoning`; `reasoning_content` wins when both come, so it shows once.
        let chat = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"a\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"x\",\"reasoning_content\":\"b\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
        );
        let (text, message, _) = run(chat.as_bytes()).unwrap();
        assert_eq!(text, "<a><b>");
        assert!(message.contains(r#""reasoning_content":"ab""#));

        // A ChatGPT summary is shown, ends its line, and stays out of the message.
        let responses = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Plan**\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        );
        let (text, message, _) = run_as(responses.as_bytes(), Api::Responses).unwrap();
        assert_eq!(text, "<**Plan**><\n>hi");
        assert_eq!(&*message, r#"{"role":"assistant","content":"hi"}"#);
    }

    #[test]
    fn tool_call_deltas_by_index() {
        let data = include_bytes!("../testdata/tools.sse");
        let (text, message, _) = run(data).unwrap();
        // Thinking streams before the text, marked `<...>` by `feed_all`.
        assert_eq!(text, "<Need ><the file.>Let me look.");
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"Let me look.","reasoning_content":"Need the file.","tool_calls":[{"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}},{"id":"call_b","type":"function","function":{"name":"glob","arguments":"{}"}}]}"#
        );
        let mut acc = Acc::new(|_: Part| {}, Api::Chat);
        acc.feed(data).unwrap();
        let calls = acc.finish().unwrap().tool_calls;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn explicit_nulls_are_absent_fields() {
        let data = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\",\"tool_calls\":null,\"reasoning_content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":null}]}\n\n",
            "data: {\"choices\":null,\"usage\":null}\n\n",
            "data: [DONE]\n\n",
        );
        let (text, message, _) = run(data.as_bytes()).unwrap();
        assert_eq!((text.as_str(), &*message), ("hi", r#"{"role":"assistant","content":"hi"}"#));
    }

    #[test]
    fn responses_text_calls_and_usage() {
        let data = include_bytes!("../testdata/responses.sse");
        let (text, message, usage) = run_as(data, Api::Responses).unwrap();
        assert_eq!(text, "Let me look.");
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"Let me look.","tool_calls":[{"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]}"#
        );
        assert_eq!(usage, Usage { input: 124, cached: Some(64), output: 18 });
        let cut = &data[..data.len() - 120];
        assert_eq!(run_as(cut, Api::Responses), Err("stream ended early".into()));
        let failed =
            b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"x\"}}}\n\n";
        assert_eq!(run_as(failed, Api::Responses), Err(r#"api: {"code":"x"}"#.into()));
    }

    #[test]
    fn anthropic_keeps_its_blocks_and_counts_cached_input() {
        let data = include_bytes!("../testdata/anthropic.sse");
        let (text, message, usage) = run_as(data, Api::Anthropic).unwrap();
        assert_eq!(text, "Let me look.");
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"Let me look.","tool_calls":[{"id":"toolu_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}],"content_blocks":[{"signature":"sig==","thinking":"","type":"thinking"},{"text":"Let me look.","type":"text"},{"id":"toolu_a","input":{"path":"a.rs"},"name":"read","type":"tool_use"}]}"#
        );
        assert_eq!(usage, Usage { input: 1120, cached: Some(1000), output: 42 });
        let cut = &data[..data.len() - 40];
        assert_eq!(run_as(cut, Api::Anthropic), Err("stream ended early".into()));
        let refused =
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n";
        assert!(run_as(refused, Api::Anthropic).unwrap_err().contains("refusal"));
        // A Chat Completions server never sees them.
        let body = r#"{"messages":[{"role":"assistant","content":"x","content_blocks":[]}]}"#;
        assert_eq!(
            chat::body(body.into()).unwrap(),
            r#"{"messages":[{"content":"x","role":"assistant"}]}"#
        );
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
