//! Node system configuration inspection (`GET /api/v1/system/config`) and the node-wide reply and
//! context policies (`/api/v1/system/reply-policy`, `/api/v1/system/context-policy`).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::Serialize;

use kanon_core::{ContextPolicy, ReplyPolicy};

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
        .route(
            "/api/v1/system/context-policy",
            get(get_context_policy).put(put_context_policy),
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
    /// LLM provider and agent configuration.
    pub llm: LlmConfigSection,
    /// Node-wide reply policy inherited by instances without an override.
    pub reply_policy: ReplyPolicy,
    /// Node-wide context-extras policy inherited by instances without an override.
    pub context_policy: ContextPolicy,
    /// Host operating system and architecture.
    pub environment: EnvironmentSection,
}

/// The node's conversational model: the global default model and how the agent is tuned.
#[derive(Debug, Serialize)]
pub struct LlmConfigSection {
    /// Whether the node has a model to answer with.
    pub configured: bool,
    /// Canonical `<provider>/<model-id>` the node answers with by default; empty when unset.
    pub model: String,
    /// Provider serving that model.
    pub provider: Option<String>,
    /// Context window of that model in tokens, when known.
    pub context_length: Option<u32>,
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
async fn system_config(State(state): State<ApiState>) -> Json<SystemConfigResponse> {
    // The agent is the ground truth for whether the node can answer and how it is tuned.
    let llm = match state.agent() {
        Some(agent) => {
            let cfg = agent.config();
            LlmConfigSection {
                configured: true,
                model: cfg.model_ref(),
                provider: cfg.provider.clone(),
                context_length: cfg.context_length,
                max_iterations: cfg.max_iterations,
                temperature: cfg.temperature,
                max_tokens: cfg.max_tokens,
            }
        }
        None => LlmConfigSection {
            configured: false,
            model: String::new(),
            provider: None,
            context_length: None,
            max_iterations: kanon_llm::AgentConfig::default().max_iterations,
            temperature: None,
            max_tokens: None,
        },
    };

    Json(SystemConfigResponse {
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
        llm,
        reply_policy: state.reply_policy().get(),
        context_policy: state.context_policy().get(),
        environment: EnvironmentSection {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            rust_edition: "2024",
        },
    })
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

/// Response describing the node-wide context-extras policy.
#[derive(Debug, Serialize)]
pub struct ContextPolicyResponse {
    /// The effective policy.
    pub policy: ContextPolicy,
}

/// Handler for `GET /api/v1/system/context-policy`.
async fn get_context_policy(State(state): State<ApiState>) -> Json<ContextPolicyResponse> {
    Json(ContextPolicyResponse {
        policy: state.context_policy().get(),
    })
}

/// Handler for `PUT /api/v1/system/context-policy`.
async fn put_context_policy(
    State(state): State<ApiState>,
    Json(policy): Json<ContextPolicy>,
) -> Result<Json<ContextPolicyResponse>, ApiError> {
    let mut settings = state.node_settings();
    settings.context_policy = policy;
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    tracing::info!(
        include_sender_id = policy.include_sender_id,
        include_timestamp = policy.include_timestamp,
        "Node-wide context policy updated"
    );
    Ok(get_context_policy(State(state)).await)
}
