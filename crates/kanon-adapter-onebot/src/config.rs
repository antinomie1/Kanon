//! OneBot v11 configuration shared by bootstrap and the management console.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use url::Url;

/// Platform routing key used by default.
pub const DEFAULT_PLATFORM: &str = "onebot";

/// Which side initiates the universal WebSocket connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Kanon connects to the implementation's universal endpoint.
    #[default]
    ForwardWebsocket,
    /// The implementation connects to Kanon's dedicated listener.
    ReverseWebsocket,
}

/// Settings for one OneBot account, independent of the management gateway port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OneBotConfig {
    /// Whether to open a connection or listener.
    pub enabled: bool,
    /// Stable platform identifier bound by bot instances.
    pub platform: String,
    /// Optional console label; cannot change while registered.
    pub display_name: Option<String>,
    /// Connection direction, using one universal socket in either mode.
    pub transport: TransportKind,
    /// Remote URL in forward mode; bind address and path in reverse mode.
    pub ws_url: String,
    /// Bearer credential, omitted from management responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}

impl Default for OneBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            platform: DEFAULT_PLATFORM.into(),
            display_name: None,
            transport: TransportKind::default(),
            ws_url: "ws://127.0.0.1:6700".into(),
            access_token: None,
        }
    }
}

impl OneBotConfig {
    /// Normalizes and validates once before any runtime or persistence changes.
    pub fn prepare(mut self) -> Result<Self, String> {
        self.platform = self.platform.trim().into();
        self.ws_url = self.ws_url.trim().into();
        self.display_name = self
            .display_name
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        self.access_token = self
            .access_token
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if self.platform.is_empty()
            || self
                .platform
                .chars()
                .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' && c != '-')
        {
            return Err("platform must contain lowercase letters, digits, '_' or '-'".into());
        }
        let url = Url::parse(&self.ws_url).map_err(|_| "invalid WebSocket URL")?;
        if !matches!(url.scheme(), "ws" | "wss")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("use an absolute ws:// or wss:// URL without credentials, query or fragment; configure access_token separately".into());
        }
        if let Some(token) = &self.access_token {
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "access_token is not a valid HTTP header value")?;
        }
        if self.transport == TransportKind::ReverseWebsocket {
            if url.scheme() != "ws" {
                return Err(
                    "reverse WebSocket listens over ws://; terminate TLS at a reverse proxy".into(),
                );
            }
            self.listen_addr()?;
        }
        Ok(self)
    }

    /// Resolves the reverse listener without DNS or implicit interface selection.
    pub(crate) fn listen_addr(&self) -> Result<SocketAddr, String> {
        let url = Url::parse(&self.ws_url).map_err(|_| "invalid WebSocket URL")?;
        let host = url
            .host_str()
            .ok_or("reverse WebSocket requires an IP address")?;
        let ip = host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .map_err(|_| "reverse WebSocket requires a literal bind IP address")?;
        let port = url
            .port_or_known_default()
            .ok_or("missing WebSocket port")?;
        if port == 0 {
            return Err("reverse WebSocket requires a nonzero port".into());
        }
        Ok(SocketAddr::new(ip, port))
    }

    /// Returns a credential-free configuration for the console.
    pub fn without_token(&self) -> Self {
        Self {
            access_token: None,
            ..self.clone()
        }
    }

    /// Human-readable adapter name.
    pub fn effective_display_name(&self) -> String {
        self.display_name
            .clone()
            .unwrap_or_else(|| self.platform.clone())
    }
}
