//! Node system configuration inspection (`GET /api/v1/system/config`) and the node-wide reply
//! policy (`/api/v1/system/reply-policy`).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::Serialize;

use kanon_core::ReplyPolicy;

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the system configuration routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/system/config", get(system_config))
        .route(
            "/api/v1/system/reply-policy",
            get(get_reply_policy).put(put_reply_policy),
        )
}

/// Comprehensive node and system configuration payload.
#[derive(Debug, Serialize)]
pub struct SystemConfigResponse {
    /// Gateway and microkernel version.
    pub version: String,
    /// Seconds elapsed since the node process started.
    pub uptime_seconds: u64,
    /// IPC socket path for the core kernel.
    pub ipc_socket_path: String,
    /// Runtime socket directory.
    pub run_dir: String,
    /// Persistent data directory.
    pub data_dir: String,
    /// Session memory sliding window depth.
    pub memory_window: usize,
    /// Platform webhook adapter status.
    pub webhook: WebhookConfigSection,
    /// LLM provider and agent configuration.
    pub llm: LlmConfigSection,
    /// Node-wide reply policy inherited by instances without an override.
    pub reply_policy: ReplyPolicy,
    /// Host operating system and architecture.
    pub environment: EnvironmentSection,
}

/// Webhook adapter configuration details.
#[derive(Debug, Serialize)]
pub struct WebhookConfigSection {
    /// Inbound platform identifier.
    pub platform: String,
    /// Whether outbound webhook delivery callback is configured.
    pub callback_configured: bool,
    /// Outbound callback URL.
    pub callback_url: Option<String>,
    /// Whether HMAC-SHA256 signature verification is active.
    pub signature_verification: bool,
}

/// LLM provider configuration details.
#[derive(Debug, Serialize)]
pub struct LlmConfigSection {
    /// Whether an LLM provider is active.
    pub configured: bool,
    /// Where the effective provider comes from: `console`, `env` or `none`.
    pub source: &'static str,
    /// Active protocol identifier.
    pub protocol: String,
    /// Default model tag.
    pub model: String,
    /// Base URL if configured.
    pub base_url: Option<String>,
    /// Whether an API key credential was supplied.
    pub api_key_configured: bool,
    /// Maximum tool reasoning loop iterations.
    pub max_iterations: usize,
    /// Sampling temperature if configured.
    pub temperature: Option<f32>,
    /// Max generation tokens limit if configured.
    pub max_tokens: Option<u32>,
}

/// Host environment details.
#[derive(Debug, Serialize)]
pub struct EnvironmentSection {
    /// Target operating system.
    pub os: &'static str,
    /// Target CPU architecture.
    pub arch: &'static str,
    /// Rust edition used to build the binary.
    pub rust_edition: &'static str,
}

/// Handler for `GET /api/v1/system/config`.
async fn system_config(
    State(state): State<ApiState>,
) -> Result<Json<SystemConfigResponse>, crate::error::ApiError> {
    let webhook_adapter = state.supervisor().adapters().get("webhook").await;
    let callback_url_env = std::env::var("KANON_WEBHOOK_CALLBACK_URL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let secret_env = std::env::var("KANON_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty());

    let webhook = WebhookConfigSection {
        platform: std::env::var("KANON_WEBHOOK_PLATFORM").unwrap_or_else(|_| "webhook".to_string()),
        callback_configured: webhook_adapter
            .as_ref()
            .map(|w| w.is_connected())
            .unwrap_or(false)
            || callback_url_env.is_some(),
        callback_url: callback_url_env,
        signature_verification: secret_env.is_some(),
    };

    // Report the *effective* provider (console selection first, environment bootstrap second)
    // rather than merely echoing the environment, which may have been overridden at runtime.
    // A failure to read the persisted document is surfaced rather than masked by a default.
    let active = super::providers::active_provider_info(&state)?;

    let (max_iterations, temperature, max_tokens) = match state.agent() {
        Some(agent) => {
            let cfg = agent.config();
            (cfg.max_iterations, cfg.temperature, cfg.max_tokens)
        }
        None => (5, active.temperature, active.max_tokens),
    };

    let llm = LlmConfigSection {
        configured: active.configured,
        source: active.source,
        protocol: active.protocol,
        model: active.model,
        base_url: active.base_url,
        api_key_configured: active.api_key_configured,
        max_iterations,
        temperature,
        max_tokens,
    };

    Ok(Json(SystemConfigResponse {
        version: state.version().to_string(),
        uptime_seconds: state.started_at().elapsed().as_secs(),
        ipc_socket_path: state
            .supervisor()
            .core_sock_path()
            .to_string_lossy()
            .to_string(),
        run_dir: state.supervisor().run_dir().to_string_lossy().to_string(),
        data_dir: state
            .config_store()
            .base_dir()
            .to_string_lossy()
            .to_string(),
        memory_window: 40,
        webhook,
        llm,
        reply_policy: state.reply_policy().get(),
        environment: EnvironmentSection {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            rust_edition: "2024",
        },
    }))
}

/// Response describing the node-wide reply policy.
#[derive(Debug, Serialize)]
pub struct ReplyPolicyResponse {
    /// The effective policy.
    pub policy: ReplyPolicy,
    /// Human-readable rendering of the policy, for console display.
    pub description: String,
}

/// Handler for `GET /api/v1/system/reply-policy`.
async fn get_reply_policy(State(state): State<ApiState>) -> Json<ReplyPolicyResponse> {
    let policy = state.reply_policy().get();
    Json(ReplyPolicyResponse {
        description: policy.describe(),
        policy,
    })
}

/// Handler for `PUT /api/v1/system/reply-policy`.
///
/// The policy is applied through the same path as every other node setting, so the in-memory
/// snapshot, the running pipeline and `data/system.json` can never disagree.
async fn put_reply_policy(
    State(state): State<ApiState>,
    Json(policy): Json<ReplyPolicy>,
) -> Result<Json<ReplyPolicyResponse>, ApiError> {
    let mut settings = state.node_settings();
    settings.reply_policy = policy;
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    tracing::info!(policy = %policy.describe(), "Node-wide reply policy updated");
    Ok(get_reply_policy(State(state)).await)
}
