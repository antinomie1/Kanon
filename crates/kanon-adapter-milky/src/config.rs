//! Operator-facing configuration of the Milky platform adapter.
//!
//! # Why configuration is a plain serializable struct
//! The adapter is configured twice in a node's life: once from the environment (containers, CI,
//! a fresh deployment) and once — and thereafter — from `data/system.json` through the management
//! console. Both paths must produce *the same* value, so the struct is the single description of
//! a Milky connection and every source is merely a way to populate it. Validation lives here too,
//! so a configuration accepted by the console is exactly a configuration the adapter can run:
//! neither path can smuggle in a combination the runtime would later reject.
//!
//! # What is deliberately not configurable
//! Request timeouts and reconnect backoff are constants, not settings. They are properties of the
//! adapter's I/O discipline (bounded delivery latency, bounded reconnect storms) rather than of a
//! particular deployment, and exposing them would create knobs whose only realistic effect is to
//! break that discipline.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default platform identifier owned by the adapter.
///
/// It is the string carried in `PipelineEventRequest.platform` and the one bot instances bind to,
/// so it must be stable across restarts. Deployments that need a different identifier set it in
/// the stored configuration; changing it later requires a node restart, because a platform
/// identifier is the routing key of the adapter registry.
pub const DEFAULT_PLATFORM: &str = "milky";

/// Default protocol-implementation base URL.
///
/// Port `3010` is the port Milky implementations conventionally listen on.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3010";

/// Timeout applied to a single API call or event-stream handshake.
///
/// Kept bounded because the outbound dispatcher drains each platform's queue sequentially: a
/// protocol implementation that stops answering must fail a delivery instead of stalling the
/// queue behind it.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// First delay before an event stream is re-established after a failure.
pub const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Upper bound of the exponential reconnect backoff.
///
/// A protocol implementation that is down for hours must not be hammered, but recovery should
/// still be noticed within half a minute once it returns.
pub const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Inbound event transport used to receive pushed events.
///
/// Milky implementations expose one `/event` endpoint that upgrades to WebSocket or falls back to
/// Server-Sent Events depending on the request, so the choice is the application side's: both
/// transports carry the very same JSON payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Server-Sent Events over the plain `/event` HTTP response stream.
    ///
    /// The default because it needs nothing but an HTTP client and survives proxies that refuse
    /// connection upgrades.
    #[default]
    Sse,
    /// WebSocket upgrade of the same `/event` endpoint.
    ///
    /// Preferred when the protocol implementation or an intermediate proxy buffers SSE
    /// responses, which would otherwise delay every event until the buffer flushes.
    Websocket,
}

impl TransportKind {
    /// Canonical wire/configuration name of this transport.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sse => "sse",
            Self::Websocket => "websocket",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TransportKind {
    type Err = ConfigError;

    /// Parses a transport name, accepting the common aliases operators type by hand.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sse" | "eventsource" | "event-stream" => Ok(Self::Sse),
            "websocket" | "ws" | "wss" => Ok(Self::Websocket),
            other => Err(ConfigError::Transport(other.to_string())),
        }
    }
}

/// A complete Milky adapter configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MilkyConfig {
    /// Whether the adapter should connect to the protocol implementation at all.
    ///
    /// A disabled adapter stays registered — so the console can always show and edit it — but
    /// holds no connection, reports itself as not connected and fails deliveries explicitly.
    #[serde(default)]
    pub enabled: bool,
    /// Platform identifier owned by this adapter.
    #[serde(default = "default_platform")]
    pub platform: String,
    /// Console-facing name; falls back to the platform identifier when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Base URL of the Milky protocol implementation, without a trailing slash.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Shared `access_token` sent as `Authorization: Bearer ...`.
    ///
    /// Optional: Milky implementations may run without one, but the protocol documentation warns
    /// that an unauthenticated endpoint must never be reachable from an untrusted network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Transport used to receive inbound events.
    #[serde(default)]
    pub transport: TransportKind,
    /// Accept every friend request automatically; otherwise requests wait for a human in QQ.
    #[serde(default)]
    pub auto_accept_friends: bool,
    /// Accept every invitation into a group automatically.
    #[serde(default)]
    pub auto_accept_group_invites: bool,
    /// React with a thumbs-up to a group message the bot is about to answer, so the sender sees
    /// it was noticed while the model is still thinking.
    #[serde(default)]
    pub reaction_ack: bool,
}

/// Returns the default platform identifier for serde defaults.
fn default_platform() -> String {
    DEFAULT_PLATFORM.to_string()
}

/// Returns the default base URL for serde defaults.
fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

impl Default for MilkyConfig {
    /// Returns a disabled configuration pointing at the conventional local Milky endpoint.
    fn default() -> Self {
        Self {
            enabled: false,
            platform: default_platform(),
            display_name: None,
            base_url: default_base_url(),
            access_token: None,
            transport: TransportKind::Sse,
            auto_accept_friends: false,
            auto_accept_group_invites: false,
            reaction_ack: false,
        }
    }
}

/// Reasons a Milky configuration cannot be used to run the adapter.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The platform identifier is empty.
    #[error("platform identifier must not be empty")]
    EmptyPlatform,
    /// The platform identifier contains a character that would complicate routing or logging.
    #[error(
        "platform identifier '{platform}' contains invalid character '{character}'; use lowercase letters, digits, '_' or '-'"
    )]
    InvalidPlatform {
        /// The rejected identifier.
        platform: String,
        /// The first offending character.
        character: char,
    },
    /// The base URL is empty.
    #[error("base URL must not be empty")]
    EmptyBaseUrl,
    /// The base URL is not an absolute HTTP(S) URL.
    #[error("base URL '{0}' must start with http:// or https:// and name a host")]
    InvalidBaseUrl(String),
    /// The transport name is not recognized.
    #[error("unknown transport '{0}'; expected 'sse' or 'websocket'")]
    Transport(String),
}

impl MilkyConfig {
    /// Trims textual fields and normalizes the base URL.
    ///
    /// Applied before validation *and* before persistence so the stored document never carries a
    /// trailing slash or a whitespace-padded token that would later produce a different request
    /// URL than the one the operator reviewed.
    pub fn normalized(mut self) -> Self {
        self.platform = self.platform.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.display_name = self
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        self.access_token = self
            .access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        self
    }

    /// Validates the configuration for a running adapter.
    ///
    /// Disabled configurations are validated as strictly as enabled ones: a configuration that
    /// cannot run must be rejected while the operator is looking at it, not silently accepted and
    /// discovered broken at the moment it is switched on.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.platform.is_empty() {
            return Err(ConfigError::EmptyPlatform);
        }
        if let Some(character) = self
            .platform
            .chars()
            .find(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '_' && *c != '-')
        {
            return Err(ConfigError::InvalidPlatform {
                platform: self.platform.clone(),
                character,
            });
        }

        let url = self.base_url.as_str();
        if url.is_empty() {
            return Err(ConfigError::EmptyBaseUrl);
        }
        let host = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"));
        match host {
            Some(host)
                if !host.is_empty()
                    && !host.contains(char::is_whitespace)
                    && !host.contains('?')
                    && !host.contains('#') =>
            {
                Ok(())
            }
            _ => Err(ConfigError::InvalidBaseUrl(self.base_url.clone())),
        }
    }

    /// Validates and normalizes in one step, as used by both configuration sources.
    pub fn prepare(self) -> Result<Self, ConfigError> {
        let normalized = self.normalized();
        normalized.validate()?;
        Ok(normalized)
    }

    /// Console-facing name, falling back to the platform identifier.
    pub fn effective_display_name(&self) -> String {
        self.display_name
            .clone()
            .unwrap_or_else(|| self.platform.clone())
    }

    /// Whether a usable `access_token` is present.
    pub fn token_configured(&self) -> bool {
        self.access_token.is_some()
    }

    /// Returns the configuration with the credential removed, safe to report to a console.
    ///
    /// The token is a node credential shared with the protocol implementation; echoing it back to
    /// a browser would leak it into browser history, screenshots and support tickets while adding
    /// no operator value, because the field is write-only in practice.
    pub fn without_token(&self) -> Self {
        Self {
            access_token: None,
            ..self.clone()
        }
    }

    /// Absolute URL of one API endpoint.
    ///
    /// `endpoint` is a Milky endpoint name such as `send_group_message`; see
    /// [`crate::client::MilkyClient`] for the typed wrappers.
    pub fn api_url(&self, endpoint: &str) -> String {
        format!("{}/api/{}", self.base_url, endpoint)
    }

    /// Absolute URL of the event stream, with the scheme mapped to its WebSocket equivalent.
    ///
    /// The operator supplies one HTTP base URL; deriving the WebSocket URL from it keeps a single
    /// source of truth, so an `https://` implementation is reached over `wss://` automatically
    /// instead of requiring a second, possibly inconsistent, setting.
    pub fn event_url(&self) -> String {
        let base = self.base_url.as_str();
        if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}/event")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}/event")
        } else {
            // Validation rejects anything else, so this branch is unreachable for a prepared
            // configuration; returning the raw concatenation keeps the method total without
            // inventing a scheme.
            format!("{base}/event")
        }
    }

    /// The exact `Authorization` header value, when a token is configured.
    pub fn authorization_header(&self) -> Option<String> {
        self.access_token
            .as_ref()
            .map(|token| format!("Bearer {token}"))
    }
}
