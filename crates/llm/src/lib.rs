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
    Status(u16, Box<str>),
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
        on_text: impl FnMut(&str),
    ) -> Result<Completion, Error> {
        let (path, body) = match self.api {
            Api::Chat => ("chat/completions", body),
            Api::Responses => ("responses", responses::body(&body)?),
        };
        let mut req = self
            .http
            .post(format!("{}/{path}", self.base_url))
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
        let mut acc = Acc::new(on_text, self.api);
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
    api: Api,
}

impl<F: FnMut(&str)> Acc<F> {
    fn new(on_text: F, api: Api) -> Self {
        Self {
            sse: sse::Sse::default(),
            on_text,
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
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
        }
    }

    /// Model text, shown as it arrives and kept for the message.
    fn text(&mut self, s: &str) {
        (self.on_text)(s);
        self.text.push_str(s);
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
            Error::Status(code, body) => write!(f, "status {code}: {body}"),
            Error::Api(e) => write!(f, "api: {e}"),
            Error::Json(e) => write!(f, "bad response: {e}"),
            Error::Disconnected => f.write_str("stream ended early"),
        }
    }
}

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

    fn feed_all<'a>(parts: impl IntoIterator<Item = &'a [u8]>, api: Api) -> Out {
        let mut text = String::new();
        let mut acc = Acc::new(|s: &str| text.push_str(s), api);
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
    fn tool_call_deltas_by_index() {
        let data = include_bytes!("../testdata/tools.sse");
        let (text, message, _) = run(data).unwrap();
        assert_eq!(text, "Let me look.");
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"Let me look.","reasoning_content":"Need the file.","tool_calls":[{"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}},{"id":"call_b","type":"function","function":{"name":"glob","arguments":"{}"}}]}"#
        );
        let mut acc = Acc::new(|_: &str| {}, Api::Chat);
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
