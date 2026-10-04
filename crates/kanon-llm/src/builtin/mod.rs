//! Kanon's built-in agent engine.
//!
//! [`BuiltinAgent`] is the node's implementation of [`Agent`]: a tool loop over one
//! [`LlmProvider`], with
//! - conversational memory implementing [`Memory`] (append-only, compacted into summaries);
//! - native in-process tools implementing [`AgentTool`] (zero IPC overhead);
//! - cross-process tool calling via [`ToolHost`];
//! - static persona composition and lifecycle hooks for skills, guardrails and tracing.

mod builder;
pub use builder::*;
mod prompt;
mod turn;
use async_trait::async_trait;
use std::sync::Arc;

use kanon_proto::v1::{ToolCallRequest, tool_call_request, tool_call_response};

use crate::agent::{Agent, AgentConfig, AgentHook, AgentOutput, AgentTool, TurnOptions};
use crate::compaction::{COMPACTION_INSTRUCTION, CompactionPolicy, ends_cleanly, summary_block};
use crate::error::{AgentError, GatewayError, MemoryError};
use crate::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolDefinition,
};
use crate::gateway::{ChatChunk, ChatChunkStream, LlmProvider};
use crate::layout::normalize_request;
use crate::memory::{InMemory, Memory, MemorySnapshot};
use crate::prompt::Persona;
use crate::session::{SessionWriteGuard, SessionWriters};
use crate::stop::{StopSignal, unless_stopped};
use crate::tool_router::{
    ExecutedToolCall, ToolAttachment, ToolHost, ensure_unique_tool_names, json_to_prost_struct,
    prost_struct_to_json, resolve_tools,
};
use dashmap::DashSet;
use tokio_stream::StreamExt;

/// Result recorded for a tool call that a stopped turn left without one.
///
/// Worded for the model, which reads it in later turns: the call produced nothing because the
/// turn was stopped, not because the tool failed.
pub const STOPPED_TOOL_RESULT: &str =
    "Not completed: the turn was stopped before this tool call finished.";

/// Result recorded for a tool call left without one when its turn failed.
pub const FAILED_TOOL_RESULT: &str =
    "Not completed: the turn failed before this tool call finished.";

/// Kanon's own agent: a tool loop over one model provider.
///
/// Encapsulates model backend, memory store, native tools, lifecycle hooks, and reasoning policies.
///
/// Cloning is cheap (every heavy part is behind an `Arc`) and is how a conversation compaction
/// outlives the turn that scheduled it: the clone shares the memory, provider and hooks.
#[derive(Clone)]
pub struct BuiltinAgent {
    /// Identifier or role name of this agent.
    name: String,
    /// Static instructions placed after the persona and before ordinary lifecycle hooks.
    system_prompt: Option<String>,
    /// Provider backend for LLM completions.
    provider: Arc<dyn LlmProvider>,
    /// Pluggable memory backend for conversational history.
    memory: Arc<dyn Memory>,
    /// Optional session manager tracking lifecycle, turns, and metadata.
    session_manager: Option<Arc<crate::session::SessionManager>>,
    /// Optional persona registry for dynamic persona resolution.
    persona_registry: Option<Arc<crate::prompt::PersonaRegistry>>,
    /// Native in-process tools directly callable without IPC overhead.
    tools: Vec<Arc<dyn AgentTool>>,
    /// Lifecycle interception hooks.
    hooks: Vec<Arc<dyn AgentHook>>,
    /// Operational configuration.
    config: AgentConfig,
    /// Standalone turns, stream producers and compactions share one writer registry. Managed
    /// agents use the session manager's registry and need no second lock layer.
    standalone_writers: Option<Arc<SessionWriters>>,
    /// Sessions with a compaction in flight, so a burst of turns schedules one, not many.
    compacting: Arc<DashSet<String>>,
}

/// A completed turn's exact history and effective system prefix, captured while it owns the writer.
struct PreparedCompaction {
    snapshot: MemorySnapshot,
    prefix: Vec<ChatMessage>,
}

/// The execution target captured with the definition advertised for this turn.
enum ToolTarget {
    Native(usize),
    Plugin {
        host_index: usize,
        plugin_id: String,
        tool_name: String,
    },
}

/// Definitions and dispatch share one snapshot even when a host refreshes its metadata mid-turn.
#[derive(Default)]
struct TurnTools {
    definitions: Vec<ToolDefinition>,
    targets: std::collections::HashMap<String, ToolTarget>,
}

impl BuiltinAgent {
    /// Returns a new fluent builder for constructing a [`BuiltinAgent`].
    pub fn builder(name: impl Into<String>, provider: Arc<dyn LlmProvider>) -> AgentBuilder {
        AgentBuilder::new(name, provider)
    }

    /// Reference to the optional persona registry.
    pub fn persona_registry(&self) -> Option<&Arc<crate::prompt::PersonaRegistry>> {
        self.persona_registry.as_ref()
    }

    /// Registered native tools.
    pub fn tools(&self) -> &[Arc<dyn AgentTool>] {
        &self.tools
    }

    /// Registered lifecycle hooks.
    pub fn hooks(&self) -> &[Arc<dyn AgentHook>] {
        &self.hooks
    }

    /// Claims this turn's writer before any history append, preserving managed delegation.
    fn turn_writer(&self, session_id: &str) -> Result<SessionWriteGuard, AgentError> {
        match &self.session_manager {
            Some(sessions) => sessions.agent_write(session_id),
            None => self
                .standalone_writers
                .as_ref()
                .expect("standalone agents initialize their writer registry")
                .try_write(session_id),
        }
        .map_err(Into::into)
    }

    /// Merges native and plugin/MCP tools; final normalization follows the request hooks.
    fn collect_tools(
        &self,
        hosts: &[Arc<dyn ToolHost>],
        without_tools: bool,
    ) -> Result<TurnTools, AgentError> {
        let mut tools = TurnTools::default();
        if !self.config.tool_calling || without_tools {
            return Ok(tools);
        }
        for (index, native) in self.tools.iter().enumerate() {
            let definition = native.definition();
            tools
                .targets
                .insert(definition.name.clone(), ToolTarget::Native(index));
            tools.definitions.push(definition);
        }
        for resolved in resolve_tools(hosts)? {
            tools.targets.insert(
                resolved.definition.name.clone(),
                ToolTarget::Plugin {
                    host_index: resolved.host_index,
                    plugin_id: resolved.plugin_id,
                    tool_name: resolved.tool_name,
                },
            );
            tools.definitions.push(resolved.definition);
        }
        // Never publish a map in which insertion silently selected one of two same-named tools.
        ensure_unique_tool_names(tools.definitions.iter().map(|tool| tool.name.as_str()))?;
        Ok(tools)
    }
}

#[async_trait]
impl Agent for BuiltinAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn config(&self) -> &AgentConfig {
        &self.config
    }

    fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    fn memory(&self) -> &Arc<dyn Memory> {
        &self.memory
    }

    fn session_manager(&self) -> Option<&Arc<crate::session::SessionManager>> {
        self.session_manager.as_ref()
    }

    fn persona_registry(&self) -> Option<&Arc<crate::prompt::PersonaRegistry>> {
        self.persona_registry.as_ref()
    }

    async fn run_message_with(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<AgentOutput, AgentError> {
        let _writing = self.turn_writer(session_id)?;
        self.run_turn(session_id, message, hosts, options).await
    }

    async fn run_stream(
        &self,
        session_id: &str,
        user_input: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<ChatChunkStream<AgentError>, AgentError> {
        self.stream_turn(session_id, user_input, hosts).await
    }

    /// Compacts a session's history into a summary now, whatever its size.
    ///
    /// Returns `Ok(true)` when the history was replaced by a summary, and `Ok(false)` when there was
    /// nothing to compact (too short, or not at a clean stopping point — see [`ends_cleanly`]). The
    /// tools of `hosts` are part of the summarization request so its prefix matches the
    /// conversation's own requests and the provider's cache is reused.
    async fn compact_session_with(
        &self,
        session_id: &str,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<bool, AgentError> {
        let _writing = self
            .session_manager
            .as_ref()
            .map(|sessions| sessions.agent_write(session_id))
            .transpose()?;
        let persona = self.resolve_persona(session_id, options.persona)?;
        let tools_enabled = self.config.tool_calling && !options.without_tools;
        let tools = self.collect_tools(hosts, options.without_tools)?;
        self.compact(
            session_id,
            tools_enabled.then_some(tools.definitions.as_slice()),
            persona.as_ref(),
            None,
            options.instructions.as_deref(),
        )
        .await
    }
}

/// Removes tool-produced filesystem paths from a tool result before it enters memory.
///
/// A tool that generated a file reports the attachment (which the pipeline delivers to the platform)
/// and, very often, the path in its textual result. The path is node-internal: leaving it in the
/// transcript leaks an absolute location to the model and to anyone reading the console, so it is
/// replaced by a marker — the file itself is still sent.
fn redact_tool_paths(mut text: String, paths: &[String]) -> String {
    for path in paths {
        let trimmed = path.trim();
        if !trimmed.is_empty() && text.contains(trimmed) {
            text = text.replace(trimmed, "[attachment]");
        }
    }
    text
}

/// Joins the reasoning collected across one turn's rounds, or `None` when there was none.
fn joined_reasoning(rounds: &[String]) -> Option<String> {
    (!rounds.is_empty()).then(|| rounds.join("\n\n"))
}

/// Puts the turn's media back on its user message for one request of the turn.
///
/// The turn's message is the last user message: everything after it is this turn's own tool loop,
/// which holds only assistant and tool messages. The next turn's requests no longer carry it, so
/// the prefix they share with this one ends just before this message — a one-time cache miss of
/// that message instead of re-sending every picture of the conversation with each later request.
fn attach_turn_media(request: &mut ChatRequest, media: Option<&Vec<ContentPart>>) {
    let Some(media) = media else {
        return;
    };
    if let Some(message) = request
        .messages
        .iter_mut()
        .rev()
        .find(|message| message.role == Role::User)
    {
        message.parts = Some(media.clone());
    }
}

/// Ids of the tool calls in the last tool-calling round of `history` that have no result yet.
fn unanswered_tool_calls(history: &[ChatMessage]) -> Vec<String> {
    let Some(round) = history.iter().rposition(|message| {
        message.role == Role::Assistant
            && message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
    }) else {
        return Vec::new();
    };
    let answered: std::collections::HashSet<&str> = history[round + 1..]
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    history[round]
        .tool_calls
        .iter()
        .flatten()
        .filter(|call| !answered.contains(call.id.as_str()))
        .map(|call| call.id.clone())
        .collect()
}

/// The note that closes an unanswered turn in history, written for the model that reads it on
/// the next turn: what happened, that nothing retried it, and that the user decides about retrying.
///
/// It names the kind of failure rather than quoting the error, whose provider body can be long
/// and would sit in every later request of the session.
fn closing_note(err: &AgentError) -> String {
    let what = match err {
        AgentError::Busy(_) => "failed because the session is busy".to_string(),
        AgentError::Stopped => "was stopped by the user before it finished".to_string(),
        AgentError::Gateway(GatewayError::Http(http)) if http.is_timeout() => {
            "failed: the model request timed out".to_string()
        }
        AgentError::Gateway(GatewayError::Http(_)) => {
            "failed: the model service could not be reached".to_string()
        }
        AgentError::Gateway(GatewayError::ApiStatus { status, .. }) => {
            format!("failed: the model service answered HTTP {status}")
        }
        AgentError::Gateway(GatewayError::Json(_) | GatewayError::InvalidResponse(_)) => {
            "failed: the model's answer could not be read".to_string()
        }
        AgentError::Rpc(_) => "failed: a plugin tool call failed".to_string(),
        AgentError::ToolFailed(_) => "failed: a tool execution failed".to_string(),
        AgentError::ToolNotFound(name) => format!("failed: the tool '{name}' does not exist"),
        AgentError::Memory(_) => "failed: conversation storage failed".to_string(),
        AgentError::InvalidRequest(_) => {
            "failed: model request configuration is invalid".to_string()
        }
        AgentError::Compaction(_) => "failed: conversation compaction failed".to_string(),
    };
    format!(
        "[This turn {what}. It was not retried. Answer the next message on its own, and redo this \
         request only if the user asks for it again.]"
    )
}

/// Recovers tool calls from a completion whose model emitted markup instead of a structured array.
///
/// Some endpoints ignore the `tools` request field and answer with `<tool_call>...` text. Turning
/// that text back into real calls is what makes the reasoning loop execute the tool and continue;
/// without it the markup is returned to the user as the assistant's answer.
fn normalize_textual_tool_calls(response: &mut ChatResponse) -> Result<(), GatewayError> {
    if !response.tool_calls.is_empty() {
        return Ok(());
    }
    let Some(content) = response.content.as_deref() else {
        return Ok(());
    };

    let (recovered, cleaned) = crate::tool_call_text::extract_textual_tool_calls(content);
    if recovered.is_empty() {
        return Ok(());
    }

    // A complete-looking block can precede a truncated or refused remainder. Text recovery
    // must not turn such a response into executable calls after the wire parser accepted its
    // partial text. Gate before changing the reply or committing any call to durable history.
    if let Some(reason) = response.finish_reason.as_deref()
        && !matches!(
            reason,
            "stop" | "end_turn" | "stop_sequence" | "completed" | "tool_calls" | "tool_use"
        )
    {
        return Err(GatewayError::InvalidResponse(format!(
            "textual tool calls have unusable finish reason: {reason}"
        )));
    }

    tracing::info!(
        tool_calls = recovered.len(),
        tools = ?recovered.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
        "Recovered tool calls a model emitted as text markup"
    );

    response.tool_calls = recovered;
    response.content = if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    };
    Ok(())
}
