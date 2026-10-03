//! Decoupled LLM protocol provider implementations.
//!
//! Provides protocol-level clients strictly adhering to industry specifications:
//! - [`openai`]: Standard OpenAI Chat Completions protocol (`/chat/completions`).
//! - [`openai_responses`]: Modern OpenAI Responses protocol (`/v1/responses`).
//! - [`anthropic`]: Standard Anthropic Messages protocol (`/v1/messages`).
//!
//! No vendor endpoints or brand-specific APIs are hardcoded.

use std::time::Duration;

/// How long one model request may take, from sending it to the last byte of the answer.
///
/// Both streamed and complete answers must arrive within this time, and a model that
/// writes a long program into a single tool call needs minutes, not seconds. A limit shorter than
/// that fails the same request every time it is asked again. Five minutes still bounds a provider
/// that never answers; `/stop` ends a turn sooner.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// How long connecting to a provider may take, so an unreachable endpoint fails fast instead of
/// using up [`REQUEST_TIMEOUT`].
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub mod anthropic;
pub mod openai;
pub mod openai_responses;
pub mod sse;

pub use anthropic::{AnthropicMessagesProvider, AnthropicProvider};
pub use openai::{OpenAiChatProvider, OpenAiProvider};
pub use openai_responses::OpenAiResponsesProvider;
pub use sse::{SseDecoder, SseEvent};

/// A protocol tool call whose argument JSON can span many SSE events.
#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Converts complete wire arguments once, at the protocol's successful terminal event.
/// Partial or malformed arguments must fail the round rather than execute an empty object.
fn finish_tool_calls(
    calls: std::collections::BTreeMap<usize, PendingToolCall>,
    finish_reason: Option<String>,
) -> Result<crate::gateway::ChatChunk, crate::error::GatewayError> {
    use crate::error::GatewayError;
    let tool_calls = calls
        .into_values()
        .map(|call| {
            if call.id.is_empty() || call.name.is_empty() {
                return Err(GatewayError::InvalidResponse(
                    "streamed tool call has no id or name".into(),
                ));
            }
            let arguments = if call.arguments.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&call.arguments)?
            };
            Ok(crate::gateway::ToolCall {
                id: call.id,
                name: call.name,
                arguments,
            })
        })
        .collect::<Result<Vec<_>, GatewayError>>()?;
    Ok(crate::gateway::ChatChunk {
        tool_calls,
        ..crate::gateway::ChatChunk::done(finish_reason)
    })
}
