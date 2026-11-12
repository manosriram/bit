//! A local Ollama server, over its OpenAI-compatible API. No key; the models are the ones
//! pulled on the server, read from it at startup.

use super::{Auth, Spec, reasoning_effort};
use std::time::Duration;

pub const SPEC: Spec = Spec {
    name: "ollama",
    base_url: "http://localhost:11434/v1",
    wire: llm::Api::Chat,
    auth: Auth::None,
    cache_key: false,
    headers: |_, _| Vec::new(),
    thinking: reasoning_effort,
};

/// The models `base_url` serves (`GET /models`), each with whether it can call tools; none
/// when it's not running.
pub async fn models(base_url: &str) -> Vec<(String, bool)> {
    let base = base_url.trim_end_matches('/');
    let Some(ids) = json(reqwest::Client::new().get(format!("{base}/models"))).await else {
        return Vec::new();
    };
    let ids = ids["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str());
    // `/api/show` is Ollama's own API, beside the OpenAI-compatible `/v1`.
    let show = format!("{}/api/show", base.trim_end_matches("/v1"));
    let mut models = Vec::new();
    for id in ids {
        let req =
            reqwest::Client::new().post(&show).body(serde_json::json!({ "model": id }).to_string());
        let caps = json(req).await.map(|v| v["capabilities"].clone());
        // An Ollama too old to list capabilities: assume tools and let a turn tell.
        let tools = match caps.as_ref().and_then(|c| c.as_array()) {
            Some(c) => c.iter().any(|c| c == "tools"),
            None => true,
        };
        models.push((id.to_owned(), tools));
    }
    models
}

/// The JSON `req` answers with. A server that's up answers at once; the timeout only
/// bounds a stuck one.
async fn json(req: reqwest::RequestBuilder) -> Option<serde_json::Value> {
    let get = async { req.send().await.ok()?.error_for_status().ok()?.bytes().await.ok() };
    let body = tokio::time::timeout(Duration::from_secs(1), get).await.ok()??;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn tells_which_models_call_tools_and_none_when_down() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut c, _) = l.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = c.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /v1/models") {
                    r#"{"data":[{"id":"qwen3-coder"},{"id":"smollm:135m"},{"id":"old"}]}"#
                } else if req.contains("qwen3-coder") {
                    r#"{"capabilities":["completion","tools"]}"#
                } else if req.contains("smollm") {
                    r#"{"capabilities":["completion"]}"#
                } else {
                    r#"{}"#
                };
                let res =
                    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len());
                c.write_all(res.as_bytes()).await.unwrap();
            }
        });
        let tools = |m: &str, t| (m.to_owned(), t);
        let want = [tools("qwen3-coder", true), tools("smollm:135m", false), tools("old", true)];
        assert_eq!(super::models(&url).await, want);
        assert!(super::models("http://127.0.0.1:1/v1").await.is_empty());
    }
}
