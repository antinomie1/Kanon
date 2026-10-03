//! Request layout: every prompt is ordered *static first, dynamic last*.
//!
//! # Why order matters
//! Model providers cache a prompt by its **prefix**: the longest run of leading tokens identical to
//! an earlier request is served from the cache (cheaper and faster); everything from the first
//! differing token onward is recomputed. A request is therefore built in layers, from the one that
//! changes least to the one that changes most:
//!
//! 1. **Tools** — the tool list, in one fixed order, every schema with sorted keys. Changes only
//!    when a plugin, MCP server or skill toggle changes.
//! 2. **System block** — the persona, the skill catalog and the conversation summary, merged into
//!    a single message. Changes when the operator edits configuration or the history is compacted.
//! 3. **History** — earlier turns, append-only. Each request extends the previous one.
//! 4. **The current turn** — the user's message, prefixed with whatever the operator opted to add
//!    (time, sender). Changes every request, so it comes last and only costs its own tokens.
//!
//! # Prefix jitter
//! Two prompts that mean the same can still differ byte for byte, and a single differing byte at
//! the top invalidates the cache for everything after it. This module removes the sources of that:
//! the tool list is sorted, JSON schemas have sorted keys, and system text is trimmed and joined
//! with one fixed separator. Structure is never conditional per turn — a field is either always
//! present or always absent for a given configuration.

use crate::error::AgentError;
use crate::gateway::types::{ChatMessage, ChatRequest, Role, ToolDefinition};

/// Separator placed between the parts of the merged system block.
const SYSTEM_SEPARATOR: &str = "\n\n";

/// Rebuilds a JSON value with every object's keys in sorted order.
///
/// `serde_json` orders keys by insertion when its `preserve_order` feature is on (anything in the
/// dependency tree can enable it), and a schema converted from a protobuf `Struct` arrives in hash
/// order. Sorting explicitly makes the serialized bytes independent of both.
pub fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

/// Puts a tool list into its one canonical form: sorted by name, schemas with sorted keys.
///
/// The order never depends on registration order, host start order or how often a tool is used —
/// any of which would reorder the top of the prompt and defeat the cache.
pub fn canonical_tools(mut tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    for tool in &mut tools {
        tool.parameters = canonical_json(std::mem::take(&mut tool.parameters));
    }
    tools
}

/// The static block the leading system messages of `messages` merge into: parts trimmed,
/// empties dropped, joined with the fixed separator. Empty when there is none.
///
/// This is the system prompt exactly as [`normalize_request`] sends it, for code that must see
/// (or replace) the prompt the provider will receive.
pub fn system_text(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .take_while(|message| message.role == Role::System)
        .filter_map(|message| message.content.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(SYSTEM_SEPARATOR)
}

/// Normalizes a request after every hook has run.
///
/// The leading run of system messages (persona, skill catalog, summary, ...) becomes exactly one
/// message: parts trimmed, empties dropped, joined with a fixed separator. That is the *static
/// block*; it is what a provider's system-prompt cache keys on, and one canonical form means the
/// number of hooks contributing to it can change without reshaping the prompt.
///
/// Tool definitions are canonicalized here, after hooks have finished changing them. A system
/// message after the conversation starts is rejected: providers that extract all system messages
/// would otherwise promote that turn's context into global instructions and invalidate the cache.
pub fn normalize_request(request: &mut ChatRequest) -> Result<(), AgentError> {
    let leading = request
        .messages
        .iter()
        .take_while(|message| message.role == Role::System)
        .count();

    if request.messages[leading..]
        .iter()
        .any(|message| message.role == Role::System)
    {
        return Err(AgentError::InvalidRequest(
            "system messages must precede the conversation; put dynamic context in the user message"
                .into(),
        ));
    }

    request.tools = canonical_tools(std::mem::take(&mut request.tools));
    if leading > 0 {
        let merged = system_text(&request.messages);
        request.messages.drain(..leading);
        if !merged.is_empty() {
            request.messages.insert(0, ChatMessage::system(merged));
        }
    }

    Ok(())
}
