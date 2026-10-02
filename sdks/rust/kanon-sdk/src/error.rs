//! [`CoreError`]: what a call into the core (`BotApiService`) can fail with.
//!
//! The core reports failures as gRPC status codes, each with a documented meaning (see
//! `docs/PLUGIN_API.md`). They are mapped to variants here so a plugin can `match` on the reason
//! instead of comparing raw codes, and so the SDK's own input validation — done before anything
//! is sent — fails the same way the core would.

use tonic::Code;

/// Why a call into the core failed.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The request is malformed (`INVALID_ARGUMENT`), or the SDK rejected it before sending.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// What the request names does not exist (`NOT_FOUND`): an unknown platform, session,
    /// persona or host, or a chat no enabled bot instance answers.
    #[error("not found: {0}")]
    NotFound(String),
    /// The request is valid but not allowed in the current state (`FAILED_PRECONDITION`), e.g.
    /// changing the built-in persona.
    #[error("failed precondition: {0}")]
    FailedPrecondition(String),
    /// The core cannot serve the request now (`UNAVAILABLE`): no model is configured, the core is
    /// shutting down, or the connection to it is gone.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The core (or the adapter behind it) does not offer this call (`UNIMPLEMENTED`).
    #[error("not implemented by the core: {0}")]
    Unimplemented(String),
    /// The deadline passed (`DEADLINE_EXCEEDED`). The outcome is unknown — a reply may have been
    /// delivered — so the call must not be retried automatically.
    #[error("deadline exceeded (outcome unknown): {0}")]
    DeadlineExceeded(String),
    /// A queue or quota is full (`RESOURCE_EXHAUSTED`).
    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),
    /// Any other status the core answered with.
    #[error("core call failed ({code:?}): {message}")]
    Rpc {
        /// The gRPC status code.
        code: Code,
        /// The status message.
        message: String,
    },
    /// The core answered, but not with what the contract promises (e.g. a render without a file).
    #[error("unexpected answer from the core: {0}")]
    Unexpected(String),
    /// A value could not be converted to or from JSON (key-value storage).
    #[error("JSON conversion failed: {0}")]
    Json(#[from] serde_json::Error),
    /// The host runs without a core (standalone mode), so there is nothing to call.
    #[error("no core connection: the host runs standalone")]
    Standalone,
}

impl From<tonic::Status> for CoreError {
    fn from(status: tonic::Status) -> Self {
        let message = status.message().to_string();
        match status.code() {
            Code::InvalidArgument => Self::InvalidArgument(message),
            Code::NotFound => Self::NotFound(message),
            Code::FailedPrecondition => Self::FailedPrecondition(message),
            Code::Unavailable => Self::Unavailable(message),
            Code::Unimplemented => Self::Unimplemented(message),
            Code::DeadlineExceeded => Self::DeadlineExceeded(message),
            Code::ResourceExhausted => Self::ResourceExhausted(message),
            code => Self::Rpc { code, message },
        }
    }
}

impl CoreError {
    /// The gRPC code this error corresponds to; `None` for failures that never reached the wire
    /// as a status ([`Json`](Self::Json), [`Unexpected`](Self::Unexpected),
    /// [`Standalone`](Self::Standalone)).
    pub fn code(&self) -> Option<Code> {
        Some(match self {
            Self::InvalidArgument(_) => Code::InvalidArgument,
            Self::NotFound(_) => Code::NotFound,
            Self::FailedPrecondition(_) => Code::FailedPrecondition,
            Self::Unavailable(_) => Code::Unavailable,
            Self::Unimplemented(_) => Code::Unimplemented,
            Self::DeadlineExceeded(_) => Code::DeadlineExceeded,
            Self::ResourceExhausted(_) => Code::ResourceExhausted,
            Self::Rpc { code, .. } => *code,
            Self::Unexpected(_) | Self::Json(_) | Self::Standalone => return None,
        })
    }

    /// Shorthand for a request the SDK rejects before sending it.
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }
}
