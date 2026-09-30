//! QQ Official adapter management: write-only credentials, hot configuration and QR binding.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use kanon_adapter_qqofficial::bind::{self, LoginStatus};
use kanon_adapter_qqofficial::{QqOfficialAdapter, QqOfficialConfig, QqOfficialStatus};
use kanon_core::AdapterError;

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the QQ Official adapter management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/adapters/qqofficial/config",
            get(read_config).put(update_config),
        )
        .route("/api/v1/adapters/qqofficial/login/qr", post(login_qr))
        .route("/api/v1/adapters/qqofficial/login/poll", post(login_poll))
}

/// Stored configuration (without the secret) together with the live status.
#[derive(Debug, Serialize)]
pub struct QqOfficialConfigView {
    /// What the operator asked for; the secret is never returned.
    pub config: QqOfficialConfig,
    /// What is actually happening; after a failed connection the two legitimately disagree.
    pub status: QqOfficialStatus,
}

/// Configuration update payload.
#[derive(Debug, Deserialize)]
pub struct QqOfficialConfigRequest {
    /// Whether the adapter connects to the QQ gateway.
    pub enabled: bool,
    /// Bot AppID.
    #[serde(default)]
    pub app_id: String,
    /// Replacement AppSecret; omitted or blank keeps the stored one.
    #[serde(default)]
    pub secret: Option<String>,
    /// Use the sandbox API.
    #[serde(default)]
    pub sandbox: bool,
    /// Send replies as native Markdown.
    #[serde(default)]
    pub markdown: bool,
}

/// A new QR binding task.
#[derive(Debug, Serialize)]
pub struct QqQrResponse {
    /// Task to poll.
    pub task_id: String,
    /// Key the console hands back when polling; it decrypts the returned secret.
    pub bind_key: String,
    /// URL to render as the QR code.
    pub qrcode_url: String,
    /// Suggested polling interval.
    pub poll_interval_seconds: u64,
}

/// Poll request for a QR binding task.
#[derive(Debug, Deserialize)]
pub struct QqPollRequest {
    /// Task returned by `login/qr`.
    pub task_id: String,
    /// Key returned by `login/qr`.
    pub bind_key: String,
}

/// Result of polling a QR binding task.
#[derive(Debug, Serialize)]
pub struct QqPollResponse {
    /// `pending`, `created` (credentials saved and applied) or `expired`.
    pub status: &'static str,
    /// QQ's raw status code while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_status: Option<i64>,
    /// The bound AppID once created; the secret goes straight into the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appid: Option<String>,
}

/// Reads the stored configuration and the live status.
async fn read_config(
    State(state): State<ApiState>,
) -> Result<Json<QqOfficialConfigView>, ApiError> {
    Ok(Json(view(require_adapter(&state)?)))
}

/// Validates, hot-applies and persists a configuration.
async fn update_config(
    State(state): State<ApiState>,
    Json(body): Json<QqOfficialConfigRequest>,
) -> Result<Json<QqOfficialConfigView>, ApiError> {
    let adapter = require_adapter(&state)?;
    let secret = match body.secret.as_deref().map(str::trim) {
        Some(secret) if !secret.is_empty() => Some(secret.to_owned()),
        // Omitted or blank: keep the stored secret, so an unrelated edit cannot drop it.
        _ => adapter.config().secret,
    };
    let candidate = QqOfficialConfig {
        enabled: body.enabled,
        app_id: body.app_id,
        secret,
        sandbox: body.sandbox,
        markdown: body.markdown,
    };
    apply_and_save(&state, adapter, candidate).await?;
    tracing::info!(
        enabled = adapter.config().enabled,
        app_id = %adapter.config().app_id,
        "QQ Official adapter reconfigured through the control plane"
    );
    Ok(Json(view(adapter)))
}

/// Starts a QR binding task.
async fn login_qr(State(state): State<ApiState>) -> Result<Json<QqQrResponse>, ApiError> {
    require_adapter(&state)?;
    let task = bind::request_login(bind::BIND_BASE)
        .await
        .map_err(ApiError::Upstream)?;
    Ok(Json(QqQrResponse {
        task_id: task.task_id,
        bind_key: task.bind_key,
        qrcode_url: task.qrcode_url,
        poll_interval_seconds: bind::POLL_INTERVAL_SECONDS,
    }))
}

/// Polls a QR binding task; on success the credentials are applied, enabled and persisted.
async fn login_poll(
    State(state): State<ApiState>,
    Json(body): Json<QqPollRequest>,
) -> Result<Json<QqPollResponse>, ApiError> {
    let adapter = require_adapter(&state)?;
    let status = bind::poll_login(bind::BIND_BASE, &body.task_id, &body.bind_key)
        .await
        .map_err(ApiError::Upstream)?;
    let response = match status {
        LoginStatus::Pending(code) => QqPollResponse {
            status: "pending",
            qr_status: Some(code),
            appid: None,
        },
        LoginStatus::Expired => QqPollResponse {
            status: "expired",
            qr_status: None,
            appid: None,
        },
        LoginStatus::Bound { app_id, secret } => {
            let candidate = QqOfficialConfig {
                enabled: true,
                app_id: app_id.clone(),
                secret: Some(secret),
                ..adapter.config()
            };
            apply_and_save(&state, adapter, candidate).await?;
            tracing::info!(app_id = %app_id, "QQ Official credentials bound by QR code");
            QqPollResponse {
                status: "created",
                qr_status: None,
                appid: Some(app_id),
            }
        }
    };
    Ok(Json(response))
}

/// Applies first — it validates too, and a rejected configuration must leave both the running
/// adapter and the file untouched — then persists.
async fn apply_and_save(
    state: &ApiState,
    adapter: &QqOfficialAdapter,
    candidate: QqOfficialConfig,
) -> Result<(), ApiError> {
    adapter
        .apply(candidate)
        .await
        .map_err(|error| match error {
            AdapterError::Configuration { reason, .. } => ApiError::BadRequest(reason),
            other => ApiError::Internal(other.to_string()),
        })?;
    state
        .system_config()
        .save_qqofficial(&adapter.config())
        .map_err(|err| {
            ApiError::Internal(format!(
                "The QQ Official adapter was reconfigured but the change could not be persisted \
                 to {}: {err}. The running node uses the new settings until it restarts",
                state.system_config().path().display()
            ))
        })
}

/// Returns the adapter or reports that this node does not host one.
fn require_adapter(state: &ApiState) -> Result<&Arc<QqOfficialAdapter>, ApiError> {
    state.qqofficial().ok_or_else(|| {
        ApiError::NotFound(
            "This node does not host the QQ Official adapter; it was not registered at startup"
                .to_string(),
        )
    })
}

fn view(adapter: &QqOfficialAdapter) -> QqOfficialConfigView {
    QqOfficialConfigView {
        config: adapter.config().without_secret(),
        status: adapter.status(),
    }
}
