//! The Chat Completions adapter. Requests go out as they are; the stream sends deltas, with
//! tool-call fragments gathered by `index`, and ends at `[DONE]`.

use crate::{Acc, Error, ToolCall, Usage};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;

pub fn data<F: FnMut(&str)>(acc: &mut Acc<F>, data: &[u8]) -> Result<(), Error> {
    if data == b"[DONE]" {
        acc.done = true;
        return Ok(());
    }
    let c: Chunk = serde_json::from_slice(data)?;
    if let Some(e) = c.error {
        return Err(Error::Api(e.get().into()));
    }
    for d in c.choices.into_iter().flatten().filter_map(|c| c.delta) {
        if let Some(s) = d.content.as_deref() {
            acc.text(s);
        }
        acc.reasoning.push_str(d.reasoning_content.as_deref().unwrap_or(""));
        for t in d.tool_calls.into_iter().flatten() {
            if acc.calls.len() <= t.index {
                acc.calls.resize(t.index + 1, ToolCall::default());
            }
            let call = &mut acc.calls[t.index];
            if let Some(id) = t.id.filter(|id| !id.is_empty()) {
                call.id = id.into_owned();
            }
            let f = t.function.unwrap_or_default();
            // Some providers repeat the name in every chunk; it is never split.
            if let Some(name) = f.name.filter(|_| call.name.is_empty()) {
                call.name = name.into_owned();
            }
            call.args.push_str(f.arguments.as_deref().unwrap_or(""));
        }
    }
    acc.usage = c.usage.unwrap_or(acc.usage);
    Ok(())
}

// Optional fields are `Option` rather than `#[serde(default)]`: some providers send an explicit
// `null` (`"tool_calls":null`), which `default` alone rejects.
#[derive(Deserialize)]
struct Chunk<'a> {
    #[serde(borrow)]
    choices: Option<Vec<Choice<'a>>>,
    usage: Option<Usage>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Choice<'a> {
    #[serde(borrow)]
    delta: Option<Delta<'a>>,
}

#[derive(Deserialize, Default)]
struct Delta<'a> {
    // Borrowed unless the text has JSON escapes, so most deltas are not copied.
    #[serde(borrow)]
    content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    reasoning_content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool_calls: Option<Vec<CallDelta<'a>>>,
}

#[derive(Deserialize)]
struct CallDelta<'a> {
    #[serde(default)]
    index: usize,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    function: Option<FnDelta<'a>>,
}

#[derive(Deserialize, Default)]
struct FnDelta<'a> {
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    arguments: Option<Cow<'a, str>>,
}
