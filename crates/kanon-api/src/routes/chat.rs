//! Online sandbox chat endpoint (`POST /api/v1/chat/completions`).
//!
//! Drives the same [`kanon_llm::Agent`] that serves live traffic, so console debugging exercises
//! the real prompt composition, persona binding, memory and tool-calling state machine rather
//! than a parallel "test-only" path.
//!
//! Two response modes are supported:
//! - `stream: false` (default) → a single JSON document with the final answer;
//! - `stream: true` → `text/event-stream` where each `data:` frame carries `delta`, `done` or
//!   `error` payloads, matching the SSE conventions already used by the LLM providers.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::StreamExt;
use kanon_llm::{Agent, AgentError, tool_router::ExecutedToolCall};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::metrics::MetricsRegistry;
use crate::state::{ApiState, default_agent_config};

/// Registers the sandbox chat route.
pub fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/chat/completions", post(completions))
}

/// Request payload for a sandbox completion.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    /// Session key to run the turn against (opaque, provided by the caller).
    pub session_id: String,
    /// User message text.
    pub message: String,
    /// Stream the answer as Server-Sent Events instead of returning a single JSON document.
    #[serde(default)]
    pub stream: bool,
    /// Expose plugin tools to the model and allow multi-turn tool calling.
    #[serde(default)]
    pub tools: bool,
    /// Optional persona override applied to the session before this turn.
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Optional model identifier (overriding active or default).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional protocol (e.g. `openai`, `anthropic`, `openai_responses`).
    #[serde(default)]
    pub protocol: Option<String>,
    /// Optional provider base URL.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Optional provider API key.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Non-streaming completion result.
#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    /// Session key the turn ran against.
    pub session_id: String,
    /// Final assistant text.
    pub content: String,
    /// Reasoning turns executed, including tool-calling rounds.
    pub turns: usize,
    /// Provider finish reason when reported.
    pub finish_reason: Option<String>,
    /// Tool calls executed during the turn.
    pub executed_tools: Vec<ToolCallView>,
    /// Persona in force for this session after the turn.
    pub persona_id: Option<String>,
}

/// Compact view of an executed tool call.
#[derive(Debug, Serialize)]
pub struct ToolCallView {
    /// Provider-allocated call identifier.
    pub call_id: String,
    /// Tool that was invoked.
    pub tool_name: String,
    /// Plugin that owns the tool (`native` for in-process tools).
    pub plugin_id: String,
    /// Host process that executed the tool (`in_process` for native tools).
    pub host_id: String,
    /// Whether execution succeeded.
    pub success: bool,
}

impl From<ExecutedToolCall> for ToolCallView {
    fn from(call: ExecutedToolCall) -> Self {
        Self {
            call_id: call.call_id,
            tool_name: call.tool_name,
            plugin_id: call.plugin_id,
            host_id: call.host_id,
            success: call.success,
        }
    }
}

/// Serves a sandbox completion in JSON or SSE form.
async fn completions(
    State(state): State<ApiState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        return Err(ApiError::BadRequest(
            "Field 'session_id' must not be empty".to_string(),
        ));
    }
    if request.message.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "Field 'message' must not be empty".to_string(),
        ));
    }

    let agent = resolve_agent(&state, &request)?;
    MetricsRegistry::incr(&state.observability().metrics.chat_completions);

    let writing = state.sessions().try_write(&session_id)?;
    apply_persona_override(&state, &session_id, request.persona_id.as_deref())?;

    // Plugin tools are aggregated from the live supervisor registry on every request so a
    // hot-reloaded plugin becomes callable without restarting the gateway.
    let mut hosts: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>> = state
        .supervisor()
        .get_all_hosts()
        .await
        .into_iter()
        .map(|host| host as Arc<dyn kanon_llm::tool_router::ToolHost>)
        .collect();

    // MCP servers reach the agent through the same slice. A sandbox session carries no instance
    // identifier, so the node-wide switch alone decides which servers are offered.
    hosts.extend(
        state
            .mcp()
            .hosts_for_instance(state.plugin_state(), None)
            .await,
    );

    if request.stream {
        writing
            .scope(stream_completion(
                state,
                agent,
                session_id,
                request.message,
                request.tools,
                hosts,
            ))
            .await
    } else {
        let output = writing
            .scope(async {
                if request.tools {
                    agent.run(&session_id, &request.message, &hosts).await
                } else {
                    agent.run_standalone(&session_id, &request.message).await
                }
            })
            .await
            .map_err(map_agent_error)?;

        Ok(Json(ChatCompletionResponse {
            session_id: session_id.clone(),
            content: output.content,
            turns: output.turns,
            finish_reason: output.finish_reason,
            executed_tools: output
                .executed_tools
                .into_iter()
                .map(ToolCallView::from)
                .collect(),
            persona_id: state.sessions().get_persona(&session_id),
        })
        .into_response())
    }
}

/// Streams a completion as Server-Sent Events.
async fn stream_completion(
    state: ApiState,
    agent: Arc<dyn kanon_llm::Agent>,
    session_id: String,
    message: String,
    tools: bool,
    hosts: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>>,
) -> Result<Response, ApiError> {
    let stream = if tools {
        agent.run_stream(&session_id, &message, &hosts).await
    } else {
        agent.run_standalone_stream(&session_id, &message).await
    }
    .map_err(map_agent_error)?;

    let sessions = state.sessions().clone();
    let sid = session_id.clone();

    let events = stream.map(move |chunk| {
        let payload = match chunk {
            Ok(chunk) if chunk.is_finished => serde_json::json!({
                "type": "done",
                "finish_reason": chunk.finish_reason,
                "persona_id": sessions.get_persona(&sid),
                "delta": chunk.delta_text,
                "reasoning": chunk.reasoning_text,
            }),
            Ok(chunk) => serde_json::json!({
                "type": "delta",
                "delta": chunk.delta_text,
                "reasoning": chunk.reasoning_text,
            }),
            Err(err) => serde_json::json!({
                "type": "error",
                "message": err.to_string(),
            }),
        };

        // Serialization of a `json!` value cannot fail; keep the SSE frame well-formed either way.
        Ok::<Event, Infallible>(Event::default().data(payload.to_string()))
    });

    // Keep-alive comments keep proxies and browsers from closing an idle reasoning stream while
    // the model is still thinking between tool-calling rounds.
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response())
}

/// Applies an optional persona override to the session before running the turn.
fn apply_persona_override(
    state: &ApiState,
    session_id: &str,
    persona_id: Option<&str>,
) -> Result<(), ApiError> {
    let Some(persona_id) = persona_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(());
    };

    if state.personas().get(persona_id).is_none() {
        return Err(ApiError::NotFound(format!(
            "Persona '{persona_id}' is not registered"
        )));
    }

    state.sessions().get_or_create(session_id);
    state.sessions().set_persona(session_id, persona_id);
    Ok(())
}

/// Maps agent failures onto management API errors.
fn map_agent_error(err: AgentError) -> ApiError {
    match err {
        AgentError::Busy(session) => ApiError::Conflict(format!("Session '{session}' is busy")),
        AgentError::Memory(message) => {
            ApiError::Internal(format!("Conversation memory failure: {message}"))
        }
        AgentError::Compaction(message) => {
            ApiError::Internal(format!("Conversation compaction failure: {message}"))
        }
        AgentError::ToolNotFound(tool) => ApiError::BadRequest(format!(
            "Tool '{tool}' is not registered by any plugin or native tool"
        )),
        AgentError::Rpc(status) => ApiError::Upstream(format!("Plugin IPC call failed: {status}")),
        AgentError::Gateway(gateway) => {
            ApiError::Upstream(format!("Model provider failure: {gateway}"))
        }
        // Console turns carry no stop signal today; should one be stopped, it is a conflict with
        // whoever stopped it, not a server fault.
        AgentError::Stopped => ApiError::Conflict("The turn was stopped before it finished".into()),
    }
}

/// Resolves an [`Agent`] runtime to drive completion for this request.
///
/// If dynamic provider coordinates are specified (`protocol` and `base_url`), an ephemeral
/// agent sharing the node's session manager, memory, and persona registry is constructed.
/// Otherwise, falls back to the node's configured agent, applying an optional model override.
fn resolve_agent(
    state: &ApiState,
    request: &ChatCompletionRequest,
) -> Result<Arc<dyn Agent>, ApiError> {
    if let (Some(proto), Some(url)) = (&request.protocol, &request.base_url) {
        let trimmed_url = url.trim().trim_end_matches('/').to_string();
        if !trimmed_url.is_empty() {
            let raw_model = request
                .model
                .as_deref()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("default");
            // Strip any provider prefix like "deepseek/deepseek-chat" -> "deepseek-chat"
            let model = match raw_model.split_once('/') {
                Some((_prov, actual_model)) if !actual_model.trim().is_empty() => {
                    actual_model.trim().to_string()
                }
                _ => raw_model.trim().to_string(),
            };
            let key = request.api_key.clone().filter(|k| !k.trim().is_empty());

            // Built through the shared factory so a protocol accepted here is accepted
            // everywhere, and so the ephemeral agent still shares the node's memory, sessions,
            // personas and trace bus.
            let provider = kanon_llm::build_provider(&proto, trimmed_url, key, model.clone())
                .map_err(ApiError::BadRequest)?;
            let mut config = default_agent_config(model);
            config.max_iterations = state
                .agent()
                .map(|agent| agent.config().max_iterations)
                .unwrap_or(config.max_iterations);

            return Ok(state.agent_factory().build_with(provider, config));
        }
    }

    if state.agent().is_some() {
        // Keep the provider prefix intact so overrides resolve to the selected endpoint. A bad
        // reference is an error; falling back could send a conversation to another provider.
        return state
            .agent_for_model(request.model.as_deref())
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "Requested model does not resolve to a configured provider".into(),
                )
            });
    }

    Err(ApiError::Unavailable(
        "No LLM provider is configured for this core; chat completions are disabled. 请在控制台「模型提供商」中配置提供商并选择模型。".to_string(),
    ))
}
