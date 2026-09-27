//! Recovery of tool calls a model emitted as plain text instead of structured calls.
//!
//! # Why this exists
//! Not every OpenAI-compatible endpoint implements native function calling: several models answer
//! a request that carried `tools` with markup in the message body instead of a `tool_calls` array —
//! for example
//!
//! ```text
//! <tool_call><function=mai_play_score><parameter=output_format>image</parameter>
//! <parameter=qq>1705702687</parameter></function></tool_call>
//! ```
//!
//! Without recovery the pipeline treats that markup as the assistant's answer, sends it to the chat
//! platform verbatim, and never executes the tool — the user sees a broken XML blob and the turn
//! ends. This module turns such markup back into real [`ToolCall`] values so the normal tool loop
//! runs, while preserving any surrounding prose as the visible answer.
//!
//! # Supported shapes
//! - `<tool_call>{ "name": ..., "arguments": {...} }</tool_call>` (JSON body, Hermes-style);
//! - `<tool_call><function=NAME><parameter=KEY>VALUE</parameter></function></tool_call>`;
//! - the same with `<function name="NAME">` / `<parameter name="KEY">` attribute syntax;
//! - several blocks in one message, optionally wrapped in `<tool_calls>`.
//!
//! Anything that does not parse is left untouched: swallowing a malformed block would replace a
//! visibly broken answer with no answer at all, which is strictly worse for the operator.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::gateway::types::ToolCall;

/// Monotonic counter used to allocate ids for recovered calls.
///
/// Ids only have to be unique within a conversation so the assistant/tool pair can be matched;
/// a process-wide counter guarantees that across every session without carrying extra state.
static TEXT_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Extracts textual tool calls from a model completion.
///
/// Returns the recovered calls and the completion with the parsed blocks removed. When nothing
/// parses, the calls are empty and the content is returned unchanged.
pub fn extract_textual_tool_calls(content: &str) -> (Vec<ToolCall>, String) {
    if !looks_like_tool_call_markup(content) {
        return (Vec::new(), content.to_string());
    }

    let mut calls = Vec::new();
    let mut cleaned = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(open_start) = find_tag_start(&content[cursor..], "tool_call") {
        let open_start = cursor + open_start;
        let Some(open_end) = content[open_start..]
            .find('>')
            .map(|offset| open_start + offset)
        else {
            break;
        };
        let body_start = open_end + 1;
        let Some(close_start) = content[body_start..]
            .find("</tool_call>")
            .map(|o| body_start + o)
        else {
            break;
        };

        let body = &content[body_start..close_start];
        match parse_tool_call_body(body) {
            Some(parsed) => {
                cleaned.push_str(&content[cursor..open_start]);
                calls.extend(parsed);
                cursor = close_start + "</tool_call>".len();
            }
            None => {
                // Leave the unparsable block in place and keep scanning after it.
                cleaned.push_str(&content[cursor..close_start + "</tool_call>".len()]);
                cursor = close_start + "</tool_call>".len();
            }
        }
    }

    if calls.is_empty() {
        return (Vec::new(), content.to_string());
    }

    cleaned.push_str(&content[cursor..]);
    (calls, cleaned.trim().to_string())
}

/// Why this module returns `(calls, cleaned)`:
///
/// Recovering calls must not change the answer when there was nothing to recover, so the fast path
/// rejects content without the marker before allocating anything.
fn looks_like_tool_call_markup(content: &str) -> bool {
    content.contains("<tool_call>")
        || content.contains("<tool_call ")
        || content.contains("<function=")
}

/// Finds the byte offset of an opening `<tool_call>` tag, tolerating attributes.
fn find_tag_start(content: &str, tag: &str) -> Option<usize> {
    let plain = format!("<{tag}>");
    if let Some(index) = content.find(&plain) {
        return Some(index);
    }
    content.find(&format!("<{tag} "))
}

/// Parses the body of one `<tool_call>` block into zero or more tool calls.
fn parse_tool_call_body(body: &str) -> Option<Vec<ToolCall>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return parse_json_body(trimmed);
    }

    parse_function_body(trimmed)
}

/// Parses a JSON tool-call body in any of the shapes models emit.
fn parse_json_body(body: &str) -> Option<Vec<ToolCall>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let entries = match value {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    };

    let mut calls = Vec::new();
    for entry in entries {
        if let Some(call) = json_entry_to_call(&entry) {
            calls.push(call);
        }
    }

    if calls.is_empty() { None } else { Some(calls) }
}

/// Converts one JSON object into a tool call, accepting the common key spellings.
fn json_entry_to_call(entry: &serde_json::Value) -> Option<ToolCall> {
    // `{"function": {"name": ..., "arguments": ...}}` (OpenAI-shaped body)
    let source = entry.get("function").unwrap_or(entry);

    let name = source
        .get("name")
        .and_then(|n| n.as_str())
        .or_else(|| entry.get("name").and_then(|n| n.as_str()))?
        .trim();
    if name.is_empty() {
        return None;
    }

    let arguments = source
        .get("arguments")
        .or_else(|| source.get("parameters"))
        .or_else(|| source.get("input"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    Some(ToolCall {
        id: next_call_id(),
        name: name.to_string(),
        arguments: normalize_arguments(arguments),
    })
}

/// Accepts arguments as an object or as a JSON-encoded string, always yielding an object.
fn normalize_arguments(arguments: serde_json::Value) -> serde_json::Value {
    match arguments {
        serde_json::Value::String(encoded) => serde_json::from_str(&encoded)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
        other => other,
    }
}

/// Parses `<function=NAME><parameter=KEY>VALUE</parameter>...</function>` bodies.
fn parse_function_body(body: &str) -> Option<Vec<ToolCall>> {
    // A body may contain several function blocks; each becomes its own call.
    let mut calls = Vec::new();
    let mut cursor = 0usize;

    while let Some(relative) = body[cursor..].find("<function") {
        let start = cursor + relative;
        let Some((name, after_open)) = parse_function_tag(&body[start..]) else {
            // A malformed block must not discard the calls recovered before it.
            break;
        };
        let params_start = start + after_open;
        // Parameters run until the matching `</function>` or the next `<function` block.
        let remainder = &body[params_start..];
        let end = remainder
            .find("</function>")
            .or_else(|| remainder.find("<function"))
            .unwrap_or(remainder.len());
        let parameters = parse_parameters(&remainder[..end]);

        if !name.is_empty() {
            calls.push(ToolCall {
                id: next_call_id(),
                name,
                arguments: serde_json::Value::Object(parameters),
            });
        }
        cursor = params_start + end;
    }

    if calls.is_empty() { None } else { Some(calls) }
}

/// Reads the function name from `<function=NAME>`, `<function name="NAME">` or `<function>NAME</function>`.
///
/// Returns the name and the byte offset just past the opening tag(s), relative to `text`. The
/// offset is what lets the caller parse parameters from exactly after the tag instead of from a
/// guessed position.
fn parse_function_tag(text: &str) -> Option<(String, usize)> {
    const OPEN: &str = "<function";
    let after_tag = text.strip_prefix(OPEN)?;

    if let Some(rest) = after_tag.strip_prefix(" name=") {
        let end = rest.find('>')?;
        return Some((
            unquote(rest[..end].trim()),
            OPEN.len() + " name=".len() + end + 1,
        ));
    }
    if let Some(rest) = after_tag.strip_prefix('=') {
        let end = rest.find('>')?;
        return Some((unquote(rest[..end].trim()), OPEN.len() + 1 + end + 1));
    }
    if let Some(rest) = after_tag.strip_prefix('>') {
        let end = rest.find("</function>")?;
        return Some((
            rest[..end].trim().to_string(),
            OPEN.len() + 1 + end + "</function>".len(),
        ));
    }

    None
}

/// Parses every `<parameter=KEY>VALUE</parameter>` / `<parameter name="KEY">VALUE</parameter>` pair.
fn parse_parameters(text: &str) -> serde_json::Map<String, serde_json::Value> {
    const OPEN: &str = "<parameter";
    let mut arguments = serde_json::Map::new();
    let mut cursor = 0usize;

    while let Some(relative) = text[cursor..].find(OPEN) {
        let start = cursor + relative;
        let after_tag = &text[start + OPEN.len()..];

        // `value_start` is the byte offset of the value relative to `text`, computed from the exact
        // prefix that was consumed. Deriving it (instead of re-adding a guessed tag length) is what
        // keeps the first character of every value from being eaten.
        let (key, value_start) = if let Some(rest) = after_tag.strip_prefix(" name=") {
            let Some(end) = rest.find('>') else { break };
            (
                unquote(rest[..end].trim()),
                start + OPEN.len() + " name=".len() + end + 1,
            )
        } else if let Some(rest) = after_tag.strip_prefix('=') {
            let Some(end) = rest.find('>') else { break };
            (
                unquote(rest[..end].trim()),
                start + OPEN.len() + 1 + end + 1,
            )
        } else {
            break;
        };

        let remainder = &text[value_start..];
        let end = match remainder.find("</parameter>") {
            Some(end) => end,
            // A parameter without a closing tag consumes the rest of the block; that is the only
            // reading that does not silently drop the model's final argument.
            None => remainder.len(),
        };
        let value = &remainder[..end];

        if !key.is_empty() {
            arguments.insert(key, coerce_scalar(value.trim()));
        }
        cursor = value_start + end;
    }

    arguments
}

/// Converts a textual parameter value into the JSON type it obviously represents.
///
/// Tool schemas expect numbers, booleans and nested objects, and a model writing
/// `<parameter=count>3</parameter>` means the number 3. A value that is not valid JSON stays a
/// string, which is what free-form arguments need.
fn coerce_scalar(value: &str) -> serde_json::Value {
    if value.is_empty() {
        return serde_json::Value::String(String::new());
    }
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_string()))
}

/// Strips matching surrounding quotes from a tag attribute value.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    unquoted.to_string()
}

/// Allocates the next recovered-call identifier.
fn next_call_id() -> String {
    format!(
        "textcall_{}",
        TEXT_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
