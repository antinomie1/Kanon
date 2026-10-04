//! Native DSH management under the shared agent resource family.
//!
//! Connection settings belong to Kanon; behavior, credentials, models and journals stay in
//! DSH. Returning the native schemas avoids a second, inevitably incomplete settings model.

use super::*;
use axum::extract::{Path, Query};
use axum::routing::post;
use kanon_llm::dsh::{DshClient, DshConfig, DshError, DshSession, DshSnapshot};
use serde_json::Value;
use std::sync::Arc;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/agents/dsh/connection",
            get(connection).put(save_connection),
        )
        .route(
            "/api/v1/agents/dsh/settings",
            get(settings).put(update_settings),
        )
        .route("/api/v1/agents/dsh/models", get(models))
        .route("/api/v1/agents/dsh/sessions", get(sessions))
        .route("/api/v1/agents/dsh/sessions/:id", get(snapshot))
        .route("/api/v1/agents/dsh/sessions/:id/history", get(history))
        .route("/api/v1/agents/dsh/sessions/:id/model", put(select_model))
        .route("/api/v1/agents/dsh/sessions/:id/title", put(rename))
        .route("/api/v1/agents/dsh/sessions/:id/stop", post(stop))
        .route("/api/v1/agents/dsh/sessions/:id/archive", post(archive))
}

fn client(state: &ApiState) -> Result<Arc<DshClient>, ApiError> {
    state
        .agent_factory()
        .dsh_client()
        .ok_or_else(|| ApiError::Unavailable("DSH connection is not configured".into()))
}

fn remote_error(error: DshError) -> ApiError {
    match error {
        DshError::Config(message) => ApiError::BadRequest(message),
        DshError::Timeout(operation) => ApiError::Timeout(format!("DSH {operation} timed out")),
        DshError::Stopped => ApiError::Conflict("DSH turn stopped".into()),
        DshError::Remote { code, message } if code.contains("not-found") => {
            ApiError::NotFound(message)
        }
        DshError::Remote { code, message }
            if code.contains("conflict") || code.contains("busy") =>
        {
            ApiError::Conflict(message)
        }
        other => ApiError::Upstream(other.to_string()),
    }
}

/// Returns only connection coordinates; never reads the browser credential file.
async fn connection(State(state): State<ApiState>) -> Json<Option<DshConfig>> {
    Json(state.node_settings().dsh)
}

/// Prepares and persists the connection before publishing it to live routes.
async fn save_connection(
    State(state): State<ApiState>,
    Json(config): Json<Option<DshConfig>>,
) -> Result<Json<Option<DshConfig>>, ApiError> {
    if config.is_none() {
        let default = state.node_settings().default_agent;
        if state
            .instances()
            .list()
            .await
            .iter()
            .any(|instance| instance.agent.as_deref().unwrap_or(&default) == "dsh")
        {
            return Err(ApiError::Conflict(
                "DSH is selected by an instance; change its agent before removing the connection"
                    .into(),
            ));
        }
    }
    state.update_node_settings(|settings| {
        settings.dsh = config;
        Ok(())
    })?;
    Ok(connection(State(state)).await)
}

async fn settings(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .settings()
        .await
        .map(Json)
        .map_err(remote_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    namespace: String,
    patch: Value,
    revision: u64,
}

async fn update_settings(
    State(state): State<ApiState>,
    Json(request): Json<SettingsPatch>,
) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .update_settings(&request.namespace, request.patch, request.revision)
        .await
        .map(Json)
        .map_err(remote_error)
}

async fn models(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .models()
        .await
        .map(Json)
        .map_err(remote_error)
}

async fn sessions(State(state): State<ApiState>) -> Result<Json<Vec<DshSession>>, ApiError> {
    client(&state)?
        .sessions()
        .await
        .map(Json)
        .map_err(remote_error)
}

async fn snapshot(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<DshSnapshot>, ApiError> {
    client(&state)?
        .snapshot(&id)
        .await
        .map(Json)
        .map_err(remote_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryPage {
    through_seq: u64,
    before_seq: u64,
}

async fn history(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(page): Query<HistoryPage>,
) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .page(&id, page.through_seq, page.before_seq)
        .await
        .map(Json)
        .map_err(remote_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSelection {
    provider: String,
    model: String,
    effort: Option<String>,
}

async fn select_model(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(model): Json<ModelSelection>,
) -> Result<Json<Value>, ApiError> {
    let client = client(&state)?;
    let _writing = state
        .sessions()
        .try_write(&id)
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    client
        .select_model(&id, &model.provider, &model.model, model.effort.as_deref())
        .await
        .map(Json)
        .map_err(remote_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Title {
    title: String,
}

async fn rename(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(title): Json<Title>,
) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .rename_session(&id, &title.title)
        .await
        .map(Json)
        .map_err(remote_error)
}

async fn stop(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    client(&state)?
        .stop_session(&id)
        .await
        .map_err(remote_error)?;
    Ok(Json(serde_json::json!({"stopped": true})))
}

async fn archive(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let client = client(&state)?;
    let _writing = state
        .sessions()
        .try_write(&id)
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    client
        .archive_session(&id)
        .await
        .map(Json)
        .map_err(remote_error)
}
