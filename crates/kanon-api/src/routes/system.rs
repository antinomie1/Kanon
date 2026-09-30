//! Node system configuration inspection (`GET /api/v1/system/config`) and the node-wide reply,
//! context and notice policies (`/api/v1/system/reply-policy`, `/api/v1/system/context-policy`,
//! `/api/v1/system/event-policy`).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::Serialize;

use kanon_core::{BashPolicy, CommandPolicy, ContextPolicy, EventPolicy, ReplyPolicy};

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the system configuration routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/system/config", get(system_config))
        .route(
            "/api/v1/tools/bash/policy",
            get(get_bash_policy).put(put_bash_policy),
        )
        .route("/api/v1/tools/bash/reset", post(reset_bash_sandbox))
        .route(
            "/api/v1/system/reply-policy",
            get(get_reply_policy).put(put_reply_policy),
        )
        .route(
            "/api/v1/system/context-policy",
            get(get_context_policy).put(put_context_policy),
        )
        .route(
            "/api/v1/system/event-policy",
            get(get_event_policy).put(put_event_policy),
        )
        .route(
            "/api/v1/system/command-policy",
            get(get_command_policy).put(put_command_policy),
        )
}

/// Explicit operator action; normal tool calls never discard a persistent container.
async fn reset_bash_sandbox(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tool = state
        .bash_tool()
        .ok_or_else(|| ApiError::Unavailable("Bash runtime is not registered".into()))?;
    tool.reset_sandbox().await.map_err(ApiError::Conflict)?;
    Ok(Json(serde_json::json!({"reset":true})))
}

/// Returns the Bash switch and execution backend without altering the static tool catalog.
async fn get_bash_policy(State(state): State<ApiState>) -> Json<BashPolicy> {
    Json(state.bash_policy().get())
}

/// Persists Bash settings before publishing them to the execution gate.
///
/// Who may run Bash is not part of this document: it is the explicit administrator list of the
/// command policy, so there is exactly one place that grants elevated rights.
async fn put_bash_policy(
    State(state): State<ApiState>,
    Json(policy): Json<BashPolicy>,
) -> Result<Json<BashPolicy>, ApiError> {
    policy.validate().map_err(ApiError::BadRequest)?;
    // Held until the new settings are applied, so no command can start on the old Docker
    // endpoint after the check that it owns no container there.
    let _runtime_guard = match state.bash_tool() {
        Some(tool) => tool
            .prepare_policy_update(&policy)
            .await
            .map_err(ApiError::Conflict)?,
        None => None,
    };
    let mut settings = state.node_settings();
    settings.bash_policy = policy;
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;
    Ok(get_bash_policy(State(state)).await)
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
    /// Node-wide notice policy.
    pub event_policy: EventPolicy,
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
        event_policy: state.event_policy().get(),
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

/// Response describing the node-wide notice policy.
#[derive(Debug, Serialize)]
pub struct EventPolicyResponse {
    /// The effective policy.
    pub policy: EventPolicy,
}

/// Handler for `GET /api/v1/system/event-policy`.
async fn get_event_policy(State(state): State<ApiState>) -> Json<EventPolicyResponse> {
    Json(EventPolicyResponse {
        policy: state.event_policy().get(),
    })
}

/// Handler for `PUT /api/v1/system/event-policy`.
///
/// Applied and persisted through the node settings path, like the reply and context policies.
async fn put_event_policy(
    State(state): State<ApiState>,
    Json(policy): Json<EventPolicy>,
) -> Result<Json<EventPolicyResponse>, ApiError> {
    let mut settings = state.node_settings();
    settings.event_policy = policy;
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    tracing::info!(?policy, "Node-wide event policy updated");
    Ok(get_event_policy(State(state)).await)
}

/// Response describing the node-wide command permissions.
#[derive(Debug, Serialize)]
pub struct CommandPolicyResponse {
    /// The effective policy, normalized.
    pub policy: CommandPolicy,
}

/// Handler for `GET /api/v1/system/command-policy`.
async fn get_command_policy(State(state): State<ApiState>) -> Json<CommandPolicyResponse> {
    Json(CommandPolicyResponse {
        policy: state.command_policy().get(),
    })
}

/// Handler for `PUT /api/v1/system/command-policy`.
///
/// The policy is normalized (command names lose their slash and case) and validated before it is
/// applied and persisted, so the stored document is exactly what the pipeline enforces.
async fn put_command_policy(
    State(state): State<ApiState>,
    Json(policy): Json<CommandPolicy>,
) -> Result<Json<CommandPolicyResponse>, ApiError> {
    let policy = policy.prepare().map_err(ApiError::BadRequest)?;
    let mut settings = state.node_settings();
    settings.command_policy = policy;
    state
        .apply_node_settings(settings)
        .map_err(ApiError::BadRequest)?;

    tracing::info!("Node-wide command policy updated");
    Ok(get_command_policy(State(state)).await)
}
