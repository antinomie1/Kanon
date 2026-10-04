//! OneBot v11 management with write-only credentials and hot configuration.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use kanon_adapter_onebot::{OneBotAdapter, OneBotConfig, OneBotStatus, TransportKind};
use kanon_core::AdapterError;

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the OneBot adapter management routes.
pub fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/adapters/onebot/config",
        get(read_config).put(update_config),
    )
}

/// Effective configuration together with live adapter status.
#[derive(Debug, Serialize)]
pub struct OneBotConfigView {
    /// Stored configuration with the credential removed; this is what a console form edits.
    pub config: OneBotConfig,
    /// Live connection state and the account identity.
    ///
    /// Separate from `config` on purpose — configuration is what was asked for, status is what is
    /// actually happening, and after a failed connection the two legitimately disagree.
    pub status: OneBotStatus,
}

/// Configuration update payload.
///
/// `platform` and `display_name` are part of the payload for completeness, but changing either is
/// rejected with a `400`: they identify the adapter inside the registry, which cannot be renamed
/// while the node runs.
#[derive(Debug, Deserialize)]
pub struct OneBotConfigRequest {
    /// Whether the adapter should connect to the protocol implementation.
    pub enabled: Option<bool>,
    /// Platform identifier; omitted values retain the current identity.
    #[serde(default)]
    pub platform: Option<String>,
    /// Console-facing name; omitted values retain the current identity.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Forward connection URL or reverse WebSocket listener URL.
    pub ws_url: Option<String>,
    /// Which side initiates the combined event and API WebSocket.
    #[serde(default)]
    pub transport: Option<TransportKind>,
    /// Replacement credential; omitted or empty keeps the stored one.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Removes the stored credential.
    #[serde(default)]
    pub clear_access_token: bool,
}

/// Reads the stored configuration and the live status.
async fn read_config(State(state): State<ApiState>) -> Result<Json<OneBotConfigView>, ApiError> {
    let adapter = require_adapter(&state)?;
    Ok(Json(view(adapter)))
}

/// Validates, persists and hot-applies a configuration.
async fn update_config(
    State(state): State<ApiState>,
    Json(body): Json<OneBotConfigRequest>,
) -> Result<Json<OneBotConfigView>, ApiError> {
    let adapter = require_adapter(&state)?;

    if body.clear_access_token
        && body
            .access_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
    {
        return Err(ApiError::BadRequest(
            "Provide either 'access_token' or 'clear_access_token', not both".to_string(),
        ));
    }

    let store = state.system_config().clone();
    adapter
        .update_config(
            move |stored| {
                let access_token = if body.clear_access_token {
                    None
                } else {
                    body.access_token
                        .as_deref()
                        .map(str::trim)
                        .filter(|token| !token.is_empty())
                        .map(str::to_string)
                        .or(stored.access_token)
                };
                OneBotConfig {
                    enabled: body.enabled.unwrap_or(stored.enabled),
                    platform: body.platform.unwrap_or(stored.platform),
                    display_name: body.display_name.or(stored.display_name),
                    ws_url: body.ws_url.unwrap_or(stored.ws_url),
                    access_token,
                    transport: body.transport.unwrap_or(stored.transport),
                }
            },
            move |config| store.save_onebot(config),
        )
        .await
        .map_err(map_configuration_error)?;

    tracing::info!(
        platform = %adapter.identity(),
        enabled = %adapter.config().enabled,
        ws_url = %adapter.config().ws_url,
        "OneBot adapter reconfigured through the control plane"
    );

    Ok(Json(view(adapter)))
}

/// Returns the OneBot adapter or reports that this node does not host one.
fn require_adapter(state: &ApiState) -> Result<&Arc<OneBotAdapter>, ApiError> {
    state.onebot().ok_or_else(|| {
        ApiError::NotFound(
            "This node does not host the OneBot platform adapter; it was not registered at startup"
                .to_string(),
        )
    })
}

/// Builds the console view of an adapter.
fn view(adapter: &OneBotAdapter) -> OneBotConfigView {
    OneBotConfigView {
        config: adapter.config().without_token(),
        status: adapter.status(),
    }
}

/// Maps a configuration-time adapter error onto the management error taxonomy.
fn map_configuration_error(error: AdapterError) -> ApiError {
    match error {
        AdapterError::Configuration { reason, .. } => ApiError::BadRequest(reason),
        other => ApiError::Internal(other.to_string()),
    }
}
