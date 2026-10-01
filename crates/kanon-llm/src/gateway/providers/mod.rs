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
/// The agent does not stream, so a whole answer has to arrive within this time, and a model that
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
