//! Model provider catalog, activation and connectivity testing (`/api/v1/providers`).
//!
//! # Why providers are named
//! A model is addressed as `<provider>/<model-id>`, so the node needs a directory that maps the
//! provider part to an endpoint, its protocol and its credential. That directory lives in
//! `data/system.json`, is edited here, and is applied to the running node through the shared agent
//! factory — the very next message uses it, with no restart.
//!
//! # One authoritative store, one apply path
//! Every mutation loads the current [`NodeSettings`], changes it, and hands the result to
//! `ApiState::apply_node_settings`, which validates, persists and publishes in that order. Keeping
//! a single apply path is what guarantees the console can never describe a directory the running
//! node does not have.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post, put};
use serde::{Deserialize, Serialize};

use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest};
use kanon_llm::{ModelCapabilities, ModelRef, ModelSpec, ProviderEntry};

use crate::error::ApiError;
use crate::llm_config::{NodeSettings, derive_provider_name, provider_presets};
use crate::state::ApiState;

/// Registers the providers endpoints.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/providers",
            get(list_providers).post(upsert_provider),
        )
        .route(
            "/api/v1/providers/active",
            put(activate_provider).delete(clear_active_provider),
        )
        .route("/api/v1/providers/default", put(set_default_provider))
        .route("/api/v1/providers/delete", post(delete_provider))
        .route("/api/v1/providers/test", post(test_provider))
        .route("/api/v1/providers/models", post(fetch_models))
}

/// Catalog response listing active and available providers and presets.
#[derive(Debug, Serialize)]
pub struct ProvidersCatalogResponse {
    /// Effective provider on this node.
    pub active: ActiveProviderInfo,
    /// Available wire protocols.
    pub available_protocols: Vec<ProtocolDescriptor>,
    /// Popular pre-configured provider templates.
    pub presets: Vec<ProviderPreset>,
    /// Every configured provider endpoint.
    pub providers: Vec<ProviderInfo>,
    /// Name of the endpoint used for unprefixed model references.
    pub default_provider: Option<String>,
    /// Canonical model reference the node answers with by default.
    pub default_model: Option<String>,
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
    /// Whether this is the default endpoint.
    pub is_default: bool,
}

impl ProviderInfo {
    /// Renders one entry for the console.
    fn from_entry(entry: &ProviderEntry, default: Option<&str>) -> Self {
        Self {
            name: entry.name.clone(),
            protocol: entry.protocol.clone(),
            base_url: entry.base_url.clone(),
            api_key_configured: entry.has_api_key(),
            temperature: entry.temperature,
            max_tokens: entry.max_tokens,
            is_default: default == Some(entry.name.as_str()),
        }
    }
}

/// Summary of the currently effective model provider.
#[derive(Debug, Serialize)]
pub struct ActiveProviderInfo {
    /// Whether a model runtime is loaded and available.
    pub configured: bool,
    /// Where the effective provider comes from: `console`, `env`, `runtime` or `none`.
    ///
    /// Reported explicitly so an operator can tell a provider saved through the console apart
    /// from one supplied by the environment bootstrap or injected in-process.
    pub source: &'static str,
    /// Wire protocol of the endpoint serving the default model.
    pub protocol: String,
    /// Canonical `<provider>/<model-id>` reference in effect.
    pub model: String,
    /// Model id actually sent upstream (the provider prefix stripped).
    pub upstream_model: String,
    /// Provider endpoint serving the default model.
    pub provider: Option<String>,
    /// Provider base URL if set.
    pub base_url: Option<String>,
    /// Whether an API credential is configured.
    pub api_key_configured: bool,
    /// Configured sampling temperature.
    pub temperature: Option<f32>,
    /// Configured maximum generation tokens.
    pub max_tokens: Option<u32>,
    /// Context window of the effective model, when known.
    pub context_length: Option<u32>,
    /// Capabilities of the effective model.
    pub capabilities: ModelCapabilities,
}

/// Request payload for `POST /api/v1/providers` (create or replace one named endpoint).
#[derive(Debug, Deserialize)]
pub struct UpsertProviderRequest {
    /// Provider name; also the prefix of every model reference it serves.
    pub name: String,
    /// Wire protocol: `openai` (alias `openai_chat`), `openai_responses` or `anthropic`.
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
    /// Whether this endpoint should become the default one.
    #[serde(default)]
    pub make_default: bool,
    /// Default model reference to use when `make_default` is set.
    #[serde(default)]
    pub model: Option<String>,
}

/// Request payload for `PUT /api/v1/providers/active`.
///
/// Mirrors the `KANON_LLM_*` environment variables one-for-one; `temperature` and `max_tokens`
/// are optional agent tuning that the environment bootstrap cannot express.
#[derive(Debug, Deserialize)]
pub struct ActivateProviderRequest {
    /// Wire protocol: `openai` (alias `openai_chat`), `openai_responses` or `anthropic`.
    pub protocol: String,
    /// Provider base URL, e.g. `https://api.xiaomimimo.com/v1`.
    pub base_url: String,
    /// Default model identifier, e.g. `deepseek-chat` or `xiaomi/mimo-v2.6-flash`.
    pub model: String,
    /// Provider credential. Optional for local runtimes that need none.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Sampling temperature applied to the node's agent.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Maximum generation tokens applied to the node's agent.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Optional endpoint name; defaults to the existing default endpoint's name, or one derived
    /// from the base URL (so `api.xiaomimimo.com` becomes `xiaomi`).
    #[serde(default)]
    pub provider_name: Option<String>,
}

/// Request payload for `PUT /api/v1/providers/default`.
#[derive(Debug, Deserialize)]
pub struct SetDefaultProviderRequest {
    /// Endpoint to make default.
    pub provider: String,
    /// Model reference to answer with; prefixed with the provider when it carries none.
    #[serde(default)]
    pub model: Option<String>,
}

/// Request payload for `POST /api/v1/providers/delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteProviderRequest {
    /// Endpoint to remove.
    pub name: String,
}

/// Response payload after a provider mutation.
#[derive(Debug, Serialize)]
pub struct ActivateProviderResponse {
    /// Whether the change was applied to the running node.
    pub applied: bool,
    /// Human-readable confirmation, or the reason nothing changed.
    pub message: String,
    /// Effective provider state after the call.
    pub active: ActiveProviderInfo,
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

/// Pre-configured provider preset for quick configuration.
#[derive(Debug, Serialize)]
pub struct ProviderPreset {
    /// Preset identifier; also the provider name it creates.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Protocol identifier.
    pub protocol: &'static str,
    /// Provider API base URL.
    pub base_url: &'static str,
}

/// Request payload to test provider connectivity.
#[derive(Debug, Deserialize)]
pub struct TestProviderRequest {
    /// Protocol wire format: `openai`, `openai_responses`, or `anthropic` (defaults to active).
    pub protocol: Option<String>,
    /// Target base URL (defaults to active).
    pub base_url: Option<String>,
    /// API key credential (defaults to active).
    pub api_key: Option<String>,
    /// Model tag to query (defaults to active or `gpt-4o-mini`).
    pub model: Option<String>,
    /// Custom test prompt (defaults to "ping").
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

/// Renders the effective model routing state.
///
/// The agent slot is the ground truth for *whether* the node can answer and with which tuning
/// values; the persisted directory supplies the provenance (protocol, base URL, credential). The
/// two can legitimately disagree — an agent can be injected programmatically with no directory at
/// all (`source: "runtime"`) — which is why neither source alone is sufficient.
pub(crate) fn active_provider_info(state: &ApiState) -> Result<ActiveProviderInfo, ApiError> {
    let settings = state.node_settings();
    let agent = state.agent();
    let configured = agent.is_some();

    if !configured {
        return Ok(ActiveProviderInfo {
            configured: false,
            source: "none",
            protocol: "openai".to_string(),
            model: String::new(),
            upstream_model: String::new(),
            provider: None,
            base_url: None,
            api_key_configured: false,
            temperature: None,
            max_tokens: None,
            context_length: None,
            capabilities: ModelCapabilities::default(),
        });
    }

    let agent = agent.expect("agent presence checked above");
    let config = agent.config();
    let model_ref = config.model_ref();
    let reference = ModelRef::parse(&model_ref);
    let provider_name = config
        .provider
        .clone()
        .or_else(|| reference.provider().map(str::to_string));
    let entry = provider_name
        .as_deref()
        .and_then(|name| settings.providers.iter().find(|entry| entry.name == name));
    let spec = state.agent_factory().models().settings_for(&reference);

    let source = if !settings.has_providers() {
        // An agent with no persisted directory was injected in-process; reporting `console` or
        // `env` here would misattribute it.
        "runtime"
    } else {
        match settings.source {
            crate::llm_config::SettingsSource::Console => "console",
            crate::llm_config::SettingsSource::Environment => "env",
        }
    };

    Ok(ActiveProviderInfo {
        configured: true,
        source,
        protocol: entry
            .map(|entry| entry.protocol.clone())
            .unwrap_or_else(|| "openai".to_string()),
        model: model_ref,
        upstream_model: config.default_model.clone(),
        provider: provider_name,
        base_url: entry.map(|entry| entry.base_url.clone()),
        api_key_configured: entry.is_some_and(ProviderEntry::has_api_key),
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        context_length: config.context_length.or(spec.context_length),
        capabilities: spec.capabilities,
    })
}

/// Handler for `GET /api/v1/providers`.
async fn list_providers(
    State(state): State<ApiState>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let active = active_provider_info(&state)?;
    let settings = state.node_settings();
    let default_provider = settings.default_provider.as_deref();

    let available_protocols = vec![
        ProtocolDescriptor {
            id: "openai",
            name: "OpenAI / Compatible (v1/chat/completions)",
            default_base_url: "https://api.openai.com/v1",
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

    let presets = provider_presets()
        .into_iter()
        .map(|preset| ProviderPreset {
            id: preset.id,
            name: preset.name,
            protocol: preset.protocol,
            base_url: preset.base_url,
        })
        .collect();

    Ok(Json(ProvidersCatalogResponse {
        active,
        available_protocols,
        presets,
        providers: settings
            .providers
            .iter()
            .map(|entry| ProviderInfo::from_entry(entry, default_provider))
            .collect(),
        default_provider: settings.default_provider,
        default_model: settings.default_model,
    }))
}

/// Handler for `PUT /api/v1/providers/active`.
///
/// Legacy single-endpoint activation: it registers (or replaces) one named endpoint and makes it
/// the default. Order is *validate → persist → apply*, so an unreachable protocol or malformed URL
/// never reaches disk and the running node keeps serving its previous directory on failure.
async fn activate_provider(
    State(state): State<ApiState>,
    Json(payload): Json<ActivateProviderRequest>,
) -> Result<Json<ActivateProviderResponse>, ApiError> {
    let protocol = payload.protocol.trim().to_lowercase();
    let base_url = payload.base_url.trim().to_string();
    let model = payload.model.trim().to_string();

    if model.is_empty() {
        return Err(ApiError::BadRequest(
            "Model identifier must not be empty".to_string(),
        ));
    }

    let mut settings = state.node_settings();
    let name = payload
        .provider_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| settings.default_provider.clone())
        .unwrap_or_else(|| derive_provider_name(&base_url));

    let existing = settings
        .providers
        .iter()
        .find(|entry| entry.name == name)
        .cloned();
    // An omitted credential keeps the stored one, which is what lets the console edit a URL
    // without forcing the operator to retype a secret it never received.
    let api_key = payload
        .api_key
        .filter(|key| !key.trim().is_empty())
        .or_else(|| existing.as_ref().and_then(|entry| entry.api_key.clone()));

    let entry = ProviderEntry {
        name: name.clone(),
        protocol: protocol.clone(),
        base_url: base_url.clone(),
        api_key: api_key.clone(),
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
    };

    // Validate the endpoint description before it can reach disk: an unsupported protocol or a
    // scheme-less URL is exactly the mistake this ordering exists to catch.
    kanon_llm::build_provider(&protocol, base_url, api_key, model.clone())
        .map_err(ApiError::BadRequest)?;

    settings.providers.retain(|entry| entry.name != name);
    settings.providers.push(entry);
    settings.default_provider = Some(name.clone());
    settings.default_model = Some(qualify_model_reference(&model, &name, &settings));

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    // Fill the model catalog from the endpoint's own listing so the operator gets the provider's
    // models (and their context windows and modalities) without a second manual step. Failures are
    // logged by the helper and never fail the activation.
    state.autofill_provider_models(&name).await;

    let active = active_provider_info(&state)?;
    tracing::info!(
        provider = %name,
        protocol = %protocol,
        model = %active.model,
        "Model provider activated through the control plane; effective immediately"
    );

    Ok(Json(ActivateProviderResponse {
        applied: true,
        message: format!("Provider '{name}' activated; new messages use it immediately"),
        active,
    }))
}

/// Handler for `POST /api/v1/providers`.
async fn upsert_provider(
    State(state): State<ApiState>,
    Json(payload): Json<UpsertProviderRequest>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::BadRequest(
            "Provider name must not be empty".to_string(),
        ));
    }
    let protocol = payload.protocol.trim().to_lowercase();
    let base_url = payload.base_url.trim().to_string();

    let mut settings = state.node_settings();
    let existing = settings
        .providers
        .iter()
        .find(|entry| entry.name == name)
        .cloned();

    let api_key = if payload.clear_api_key {
        None
    } else {
        payload
            .api_key
            .filter(|key| !key.trim().is_empty())
            .or_else(|| existing.as_ref().and_then(|entry| entry.api_key.clone()))
    };

    let entry = ProviderEntry {
        name: name.clone(),
        protocol: protocol.clone(),
        base_url: base_url.clone(),
        api_key: api_key.clone(),
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
    };

    let model = payload
        .model
        .as_deref()
        .map(|model| qualify_model_reference(model, &name, &settings));

    kanon_llm::build_provider(
        &protocol,
        base_url,
        api_key,
        model.clone().unwrap_or_default(),
    )
    .map_err(ApiError::BadRequest)?;

    if payload.make_default
        && model.is_none()
        && settings.default_provider.as_deref() != Some(&name)
    {
        return Err(ApiError::BadRequest(format!(
            "provider '{name}' cannot become the default without a model reference"
        )));
    }

    settings.providers.retain(|entry| entry.name != name);
    settings.providers.push(entry);
    // Configuring the first endpoint together with a model makes it the default: leaving that
    // unset would leave the console showing a provider the node does not actually use.
    let becomes_default =
        payload.make_default || (settings.default_provider.is_none() && model.is_some());
    if becomes_default {
        settings.default_provider = Some(name.clone());
        if let Some(model) = model {
            settings.default_model = Some(model);
        }
    }

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    state.autofill_provider_models(&name).await;
    list_providers(State(state)).await
}

/// Handler for `PUT /api/v1/providers/default`.
async fn set_default_provider(
    State(state): State<ApiState>,
    Json(payload): Json<SetDefaultProviderRequest>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let name = payload.provider.trim().to_string();
    let mut settings = state.node_settings();

    if !settings.providers.iter().any(|entry| entry.name == name) {
        return Err(ApiError::NotFound(format!(
            "provider '{name}' is not configured"
        )));
    }

    match payload
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        Some(model) => {
            settings.default_model = Some(qualify_model_reference(model, &name, &settings))
        }
        None => {
            // Keep the current model only when it already belongs to the new default provider;
            // otherwise the node would send one endpoint's model id to another endpoint.
            let belongs = settings
                .default_model
                .as_deref()
                .map(|model| ModelRef::parse(model).provider() == Some(name.as_str()))
                .unwrap_or(false);
            if !belongs {
                return Err(ApiError::BadRequest(format!(
                    "a model reference served by '{name}' is required when changing the default provider"
                )));
            }
        }
    }

    settings.default_provider = Some(name);
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    list_providers(State(state)).await
}

/// Handler for `POST /api/v1/providers/delete`.
async fn delete_provider(
    State(state): State<ApiState>,
    Json(payload): Json<DeleteProviderRequest>,
) -> Result<Json<ProvidersCatalogResponse>, ApiError> {
    let name = payload.name.trim().to_string();
    let mut settings = state.node_settings();
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

    if settings.default_provider.as_deref() == Some(name.as_str()) {
        settings.default_provider = settings.providers.first().map(|entry| entry.name.clone());
        settings.default_model = None;
    }

    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    list_providers(State(state)).await
}

/// Handler for `DELETE /api/v1/providers/active`.
///
/// Removes every configured endpoint and disables chat on the running node. The model catalog is
/// cleared with them; the reply policy is preserved because it is an unrelated preference.
async fn clear_active_provider(
    State(state): State<ApiState>,
) -> Result<Json<ActivateProviderResponse>, ApiError> {
    state.clear_node_providers().map_err(ApiError::BadRequest)?;

    tracing::info!("LLM provider cleared through the control plane; chat is now disabled");

    Ok(Json(ActivateProviderResponse {
        applied: true,
        message: "Provider cleared; chat completions and conversational routing are disabled"
            .to_string(),
        active: active_provider_info(&state)?,
    }))
}

/// Handler for `POST /api/v1/providers/test`.
async fn test_provider(
    State(state): State<ApiState>,
    Json(payload): Json<TestProviderRequest>,
) -> Result<Json<TestProviderResponse>, ApiError> {
    let prompt = payload.prompt.unwrap_or_else(|| "ping".to_string());
    let settings = state.node_settings();

    let (provider, model): (Arc<dyn LlmProvider>, String) = match (
        payload.protocol,
        payload.base_url,
    ) {
        (Some(proto), Some(url)) => {
            let raw_model = payload.model.unwrap_or_else(|| "gpt-4o-mini".to_string());
            // A test request must be sent to the endpoint being tested, so any provider prefix is
            // stripped only when it names that endpoint.
            let model = ModelRef::parse(&raw_model);
            let upstream = match model.provider() {
                Some(prefix) if prefix != derive_provider_name(&url) => model.canonical(),
                _ => model.model().to_string(),
            };
            let provider =
                kanon_llm::build_provider(&proto, url, payload.api_key, upstream.clone())
                    .map_err(ApiError::BadRequest)?;
            (provider, upstream)
        }
        _ => {
            let active = active_provider_info(&state)?;
            if !active.configured {
                return Err(ApiError::BadRequest(
                    "No LLM provider is configured on this node; please specify protocol and base_url to test".to_string(),
                ));
            }

            let entry = active
                .provider
                .as_deref()
                .and_then(|name| settings.providers.iter().find(|entry| entry.name == name))
                .cloned();

            match entry {
                Some(entry) => {
                    let model = payload.model.unwrap_or(active.upstream_model);
                    let provider = kanon_llm::build_provider(
                        &entry.protocol,
                        entry.base_url,
                        entry.api_key,
                        model.clone(),
                    )
                    .map_err(ApiError::BadRequest)?;
                    (provider, model)
                }
                None => {
                    // An agent injected in-process has no persisted endpoint description; its own
                    // client is the only truthful thing to probe.
                    let agent = state.agent().ok_or_else(|| {
                        ApiError::Unavailable(
                            "No LLM provider is configured on this node".to_string(),
                        )
                    })?;
                    let model = payload.model.unwrap_or(active.upstream_model);
                    (agent.provider().clone(), model)
                }
            }
        }
    };

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
    };

    let candidates = crate::model_discovery::discover_models(&entry)
        .await
        .map_err(ApiError::Upstream)?;
    let models = candidates.iter().map(|spec| spec.model.clone()).collect();

    Ok(Json(FetchModelsResponse { models, candidates }))
}

/// Qualifies a model reference with a provider name unless it already names a configured one.
///
/// `deepseek-chat` becomes `xiaomi/deepseek-chat` for the `xiaomi` default; an aggregator's
/// `anthropic/claude-3.5-sonnet` is kept verbatim when `anthropic` is not a configured endpoint,
/// because there the slash is part of the upstream model id.
fn qualify_model_reference(model: &str, provider: &str, settings: &NodeSettings) -> String {
    let reference = ModelRef::parse(model);
    match reference.provider() {
        Some(prefix)
            if settings.providers.iter().any(|entry| entry.name == prefix)
                || prefix == provider =>
        {
            reference.canonical()
        }
        _ => format!("{provider}/{}", reference.model()),
    }
}
