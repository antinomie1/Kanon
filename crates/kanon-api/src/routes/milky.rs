//! Milky platform adapter management (`/api/v1/adapters/milky/*`).
//!
//! # Why the adapter has its own management surface
//! Every other adapter is configured before the node starts — the bundled webhook bridge from the
//! environment, plugin adapters from their manifest. Milky is a *platform account*: an operator
//! connects a QQ number, may need to change the endpoint when the protocol implementation moves,
//! and must be able to see whether the account is actually online. That is console work, so the
//! configuration is validated, persisted to `data/system.json` and applied to the running adapter
//! in one call — no restart, and the file on disk always describes what is running.
//!
//! # Write-only credential
//! The `access_token` is never returned. A request therefore states its intent explicitly:
//! omitting the field (or sending an empty string) keeps the stored token, sending a value
//! replaces it, and `clear_access_token` removes it. An empty string cannot mean "remove",
//! because that is exactly what an HTML form submits for an untouched password field — reading it
//! as deletion would silently unauthenticate a working node on an unrelated edit.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use kanon_adapter_milky::{
    DEFAULT_PLATFORM, MilkyAdapter, MilkyConfig, MilkyStatus, MilkyTestReport, TransportKind,
};
use kanon_core::AdapterError;

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the Milky adapter management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/adapters/milky/config",
            get(read_config).put(update_config),
        )
        .route("/api/v1/adapters/milky/config/test", post(test_config))
}

/// Effective configuration together with live adapter status.
#[derive(Debug, Serialize)]
pub struct MilkyConfigView {
    /// Stored configuration with the credential removed; this is what a console form edits.
    pub config: MilkyConfig,
    /// Live status: connection state, counters and the cached account identity.
    ///
    /// Separate from `config` on purpose — configuration is what was asked for, status is what is
    /// actually happening, and after a failed connection the two legitimately disagree.
    pub status: MilkyStatus,
}

/// Configuration update payload.
///
/// `platform` and `display_name` are part of the payload for completeness, but changing either is
/// rejected with a `400`: they identify the adapter inside the registry, which cannot be renamed
/// while the node runs.
#[derive(Debug, Deserialize)]
pub struct MilkyConfigRequest {
    /// Whether the adapter should connect to the protocol implementation.
    pub enabled: bool,
    /// Platform identifier owned by the adapter (defaults to `milky`).
    #[serde(default = "default_platform")]
    pub platform: String,
    /// Console-facing name; falls back to the platform identifier.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Base URL of the protocol implementation.
    pub base_url: String,
    /// Inbound transport used to receive events.
    #[serde(default)]
    pub transport: TransportKind,
    /// Replacement credential; omitted or empty keeps the stored one.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Removes the stored credential.
    #[serde(default)]
    pub clear_access_token: bool,
}

/// Returns the platform identifier used when a request omits one.
fn default_platform() -> String {
    DEFAULT_PLATFORM.to_string()
}

/// Connectivity probe payload.
///
/// The probe runs against the values in the form rather than the saved ones, so an operator can
/// verify an endpoint before committing to it. Credential handling matches the update payload:
/// omitted or empty means "use the stored token".
#[derive(Debug, Deserialize)]
pub struct MilkyTestRequest {
    /// Base URL to probe.
    pub base_url: String,
    /// Credential to probe with, overriding the stored one when supplied.
    #[serde(default)]
    pub access_token: Option<String>,
}

/// Reads the stored configuration and the live status.
async fn read_config(State(state): State<ApiState>) -> Result<Json<MilkyConfigView>, ApiError> {
    let adapter = require_adapter(&state)?;
    Ok(Json(view(adapter)))
}

/// Validates, persists and hot-applies a configuration.
async fn update_config(
    State(state): State<ApiState>,
    Json(body): Json<MilkyConfigRequest>,
) -> Result<Json<MilkyConfigView>, ApiError> {
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

    let stored = adapter.config();
    let access_token = if body.clear_access_token {
        None
    } else {
        match body.access_token.as_deref().map(str::trim) {
            Some(token) if !token.is_empty() => Some(token.to_string()),
            // Omitted or blank: keep whatever is stored, so an unrelated edit cannot drop it.
            _ => stored.access_token.clone(),
        }
    };

    let candidate = MilkyConfig {
        enabled: body.enabled,
        platform: body.platform,
        display_name: body.display_name,
        base_url: body.base_url,
        access_token,
        transport: body.transport,
    };

    // Apply first: it validates as well, and a rejected configuration must leave both the running
    // adapter and the file on disk exactly as they were.
    adapter
        .apply(candidate)
        .await
        .map_err(map_configuration_error)?;

    state
        .system_config()
        .save_milky(&adapter.config())
        .map_err(|err| {
            // The adapter is already running the new settings; saying so explicitly beats a silent
            // divergence between the live node and the next restart.
            ApiError::Internal(format!(
                "The Milky adapter was reconfigured but the change could not be persisted to {}: {err}. \
                 The running node uses the new settings until it restarts",
                state.system_config().path().display()
            ))
        })?;

    tracing::info!(
        platform = %adapter.identity(),
        enabled = %adapter.config().enabled,
        base_url = %adapter.config().base_url,
        "Milky adapter reconfigured through the control plane"
    );

    Ok(Json(view(adapter)))
}

/// Probes an endpoint without saving anything.
async fn test_config(
    State(state): State<ApiState>,
    Json(body): Json<MilkyTestRequest>,
) -> Result<Json<MilkyTestReport>, ApiError> {
    let adapter = require_adapter(&state)?;
    let stored = adapter.config();

    let access_token = match body.access_token.as_deref().map(str::trim) {
        Some(token) if !token.is_empty() => Some(token.to_string()),
        _ => stored.access_token.clone(),
    };

    // Probing never needs the connection to be enabled, but it does need the same identity fields
    // the running adapter has, so the candidate is built from the stored configuration.
    let candidate = MilkyConfig {
        enabled: true,
        platform: adapter.identity().to_string(),
        display_name: stored.display_name.clone(),
        base_url: body.base_url,
        access_token,
        transport: stored.transport,
    };

    match adapter.test_connection(Some(candidate)).await {
        Ok(report) => Ok(Json(report)),
        Err(AdapterError::Configuration { reason, .. }) => Err(ApiError::BadRequest(reason)),
        // The endpoint answered but refused, or could not be reached at all: an upstream failure,
        // not a malformed request.
        Err(other) => Err(ApiError::Upstream(other.to_string())),
    }
}

/// Returns the Milky adapter or reports that this node does not host one.
fn require_adapter(state: &ApiState) -> Result<&Arc<MilkyAdapter>, ApiError> {
    state.milky().ok_or_else(|| {
        ApiError::NotFound(
            "This node does not host the Milky platform adapter; it was not registered at startup"
                .to_string(),
        )
    })
}

/// Builds the console view of an adapter.
fn view(adapter: &MilkyAdapter) -> MilkyConfigView {
    MilkyConfigView {
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
