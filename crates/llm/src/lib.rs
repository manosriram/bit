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
}

#[derive(Debug)]
pub struct Completion {
    /// The assistant message in wire format, ready to append to the history.
    pub message: Box<RawValue>,
    pub usage: Usage,
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
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
        }
    }

    /// `body` is a complete Chat Completions request with `"stream": true` and
    /// `"stream_options": {"include_usage": true}`.
    pub async fn stream(
        &self,
        body: String,
        on_text: impl FnMut(&str),
    ) -> Result<Completion, Error> {
        let mut res = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&*self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            return Err(Error::Status(
                status,
                res.text().await.unwrap_or_default().into(),
            ));
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
    usage: Usage,
    done: bool,
}

impl<F: FnMut(&str)> Acc<F> {
    fn new(on_text: F) -> Self {
        Self {
            sse: sse::Sse::default(),
            on_text,
            text: String::new(),
            usage: Usage::default(),
            done: false,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), Error> {
        let Self {
            sse,
            on_text,
            text,
            usage,
            done,
        } = self;
        sse.feed(chunk, |data| {
            if *done {
                return Ok(());
            }
            if data == b"[DONE]" {
                *done = true;
                return Ok(());
            }
            let c: Chunk = serde_json::from_slice(data)?;
            if let Some(e) = c.error {
                return Err(Error::Api(e.get().into()));
            }
            for s in c.choices.iter().filter_map(|c| c.delta.content.as_deref()) {
                on_text(s);
                text.push_str(s);
            }
            if let Some(u) = c.usage {
                *usage = u;
            }
            Ok(())
        })
    }

    fn finish(self) -> Result<Completion, Error> {
        if !self.done {
            return Err(Error::Disconnected);
        }
        let message = serde_json::value::to_raw_value(&Message {
            role: "assistant",
            content: &self.text,
        })?;
        Ok(Completion {
            message,
            usage: self.usage,
        })
    }
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
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
    #[serde(borrow)]
    delta: Delta<'a>,
}

#[derive(Deserialize)]
struct Delta<'a> {
    // Borrowed unless the text has JSON escapes, so most deltas are not copied.
    #[serde(borrow)]
    content: Option<Cow<'a, str>>,
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
        let cached = w
            .prompt_cache_hit_tokens
            .or(w.prompt_tokens_details.and_then(|d| d.cached_tokens));
        Usage {
            input: w.prompt_tokens,
            cached: cached.unwrap_or(0),
            output: w.completion_tokens,
        }
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
        assert_eq!(
            &*message,
            r#"{"role":"assistant","content":"héllo 世界 🦀\n\"ok\""}"#
        );
    }

    #[test]
    fn openai_usage_only_final_chunk() {
        let (text, _, usage) = run(include_bytes!("../testdata/openai.sse")).unwrap();
        assert_eq!(text, "Hi there");
        assert_eq!(
            usage,
            Usage {
                input: 1200,
                cached: 1024,
                output: 3
            }
        );
    }

    #[test]
    fn deepseek_usage() {
        let (_, _, usage) = run(include_bytes!("../testdata/deepseek.sse")).unwrap();
        assert_eq!(
            usage,
            Usage {
                input: 900,
                cached: 832,
                output: 2
            }
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
        assert!(
            run(include_bytes!("../testdata/error.sse"))
                .unwrap_err()
                .starts_with("api: ")
        );
    }

    #[test]
    fn cost() {
        let m = models::find("deepseek-chat").unwrap();
        let c = m.cost(&Usage {
            input: 1_000_000,
            cached: 500_000,
            output: 1_000_000,
        });
        assert!((c - (0.14 + 0.014 + 0.42)).abs() < 1e-9);
    }
}
