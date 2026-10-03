//! Error types for the Kanon LLM module.
//!
//! Provides granular errors for model gateway transport/API failures
//! and tool calling routing/execution failures.

use thiserror::Error;

/// Errors arising during LLM gateway requests and provider communication.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// HTTP transport or network failure.
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Serialization or JSON parsing failure.
    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// API returned a non-success HTTP status code.
    #[error("Provider API error (status {status}): {message}")]
    ApiStatus {
        /// HTTP status code returned by the provider endpoint.
        status: u16,
        /// Error message or body extracted from provider response.
        message: String,
    },

    /// Provider returned an empty or malformed payload.
    #[error("Invalid or empty response from model provider: {0}")]
    InvalidResponse(String),
}

/// Errors arising during tool routing and state machine loop execution.
#[derive(Debug, Error)]
pub enum ToolRouterError {
    /// Error originating from the underlying LLM gateway.
    #[error("LLM gateway failure: {0}")]
    Gateway(#[from] GatewayError),

    /// gRPC RPC status error returned by the plugin host.
    #[error("Tool execution gRPC failure: {0}")]
    Rpc(Box<tonic::Status>),

    /// Requested tool name was not declared by any active plugin.
    #[error("Tool '{0}' is not registered on any active plugin host")]
    ToolNotFound(String),

    /// The turn was stopped through its [`crate::StopSignal`] before it finished.
    #[error("the turn was stopped before it finished")]
    Stopped,
}

impl From<tonic::Status> for ToolRouterError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

/// Errors arising during general agent execution and reasoning loops.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Another turn or mutation owns the session writer.
    #[error("session '{0}' is busy")]
    Busy(String),
    /// Failure originating from the underlying model gateway.
    #[error("Agent LLM gateway failure: {0}")]
    Gateway(#[from] GatewayError),

    /// gRPC RPC status error returned by the plugin host.
    #[error("Agent tool execution RPC failure: {0}")]
    Rpc(Box<tonic::Status>),

    /// Requested tool name was not declared by any active plugin.
    #[error("Tool '{0}' is not registered on any active plugin host")]
    ToolNotFound(String),

    /// Memory backend failure.
    #[error("Agent memory failure: {0}")]
    Memory(String),

    /// The model did not produce a usable summary when asked to compact a conversation.
    #[error("Conversation compaction failed: {0}")]
    Compaction(String),

    /// The turn was stopped through its [`crate::StopSignal`] before it finished.
    #[error("the turn was stopped before it finished")]
    Stopped,
}

impl From<tonic::Status> for AgentError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

impl From<ToolRouterError> for AgentError {
    fn from(err: ToolRouterError) -> Self {
        match err {
            ToolRouterError::Gateway(g) => Self::Gateway(g),
            ToolRouterError::Rpc(s) => Self::Rpc(s),
            ToolRouterError::ToolNotFound(t) => Self::ToolNotFound(t),
            ToolRouterError::Stopped => Self::Stopped,
        }
    }
}

/// Errors arising during conversation memory operations.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// Another turn or mutation owns the session writer.
    #[error("session '{0}' is busy")]
    Busy(String),
    /// Embedded SQLite database failure.
    #[error("SQLite memory error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Serialization or JSON parsing error.
    #[error("Memory serialization error: {0}")]
    Serialization(String),

    /// Other memory operational failure.
    #[error("Memory backend failure: {0}")]
    Backend(String),
}

impl From<MemoryError> for AgentError {
    fn from(err: MemoryError) -> Self {
        match err {
            MemoryError::Busy(session) => Self::Busy(session),
            other => Self::Memory(other.to_string()),
        }
    }
}
