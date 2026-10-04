//! Model provider endpoints (`/api/v1/providers`).
//!
//! # Providers are endpoints, not defaults
//! A model is addressed as `<provider>/<model-id>`, so the node needs a directory that maps the
//! provider part to an endpoint, its protocol and its credential. That directory lives in
//! `data/system.json`, is edited here, and is applied to the running node through the shared agent
//! factory — the very next message uses it, with no restart.
//!
//! Which model answers by default is a separate, single decision (`PUT /api/v1/models/default`):
//! no provider is "the default" or "the active one", so there is nothing here that can disagree
//! with it.
//!
//! # One authoritative store, one apply path
//! Every mutation updates the current settings through `ApiState::update_node_settings`, which
//! serializes edits, validates, persists and publishes in that order. Keeping
//! a single apply path is what guarantees the console can never describe a directory the running
//! node does not have.

use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use kanon_llm::gateway::types::{ChatMessage, ChatRequest};
use kanon_llm::{ModelRef, ModelSpec, ProviderEntry};

use crate::error::ApiError;
use crate::llm_config::{derive_provider_name, provider_presets};
use crate::state::ApiState;

/// Pre-configured provider template shared with configuration migration.
pub use crate::llm_config::ProviderPresetDef as ProviderPreset;

/// Registers the providers endpoints.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/providers",
            get(list_providers).post(upsert_provider),
        )
        .route("/api/v1/providers/delete", post(delete_provider))
        .route("/api/v1/providers/test", post(test_provider))
        .route("/api/v1/providers/models", post(fetch_models))
}

/// Catalog response listing the configured providers and the templates for adding more.
#[derive(Debug, Serialize)]
pub struct ProvidersCatalogResponse {
    /// Available wire protocols.
    pub available_protocols: Vec<ProtocolDescriptor>,
    /// Popular pre-configured provider templates.
    pub presets: &'static [ProviderPreset],
    /// Every configured provider endpoint.
    pub providers: Vec<ProviderInfo>,
}

/// One configured provider endpoint, credential excluded.
#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    /// Operator-chosen name used as the model-reference prefix.
    pub name: String,
    /// Wire protocol.
    pub protocol: String,
    /// Endpoint base URL.
    pub base_url: String,
    /// Whether a credential is stored.
    pub api_key_configured: bool,
    /// Default sampling temperature for this endpoint.
    pub temperature: Option<f32>,
    /// Default generation ceiling for this endpoint.
    pub max_tokens: Option<u32>,
    /// Whether retained reasoning is replayed to compatible endpoints.
    pub replay_reasoning: bool,
}

impl ProviderInfo {
    /// Renders one entry for the console.
    fn from_entry(entry: &ProviderEntry) -> Self {
        Self {
            name: entry.name.clone(),
            protocol: entry.protocol.clone(),
            base_url: entry.base_url.clone(),
            api_key_configured: entry.has_api_key(),
            temperature: entry.temperature,
            max_tokens: entry.max_tokens,
            replay_reasoning: entry.replay_reasoning,
        }
    }
}

/// Request payload for `POST /api/v1/providers` (create or replace one named endpoint).
#[derive(Debug, Deserialize)]
pub struct UpsertProviderRequest {
    /// Provider name; also the prefix of every model reference it serves.
    pub name: String,
    /// Wire protocol: `openai` (alias `openai_chat`), `openai_reasoning`,
    /// `openai_responses` or `anthropic`.
    pub protocol: String,
    /// Endpoint base URL.
    pub base_url: String,
    /// Credential. Omitted means "keep the stored one"; `clear_api_key` removes it explicitly.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whether to remove the stored credential.
    #[serde(default)]
    pub clear_api_key: bool,
    /// Default sampling temperature for models on this endpoint.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Default generation ceiling for models on this endpoint.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Omission preserves an existing preference; new endpoints default to replay enabled.
    #[serde(default)]
    pub replay_reasoning: Option<bool>,
}

/// Request payload for `POST /api/v1/providers/delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteProviderRequest {
    /// Endpoint to remove.
    pub name: String,
}

/// Protocol specification descriptor.
#[derive(Debug, Serialize)]
pub struct ProtocolDescriptor {
    /// Protocol identifier.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Standard base URL for this protocol.
    pub default_base_url: &'static str,
}

/// Request payload to test provider connectivity.
///
/// Two ways to say *what* to test, and they never mix implicitly:
/// - `provider` names a configured endpoint. Its stored credential is used **on the server**, which
///   is the only way to probe it, since the console never receives the secret. `protocol`,
///   `base_url` and `api_key` are optional overrides for a form the operator has edited but not
///   saved yet.
/// - without `provider`, `protocol` and `base_url` describe a throw-away endpoint.
#[derive(Debug, Deserialize)]
pub struct TestProviderRequest {
    /// Configured endpoint to test.
    #[serde(default)]
    pub provider: Option<String>,
    /// Protocol wire format: `openai`, `openai_reasoning`, `openai_responses`, or `anthropic`.
    #[serde(default)]
    pub protocol: Option<String>,
    /// Target base URL.
    #[serde(default)]
    pub base_url: Option<String>,
    /// API key credential.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Upstream model id to query, exactly as the endpoint expects it (no provider prefix).
    ///
    /// Defaults to the global default model when this endpoint serves it, then to the first model
    /// in the endpoint's catalog.
    #[serde(default)]
    pub model: Option<String>,
    /// Custom test prompt (defaults to "ping").
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Response payload from provider connectivity and latency test.
#[derive(Debug, Serialize)]
pub struct TestProviderResponse {
    /// Test result: `ok` or `error`.
    pub status: &'static str,
    /// Measured round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// Model queried.
    pub model: String,
    /// Assistant reply preview, when successful.
    pub reply: Option<String>,
    /// Human-readable error message, when failed.
    pub error: Option<String>,
}

/// Handler for `GET /api/v1/providers`.
async fn list_providers(
    State(state): State<ApiState>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let settings = state.node_settings();

    let available_protocols = vec![
        ProtocolDescriptor {
            id: "openai",
            name: "OpenAI / Compatible (v1/chat/completions)",
            default_base_url: "https://api.openai.com/v1",
        },
        ProtocolDescriptor {
            id: "openai_reasoning",
            name: "OpenAI Compatible + reasoning_content replay",
            default_base_url: "https://api.deepseek.com/v1",
        },
        ProtocolDescriptor {
            id: "openai_responses",
            name: "OpenAI Responses API (v1/responses)",
            default_base_url: "https://api.openai.com/v1",
        },
        ProtocolDescriptor {
            id: "anthropic",
            name: "Anthropic Claude (v1/messages)",
            default_base_url: "https://api.anthropic.com/v1",
        },
    ];

    Ok(Json(ProvidersCatalogResponse {
        available_protocols,
        presets: provider_presets(),
        providers: settings
            .providers
            .iter()
            .map(ProviderInfo::from_entry)
            .collect(),
    }))
}

/// Handler for `POST /api/v1/providers`.
///
/// Creates or replaces one endpoint. It does not touch the global default model: choosing what
/// answers is a separate decision the operator makes with the endpoint's models in front of them.
async fn upsert_provider(
    State(state): State<ApiState>,
    Json(payload): Json<UpsertProviderRequest>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let name = payload.name.trim().to_string();
    state.update_node_settings(|settings| {
        let existing = settings.providers.iter().find(|entry| entry.name == name);
        let base_url = payload.base_url.trim().to_string();
        let explicit_key = payload.api_key.filter(|key| !key.trim().is_empty());
        // Credentials belong to an exact endpoint. Editing its URL must never silently send a
        // stored secret to a new destination, including automatic model discovery after saving.
        if !payload.clear_api_key
            && explicit_key.is_none()
            && existing.is_some_and(|entry| entry.has_api_key() && entry.base_url != base_url)
        {
            return Err(ApiError::BadRequest(
                "Changing base_url requires an explicit api_key or clear_api_key".into(),
            ));
        }
        let api_key = if payload.clear_api_key {
            None
        } else {
            explicit_key.or_else(|| existing.and_then(|entry| entry.api_key.clone()))
        };
        let entry = ProviderEntry {
            name: name.clone(),
            protocol: payload.protocol.trim().to_lowercase(),
            base_url,
            api_key,
            temperature: payload.temperature,
            max_tokens: payload.max_tokens,
            replay_reasoning: payload
                .replay_reasoning
                .unwrap_or_else(|| existing.is_none_or(|entry| entry.replay_reasoning)),
        };
        settings.providers.retain(|entry| entry.name != name);
        settings.providers.push(entry);
        Ok(())
    })?;

    // Fill the model catalog from the endpoint's own listing so the operator gets the provider's
    // models (and their context windows and modalities) without a second manual step. Failures are
    // logged by the helper and never fail the save.
    state.autofill_provider_models(&name).await;
    list_providers(State(state)).await
}

/// Handler for `POST /api/v1/providers/delete`.
async fn delete_provider(
    State(state): State<ApiState>,
    Json(payload): Json<DeleteProviderRequest>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let name = payload.name.trim().to_string();
    state
        .instances()
        .with_model_users(&name, |users| {
            if !users.is_empty() {
                return Err(ApiError::Conflict(format!(
                    "provider '{name}' is used by instance(s) {}; select another model there first",
                    users.join(", ")
                )));
            }
            state.update_node_settings(|settings| {
                let before = settings.providers.len();
                settings.providers.retain(|entry| entry.name != name);

                if settings.providers.len() == before {
                    return Err(ApiError::NotFound(format!(
                        "provider '{name}' is not configured"
                    )));
                }

                // Models belong to the endpoint that serves them: keeping them would leave references to a
                // credential that no longer exists.
                settings.models.retain(|spec| spec.provider != name);

                // The global default cannot outlive the endpoint that serves it. The node then has no default
                // model, which the console reports as such rather than silently picking another one.
                let serves_default = settings
                    .default_model
                    .as_deref()
                    .is_some_and(|model| ModelRef::parse(model).provider() == Some(name.as_str()));
                if serves_default {
                    settings.default_model = None;
                }

                Ok(())
            })
        })
        .await?;
    list_providers(State(state)).await
}

/// Handler for `POST /api/v1/providers/test`.
async fn test_provider(
    State(state): State<ApiState>,
    Json(payload): Json<TestProviderRequest>,
) -> Result<Json<TestProviderResponse>, ApiError> {
    let prompt = payload.prompt.unwrap_or_else(|| "ping".to_string());
    let settings = state.node_settings();
    let non_blank = |value: Option<String>| value.filter(|value| !value.trim().is_empty());

    let (protocol, base_url, api_key, candidate_models) = match non_blank(payload.provider) {
        Some(name) => {
            let entry = settings
                .providers
                .iter()
                .find(|entry| entry.name == name.trim())
                .cloned()
                .ok_or_else(|| {
                    ApiError::NotFound(format!("provider '{name}' is not configured"))
                })?;

            // The endpoint's own catalog and, when it serves the global default, that model.
            let mut candidates: Vec<String> = Vec::new();
            if let Some(default) = settings.default_model.as_deref() {
                let reference = ModelRef::parse(default);
                if reference.provider() == Some(entry.name.as_str()) {
                    candidates.push(reference.model().to_string());
                }
            }
            candidates.extend(
                settings
                    .models
                    .iter()
                    .filter(|spec| spec.provider == entry.name)
                    .map(|spec| spec.model.clone()),
            );

            let base_url = non_blank(payload.base_url)
                .map(|url| url.trim().to_string())
                .unwrap_or_else(|| entry.base_url.clone());
            let explicit_key = non_blank(payload.api_key);
            if base_url != entry.base_url && entry.has_api_key() && explicit_key.is_none() {
                return Err(ApiError::BadRequest(
                    "Testing a changed base_url requires an explicit api_key".into(),
                ));
            }
            (
                non_blank(payload.protocol)
                    .map(|protocol| protocol.trim().to_lowercase())
                    .unwrap_or(entry.protocol),
                base_url,
                explicit_key.or(entry.api_key),
                candidates,
            )
        }
        None => {
            let (Some(protocol), Some(base_url)) =
                (non_blank(payload.protocol), non_blank(payload.base_url))
            else {
                return Err(ApiError::BadRequest(
                    "Name a configured provider, or give both protocol and base_url to test"
                        .to_string(),
                ));
            };
            (
                protocol.trim().to_lowercase(),
                base_url.trim().to_string(),
                non_blank(payload.api_key),
                Vec::new(),
            )
        }
    };

    let model = non_blank(payload.model)
        .map(|model| model.trim().to_string())
        .or_else(|| candidate_models.into_iter().next())
        .ok_or_else(|| {
            ApiError::BadRequest(
                "No model to test with: pick one of this provider's models".to_string(),
            )
        })?;

    let provider = kanon_llm::build_provider(&protocol, base_url, api_key, model.clone())
        .map_err(ApiError::BadRequest)?;

    let start = Instant::now();
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::user(prompt)],
        tools: Vec::new(),
        temperature: Some(0.1),
        max_tokens: Some(32),
    };

    match tokio::time::timeout(std::time::Duration::from_secs(15), provider.chat(&request)).await {
        Ok(Ok(response)) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            Ok(Json(TestProviderResponse {
                status: "ok",
                latency_ms,
                model,
                reply: response.content,
                error: None,
            }))
        }
        Ok(Err(err)) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            Ok(Json(TestProviderResponse {
                status: "error",
                latency_ms,
                model,
                reply: None,
                error: Some(err.to_string()),
            }))
        }
        Err(_) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            Ok(Json(TestProviderResponse {
                status: "error",
                latency_ms,
                model,
                reply: None,
                error: Some("Provider connection timed out after 15 seconds".to_string()),
            }))
        }
    }
}

/// Request payload to fetch available model tags from a provider endpoint.
#[derive(Debug, Deserialize)]
pub struct FetchModelsRequest {
    /// Wire protocol (e.g. `openai`, `anthropic`).
    #[serde(default)]
    pub protocol: Option<String>,
    /// Provider API base URL.
    pub base_url: String,
    /// Optional API key credential.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Provider name used for discovery; defaults to one derived from the base URL.
    #[serde(default)]
    pub provider: Option<String>,
}

/// Response payload containing list of model IDs available on the provider.
#[derive(Debug, Serialize)]
pub struct FetchModelsResponse {
    /// Discovered model identifiers.
    pub models: Vec<String>,
    /// Full catalog candidates with endpoint-reported metadata.
    pub candidates: Vec<ModelSpec>,
}

/// Handler for `POST /api/v1/providers/models`.
///
/// Reads the endpoint's own model listing. The rich `candidates` are what the console stores in
/// the model catalog; `models` is kept as the flat identifier list older clients expect.
async fn fetch_models(
    State(_state): State<ApiState>,
    Json(payload): Json<FetchModelsRequest>,
) -> Result<Json<FetchModelsResponse>, ApiError> {
    let protocol = payload.protocol.as_deref().unwrap_or("openai");
    let base_url = payload.base_url.trim().trim_end_matches('/').to_string();
    if base_url.is_empty() {
        return Err(ApiError::BadRequest(
            "base_url must not be empty".to_string(),
        ));
    }

    let provider = payload
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derive_provider_name(&base_url));

    let entry = ProviderEntry {
        name: provider,
        protocol: protocol.to_string(),
        base_url,
        api_key: payload.api_key,
        temperature: None,
        max_tokens: None,
        replay_reasoning: true,
    };

    let candidates = crate::model_discovery::discover_models(&entry)
        .await
        .map_err(ApiError::Upstream)?;
    let models = candidates.iter().map(|spec| spec.model.clone()).collect();

    Ok(Json(FetchModelsResponse { models, candidates }))
}
