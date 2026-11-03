//! The Responses API adapter, for ChatGPT subscriptions. The history stays in Chat
//! Completions form: the body is translated when sent and the reply is gathered back into a
//! Chat Completions message, so sessions and provider switches don't depend on the wire.

use crate::{Acc, Error, ToolCall};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::borrow::Cow;

/// Translates a Chat Completions request. Same input, same bytes (keys come out sorted), so
/// the cached prefix holds.
pub fn body(chat: &str) -> Result<String, serde_json::Error> {
    let mut c: Value = serde_json::from_str(chat)?;
    let (mut instructions, mut input) = (String::new(), Vec::new());
    for m in c["messages"].as_array_mut().map(std::mem::take).unwrap_or_default() {
        match m["role"].as_str() {
            Some("system") => instructions += m["content"].as_str().unwrap_or(""),
            Some("tool") => input.push(json!({"type": "function_call_output", "call_id": m["tool_call_id"], "output": m["content"]})),
            role => {
                if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) {
                    input.push(json!({"role": role, "content": t}));
                }
                for t in m["tool_calls"].as_array().into_iter().flatten() {
                    let f = &t["function"];
                    input.push(json!({"type": "function_call", "call_id": t["id"], "name": f["name"], "arguments": f["arguments"]}));
                }
            }
        }
    }
    // Responses tools are flat, and strict unless told otherwise; bit's schemas aren't strict.
    let tools: Vec<Value> = (c["tools"].as_array().into_iter().flatten())
        .map(|t| {
            let mut f = t["function"].clone();
            (f["type"], f["strict"]) = ("function".into(), false.into());
            f
        })
        .collect();
    let mut out = json!({"model": c["model"], "instructions": instructions, "input": input, "tools": tools, "stream": true, "store": false});
    if let Some(k) = c.get("prompt_cache_key") {
        out["prompt_cache_key"] = k.clone();
    }
    Ok(out.to_string())
}

/// A Responses event. Tool calls are taken whole from `output_item.done` rather than
/// gathered from deltas, and the stream ends at `response.completed`, not `[DONE]`.
pub fn data<F: FnMut(&str)>(acc: &mut Acc<F>, data: &[u8]) -> Result<(), Error> {
    let e: Event = serde_json::from_slice(data)?;
    let r = e.response;
    match &*e.kind {
        "response.output_text.delta" => {
            acc.text(e.delta.as_deref().unwrap_or(""));
        }
        "response.output_item.done" => {
            if let Some(i) = e.item.filter(|i| i.kind == "function_call") {
                let (id, name) = (i.call_id.unwrap_or_default(), i.name.unwrap_or_default());
                acc.calls.push(ToolCall { id, name, args: i.arguments.unwrap_or_default() });
            }
        }
        // `incomplete` is a reply cut short (output limit); what came is still the reply.
        "response.completed" | "response.incomplete" => {
            acc.usage = r.and_then(|r| r.usage).unwrap_or(acc.usage);
            acc.done = true;
        }
        "response.failed" | "error" => {
            let e = r.and_then(|r| r.error).map_or(data, |e| e.get().as_bytes());
            return Err(Error::Api(String::from_utf8_lossy(e).into()));
        }
        _ => {}
    }
    Ok(())
}

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    delta: Option<Cow<'a, str>>,
    item: Option<Item>,
    #[serde(borrow)]
    response: Option<Response<'a>>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(rename = "type")]
    kind: String,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Response<'a> {
    usage: Option<crate::Usage>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn translates_chat_to_responses() {
        let chat = r#"{"model":"m","stream":true,"stream_options":{"include_usage":true},"prompt_cache_key":"s1","tools":[{"type":"function","function":{"name":"read","description":"d","parameters":{"type":"object"}}}],"messages":[{"role":"system","content":"sys"},{"role":"user","content":"hi"},{"role":"assistant","content":null,"reasoning_content":"r","tool_calls":[{"id":"c1","type":"function","function":{"name":"read","arguments":"{}"}}]},{"role":"tool","tool_call_id":"c1","content":"out"},{"role":"assistant","content":"done"}]}"#;
        assert_eq!(
            super::body(chat).unwrap(),
            r#"{"input":[{"content":"hi","role":"user"},{"arguments":"{}","call_id":"c1","name":"read","type":"function_call"},{"call_id":"c1","output":"out","type":"function_call_output"},{"content":"done","role":"assistant"}],"instructions":"sys","model":"m","prompt_cache_key":"s1","store":false,"stream":true,"tools":[{"description":"d","name":"read","parameters":{"type":"object"},"strict":false,"type":"function"}]}"#
        );
    }
}
