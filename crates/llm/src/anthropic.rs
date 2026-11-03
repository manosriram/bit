//! The Messages API adapter, for Anthropic. Like `responses.rs`, the body is translated
//! when sent and the reply gathered back into a Chat Completions message. Claude's own reply
//! blocks (thinking with its signature, text, tool use, in order) are kept as
//! `content_blocks` on that message and sent back verbatim: the API rejects thinking that
//! was dropped, reordered or edited.

use crate::{Acc, Error, ToolCall, Usage};
use serde::Deserialize;
use serde_json::{Value, json};

/// The output cap. Every current Claude model allows at least this much, and streaming
/// keeps a long reply from timing out.
const MAX_TOKENS: u32 = 64_000;

/// Translates a Chat Completions request. Same input, same bytes (keys come out sorted), so
/// the cached prefix holds.
pub fn body(chat: &str) -> Result<String, serde_json::Error> {
    let mut c: Value = serde_json::from_str(chat)?;
    let (mut system, mut out) = (String::new(), Vec::<Value>::new());
    for mut m in c["messages"].as_array_mut().map(std::mem::take).unwrap_or_default() {
        let kept = m["content_blocks"].take();
        let (role, blocks) = match (m["role"].as_str(), kept) {
            (Some("system"), _) => {
                system += m["content"].as_str().unwrap_or("");
                continue;
            }
            (Some("tool"), _) => (
                "user",
                vec![
                    json!({"type": "tool_result", "tool_use_id": m["tool_call_id"], "content": m["content"]}),
                ],
            ),
            (Some("assistant"), Value::Array(b)) => ("assistant", b),
            (role, _) => {
                let mut b = Vec::new();
                if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) {
                    b.push(json!({"type": "text", "text": t}));
                }
                for t in m["tool_calls"].as_array().into_iter().flatten() {
                    let f = &t["function"];
                    let input = serde_json::from_str(f["arguments"].as_str().unwrap_or(""));
                    let input: Value = input.unwrap_or_else(|_| json!({}));
                    b.push(json!({"type": "tool_use", "id": t["id"], "name": f["name"], "input": input}));
                }
                (if role == Some("assistant") { "assistant" } else { "user" }, b)
            }
        };
        if blocks.is_empty() {
            continue;
        }
        // Roles must alternate: a call's results, and a prompt typed after them, are one turn.
        match out.last_mut().filter(|l| l["role"] == role) {
            Some(l) => {
                if let Some(c) = l["content"].as_array_mut() {
                    c.extend(blocks);
                }
            }
            None => out.push(json!({"role": role, "content": blocks})),
        }
    }
    let tools: Vec<Value> = (c["tools"].as_array().into_iter().flatten())
        .map(|t| {
            let f = &t["function"];
            json!({"name": f["name"], "description": f["description"], "input_schema": f["parameters"]})
        })
        .collect();
    // One marker after the fixed tools and system prompt, and the automatic one, which moves
    // to the end of the conversation on every request.
    let mut out = json!({"model": c["model"], "max_tokens": MAX_TOKENS, "stream": true, "tools": tools, "messages": out, "cache_control": {"type": "ephemeral"}});
    if !system.is_empty() {
        out["system"] =
            json!([{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]);
    }
    Ok(out.to_string())
}

/// A Messages stream event. Blocks arrive one after another, so a delta belongs to the last
/// block started.
pub fn data<F: FnMut(&str)>(acc: &mut Acc<F>, data: &[u8]) -> Result<(), Error> {
    let e: Event = serde_json::from_slice(data)?;
    match &*e.kind {
        "message_start" => acc.usage = usage(e.message.and_then(|m| m.usage), acc.usage),
        "content_block_start" => {
            let b = e.content_block.unwrap_or_default();
            if b["type"] == "tool_use" {
                let s = |k: &str| b[k].as_str().unwrap_or("").to_owned();
                acc.calls.push(ToolCall { id: s("id"), name: s("name"), args: String::new() });
            }
            acc.blocks.push(b);
        }
        "content_block_delta" => {
            let d = e.delta.unwrap_or_default();
            let s = |k: &str| d[k].as_str().unwrap_or("");
            let append = |b: Option<&mut Value>, k: &str, s: &str| {
                if let Some(Value::String(t)) = b.and_then(|b| b.get_mut(k)) {
                    t.push_str(s);
                }
            };
            match d["type"].as_str() {
                Some("text_delta") => {
                    acc.text(s("text"));
                    append(acc.blocks.last_mut(), "text", s("text"));
                }
                Some("thinking_delta") => append(acc.blocks.last_mut(), "thinking", s("thinking")),
                Some("signature_delta") => {
                    append(acc.blocks.last_mut(), "signature", s("signature"))
                }
                Some("input_json_delta") => {
                    if let Some(c) = acc.calls.last_mut() {
                        c.args.push_str(s("partial_json"));
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            if let Some(b) = acc.blocks.last_mut().filter(|b| b["type"] == "tool_use") {
                let args = acc.calls.last().map_or("", |c| &c.args);
                b["input"] = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
            }
        }
        "message_delta" => {
            if e.delta.as_ref().is_some_and(|d| d["stop_reason"] == "refusal") {
                return Err(Error::Api("the model declined to answer (refusal)".into()));
            }
            acc.usage = usage(e.usage, acc.usage);
        }
        "message_stop" => acc.done = true,
        "error" => return Err(Error::Api(e.error.unwrap_or_default().to_string().into())),
        _ => {}
    }
    Ok(())
}

/// Anthropic counts cached tokens apart from `input_tokens`; bit's input is all of them. A
/// later event may repeat only some fields, so the rest stay as they were.
fn usage(w: Option<WireUsage>, was: Usage) -> Usage {
    let Some(w) = w else { return was };
    let read = w.cache_read_input_tokens.unwrap_or(0);
    let input = w.input_tokens.map(|i| i + w.cache_creation_input_tokens.unwrap_or(0) + read);
    Usage {
        input: input.unwrap_or(was.input),
        cached: w.cache_read_input_tokens.or(was.cached),
        output: w.output_tokens.unwrap_or(was.output),
    }
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    message: Option<Start>,
    content_block: Option<Value>,
    delta: Option<Value>,
    usage: Option<WireUsage>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Start {
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn translates_chat_to_messages() {
        let chat = r#"{"model":"m","stream":true,"tools":[{"type":"function","function":{"name":"read","description":"d","parameters":{"type":"object"}}}],"messages":[{"role":"system","content":"sys"},{"role":"user","content":"hi"},{"role":"assistant","content":null,"reasoning_content":"r","tool_calls":[{"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},{"id":"c2","type":"function","function":{"name":"read","arguments":""}}]},{"role":"tool","tool_call_id":"c1","content":"out1"},{"role":"tool","tool_call_id":"c2","content":"out2"},{"role":"user","content":"and?"},{"role":"assistant","content":"done","content_blocks":[{"type":"thinking","thinking":"","signature":"s"},{"type":"text","text":"done"}]}]}"#;
        assert_eq!(
            super::body(chat).unwrap(),
            r#"{"cache_control":{"type":"ephemeral"},"max_tokens":64000,"messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"},{"content":[{"id":"c1","input":{"path":"a"},"name":"read","type":"tool_use"},{"id":"c2","input":{},"name":"read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"out1","tool_use_id":"c1","type":"tool_result"},{"content":"out2","tool_use_id":"c2","type":"tool_result"},{"text":"and?","type":"text"}],"role":"user"},{"content":[{"signature":"s","thinking":"","type":"thinking"},{"text":"done","type":"text"}],"role":"assistant"}],"model":"m","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"sys","type":"text"}],"tools":[{"description":"d","input_schema":{"type":"object"},"name":"read"}]}"#
        );
    }
}
