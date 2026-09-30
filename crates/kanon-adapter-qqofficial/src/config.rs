//! QQ Official adapter settings, persisted as the `qqofficial` section of `data/system.json`.

use serde::{Deserialize, Serialize};

/// Platform routing key; bot instances bind to it, so it never changes.
pub const PLATFORM: &str = "qqofficial";

/// Console label of the adapter.
pub const DISPLAY_NAME: &str = "QQ 官方机器人";

/// Credentials and delivery options of one QQ Open Platform bot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqOfficialConfig {
    /// Whether the adapter connects to the QQ gateway.
    pub enabled: bool,
    /// Bot AppID from the QQ Open Platform.
    pub app_id: String,
    /// Bot AppSecret; write-only through the console and never echoed back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Connect to the sandbox API instead of production.
    pub sandbox: bool,
    /// Send text as native Markdown (`msg_type = 2`). The bot needs the Markdown permission; a
    /// refused message is reported as a delivery error rather than silently resent as plain text.
    pub markdown: bool,
}

impl QqOfficialConfig {
    /// Normalizes and validates once, before anything is applied or persisted.
    pub fn prepare(mut self) -> Result<Self, String> {
        self.app_id = self.app_id.trim().to_owned();
        self.secret = self
            .secret
            .map(|secret| secret.trim().to_owned())
            .filter(|secret| !secret.is_empty());
        if self.enabled && (self.app_id.is_empty() || self.secret.is_none()) {
            return Err("app_id and secret are required to enable the QQ Official adapter".into());
        }
        Ok(self)
    }

    /// The configuration without its credential, for the console.
    pub fn without_secret(&self) -> Self {
        Self {
            secret: None,
            ..self.clone()
        }
    }
}
