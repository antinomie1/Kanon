//! Unified API error taxonomy and HTTP response mapping.
//!
//! Every failure produced by a management endpoint is funnelled through [`ApiError`] so
//! that console clients always receive the same machine-readable envelope:
//!
//! ```json
//! { "error": { "code": "not_found", "message": "Plugin 'x' is not loaded by any active host" } }
//! ```
//!
//! Errors are never silently swallowed: upstream plugin failures surface as `502` payloads
//! carrying the original reason string instead of a generic "something went wrong".

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Errors returned by the management API surface.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The addressed resource (plugin, session, host) does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The request payload or query string violated the endpoint contract.
    #[error("{0}")]
    BadRequest(String),
    /// The requested operation is not valid for the current resource state.
    #[error("{0}")]
    Conflict(String),
    /// A dependency required to serve the request is not configured or not running.
    #[error("{0}")]
    Unavailable(String),
    /// A plugin host or IPC round trip failed.
    #[error("{0}")]
    Upstream(String),
    /// Authentication or signature verification failed.
    #[error("{0}")]
    Unauthorized(String),
    /// Unexpected internal failure (filesystem, serialization, task join).
    #[error("{0}")]
    Internal(String),
    /// The request body exceeded the limit the endpoint accepts.
    #[error("{0}")]
    PayloadTooLarge(String),
    /// A plugin host did not answer within the time the gateway waits for it.
    #[error("{0}")]
    Timeout(String),
}

impl ApiError {
    /// Maps the error variant onto its HTTP status code.
    pub fn status_code(&self) -> StatusCode {
        match self {
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Upstream(_) => StatusCode::BAD_GATEWAY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        }
    }

    /// Returns the stable machine-readable error code exposed to clients.
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::NotFound(_) => "not_found",
            ApiError::BadRequest(_) => "bad_request",
            ApiError::Unauthorized(_) => "unauthorized",
            ApiError::Conflict(_) => "conflict",
            ApiError::Unavailable(_) => "unavailable",
            ApiError::Upstream(_) => "upstream_error",
            ApiError::Internal(_) => "internal_error",
            ApiError::PayloadTooLarge(_) => "payload_too_large",
            ApiError::Timeout(_) => "timeout",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = json!({
            "error": {
                "code": self.code(),
                "message": self.to_string(),
            }
        });

        // Log server-side faults so that operators keep the full context even though the
        // client only receives a stable, non-leaking error envelope.
        if status.is_server_error() {
            tracing::error!(status = %status, error = %self, "Management API request failed");
        } else {
            tracing::debug!(status = %status, error = %self, "Management API request rejected");
        }

        (status, Json(body)).into_response()
    }
}

impl From<kanon_core::SupervisorError> for ApiError {
    fn from(err: kanon_core::SupervisorError) -> Self {
        use kanon_core::SupervisorError;

        match err {
            SupervisorError::HostNotFound(id) => {
                ApiError::NotFound(format!("Host '{id}' not found in supervisor"))
            }
            SupervisorError::HostBusy(id) => {
                ApiError::Conflict(format!("Host '{id}' is already being started"))
            }
            SupervisorError::PluginNotFound(id) => {
                ApiError::NotFound(format!("Plugin '{id}' is not loaded by any active host"))
            }
            SupervisorError::RestartUnavailable(id) => ApiError::Conflict(format!(
                "Host '{id}' has no recorded launch specification and cannot be restarted"
            )),
            SupervisorError::InvalidConfigPayload => ApiError::BadRequest(
                "Plugin configuration payload must be a JSON object".to_string(),
            ),
            SupervisorError::ConfigReloadRejected {
                host_id,
                plugin_id,
                reason,
            } => ApiError::Upstream(format!(
                "Host '{host_id}' rejected config reload for plugin '{plugin_id}': {reason}"
            )),
            SupervisorError::StaleConfigVersion {
                plugin_id,
                current_version,
                requested_version,
            } => ApiError::Conflict(format!(
                "Stale configuration version for plugin '{plugin_id}': current version is {current_version}, but requested {requested_version}"
            )),
            SupervisorError::Rpc(status) => {
                if status.code() == tonic::Code::NotFound {
                    ApiError::NotFound(status.message().to_string())
                } else {
                    ApiError::Upstream(format!("Plugin IPC call failed: {status}"))
                }
            }
            other => ApiError::Upstream(other.to_string()),
        }
    }
}

impl From<kanon_llm::MemoryError> for ApiError {
    fn from(err: kanon_llm::MemoryError) -> Self {
        match err {
            kanon_llm::MemoryError::Busy(session) => {
                ApiError::Conflict(format!("Session '{session}' is busy"))
            }
            other => ApiError::Internal(format!("Conversation memory failure: {other}")),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        ApiError::Internal(format!("Filesystem failure: {err}"))
    }
}
