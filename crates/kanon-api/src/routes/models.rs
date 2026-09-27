//! Per-model settings catalog (`/api/v1/models`).
//!
//! # Why settings are keyed by `<provider>/<model-id>`
//! Context window and input modalities belong to a model *as served by an endpoint*: the same
//! weights reached through two providers can differ in both. The catalog therefore stores one
//! entry per canonical reference, and the pipeline consults it before attaching an image or
//! sizing a conversation.
//!
//! # Where defaults come from
//! Entries discovered through [`super::super::model_discovery`] carry whatever the endpoint
//! reported and are marked `upstream`; entries typed in the console are marked `manual` and are
//! never overwritten by a later discovery, so an operator's correction survives a refresh.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use kanon_llm::{ModelRef, ModelSettingsSource, ModelSpec};

use crate::error::ApiError;
use crate::model_discovery;
use crate::state::ApiState;

/// Registers the model catalog endpoints.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/models", get(list_models).put(upsert_model))
        .route("/api/v1/models/delete", post(delete_model))
        .route("/api/v1/models/discover", post(discover))
}

/// Response of `GET /api/v1/models`.
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    /// Every catalog entry, ordered by canonical reference.
    pub models: Vec<ModelSpec>,
    /// Number of entries.
    pub total: usize,
    /// Canonical reference the node answers with by default, when configured.
    pub default_model: Option<String>,
    /// Provider names a model reference may use.
    pub providers: Vec<String>,
}

/// Request body of `POST /api/v1/models/delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteModelRequest {
    /// Canonical `<provider>/<model-id>` reference to remove.
    pub reference: String,
}

/// Request body of `POST /api/v1/models/discover`.
#[derive(Debug, Deserialize)]
pub struct DiscoverModelsRequest {
    /// Provider whose endpoint is queried.
    pub provider: String,
    /// Whether the discovered entries should be stored in the catalog.
    #[serde(default)]
    pub persist: bool,
}

/// Response of the discovery action.
#[derive(Debug, Serialize)]
pub struct DiscoverModelsResponse {
    /// Provider that was queried.
    pub provider: String,
    /// Models reported by the endpoint.
    pub discovered: Vec<ModelSpec>,
    /// Entries added or refreshed in the catalog (zero when `persist` was false).
    pub persisted: usize,
}

/// Handler for `GET /api/v1/models`.
async fn list_models(State(state): State<ApiState>) -> Json<ModelsResponse> {
    let settings = state.node_settings();
    let mut models = settings.models.clone();
    models.sort_by_key(|spec| spec.full_name());

    Json(ModelsResponse {
        total: models.len(),
        models,
        default_model: settings.default_model,
        providers: settings
            .providers
            .iter()
            .map(|entry| entry.name.clone())
            .collect(),
    })
}

/// Handler for `PUT /api/v1/models`.
///
/// The body is the entry itself; its `source` is forced to `manual` because a value submitted by an
/// operator is a decision, not a discovery result.
async fn upsert_model(
    State(state): State<ApiState>,
    Json(mut spec): Json<ModelSpec>,
) -> Result<Json<ModelsResponse>, ApiError> {
    spec.provider = spec.provider.trim().to_string();
    spec.model = spec.model.trim().to_string();
    if spec.provider.is_empty() || spec.model.is_empty() {
        return Err(ApiError::BadRequest(
            "provider and model must not be empty".to_string(),
        ));
    }
    if let Some(temperature) = spec.temperature
        && !(0.0..=2.0).contains(&temperature)
    {
        return Err(ApiError::BadRequest(format!(
            "temperature {temperature} is outside 0.0..=2.0"
        )));
    }
    spec.source = ModelSettingsSource::Manual;

    let reference = spec.full_name();
    let mut settings = state.node_settings();
    match settings
        .models
        .iter_mut()
        .find(|existing| existing.full_name() == reference)
    {
        Some(existing) => *existing = spec,
        None => settings.models.push(spec),
    }

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    Ok(list_models(State(state)).await)
}

/// Handler for `POST /api/v1/models/delete`.
async fn delete_model(
    State(state): State<ApiState>,
    Json(payload): Json<DeleteModelRequest>,
) -> Result<Json<ModelsResponse>, ApiError> {
    let reference = ModelRef::parse(&payload.reference).canonical();
    let mut settings = state.node_settings();
    let before = settings.models.len();
    settings.models.retain(|spec| spec.full_name() != reference);

    if settings.models.len() == before {
        // Deleting a missing entry is reported rather than silently accepted: the console would
        // otherwise redraw an unchanged list and look like it worked.
        return Err(ApiError::NotFound(format!(
            "model '{reference}' is not in the catalog"
        )));
    }

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    Ok(list_models(State(state)).await)
}

/// Handler for `POST /api/v1/models/discover`.
async fn discover(
    State(state): State<ApiState>,
    Json(payload): Json<DiscoverModelsRequest>,
) -> Result<Json<DiscoverModelsResponse>, ApiError> {
    let provider_name = payload.provider.trim().to_string();
    let settings = state.node_settings();
    let entry = settings
        .providers
        .iter()
        .find(|entry| entry.name == provider_name)
        .cloned()
        .ok_or_else(|| {
            ApiError::NotFound(format!("provider '{provider_name}' is not configured"))
        })?;

    let discovered = model_discovery::discover_models(&entry)
        .await
        .map_err(ApiError::Upstream)?;

    if !payload.persist {
        return Ok(Json(DiscoverModelsResponse {
            provider: provider_name,
            discovered,
            persisted: 0,
        }));
    }

    let mut settings = state.node_settings();
    let persisted = model_discovery::merge_discovered(&mut settings.models, &discovered);

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    Ok(Json(DiscoverModelsResponse {
        provider: provider_name,
        discovered,
        persisted,
    }))
}
