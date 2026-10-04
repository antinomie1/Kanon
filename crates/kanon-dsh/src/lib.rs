//! Optional deepseek-harness integration, independent of Kanon's built-in model loop.
//!
//! Only routing and transport belong here. DSH remains authoritative for its settings,
//! model selection, event journal, context compaction and durable session lifecycle.

mod client;
mod reservation;
mod session;
mod stream;

pub use client::DshClient;
pub use session::{DshSession, DshSnapshot, DshTurnOutput};
pub use stream::DshStream;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Connection-only settings. Agent behavior is configured through DSH's own settings API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DshConfig {
    /// DSH Web profile origin, without credentials, query parameters or a fragment.
    pub base_url: String,
    /// File containing the Cookie header issued by DSH; reread to honor cookie rotation.
    pub cookie_file: Option<PathBuf>,
    /// Deadline for an individual remote management operation.
    pub request_timeout_seconds: u64,
    /// Upper bound for an agent turn, including its tools and user interaction.
    pub turn_timeout_seconds: u64,
}

impl Default for DshConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:3080".into(),
            cookie_file: None,
            request_timeout_seconds: 30,
            turn_timeout_seconds: 600,
        }
    }
}

impl DshConfig {
    /// Validates transport inputs before any state is published or connection attempted.
    pub fn validate(&self) -> Result<(), DshError> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| DshError::Config("base_url must be an HTTP(S) origin".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(DshError::Config(
                "base_url must be an HTTP(S) origin without credentials or a path".into(),
            ));
        }
        if !(1..=120).contains(&self.request_timeout_seconds)
            || !(10..=3600).contains(&self.turn_timeout_seconds)
            || self.request_timeout_seconds > self.turn_timeout_seconds
        {
            return Err(DshError::Config("request timeout must be 1..=120 seconds and no greater than turn timeout (10..=3600)".into()));
        }
        if self
            .cookie_file
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(DshError::Config("cookie_file must not be empty".into()));
        }
        Ok(())
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }
}

/// Explicit DSH failures; none authorize retrying a prompt with the built-in agent.
#[derive(Debug, thiserror::Error)]
pub enum DshError {
    /// Connection configuration cannot represent a valid DSH deployment.
    #[error("invalid DSH configuration: {0}")]
    Config(String),
    /// The configured browser credential cannot be read or used as a header.
    #[error("DSH authentication failed: {0}")]
    Authentication(String),
    /// The HTTP or WebSocket carrier failed before a valid result arrived.
    #[error("DSH transport failed: {0}")]
    Transport(String),
    /// DSH rejected the operation with a structured domain error.
    #[error("DSH {code}: {message}")]
    Remote {
        /// Stable remote domain error code.
        code: String,
        /// Human-readable remote diagnostic.
        message: String,
    },
    /// A wire response violated the supported DSH contract.
    #[error("invalid DSH response: {0}")]
    Protocol(String),
    /// A bounded management call or conversation turn exceeded its deadline.
    #[error("DSH {0} timed out")]
    Timeout(&'static str),
    /// An operator stopped the conversation turn.
    #[error("DSH turn stopped")]
    Stopped,
    /// The turn ended without a successful completion.
    #[error("DSH turn ended: {0}")]
    TurnFailed(String),
}

/// Bounds both HTTP results and individual WebSocket frames before deserialization.
pub const MAX_WIRE_BYTES: usize = 32 * 1024 * 1024;
