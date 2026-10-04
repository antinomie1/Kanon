//! The agent: what answers a conversation turn.
//!
//! [`crate::ConversationBackend`] selects the complete runtime before model routing. The
//! local [`Agent`] contract serves [`crate::BuiltinAgent`] and embedded model loops; it exposes
//! their provider and memory. The optional DSH adapter delegates to `kanon-dsh` instead of
//! fabricating a builtin provider or memory implementation for a remote-owned conversation.
//!
//! The module also holds the types every agent shares: its configuration ([`AgentConfig`]), its
//! result ([`AgentOutput`]), native in-process tools ([`AgentTool`]) and lifecycle hooks
//! ([`AgentHook`]).

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kanon_proto::v1::ToolCallRequest;

use crate::compaction::CompactionPolicy;
use crate::error::AgentError;
use crate::gateway::types::{ChatMessage, ChatRequest, ChatResponse, ToolCall, ToolDefinition};
use crate::gateway::{ChatChunkStream, LlmProvider};
use crate::memory::Memory;
use crate::session::SessionManager;
use crate::tool_router::{ExecutedToolCall, ToolAttachment, ToolHost};

/// Identifier of [`crate::BuiltinAgent`], Kanon's own model-and-tool loop.
pub const BUILTIN_AGENT: &str = "builtin";

/// Identifiers of the agents an operator may select, node-wide or for one bot instance.
///
/// The external DSH runtime is selectable only in builds that explicitly include its feature.
/// Backend selection is independent of the built-in model provider directory.
pub fn selectable_agents() -> &'static [&'static str] {
    #[cfg(feature = "dsh")]
    {
        &[BUILTIN_AGENT, "dsh"]
    }
    #[cfg(not(feature = "dsh"))]
    {
        &[BUILTIN_AGENT]
    }
}

/// Checks that `id` names a selectable agent.
///
/// An unknown identifier is refused rather than mapped to the built-in agent: a setting that
/// silently answers with a different engine than the one it names would mislead the operator.
pub fn check_agent_id(id: &str) -> Result<(), String> {
    if selectable_agents().contains(&id) {
        Ok(())
    } else {
        Err(format!(
            "unknown agent '{id}'; available: {}",
            selectable_agents().join(", ")
        ))
    }
}

/// Answers conversation turns for the node.
///
/// A turn is one user message in one session. The agent owns what happens between receiving it
/// and returning the reply: reading the session's history from [`Agent::memory`], calling the
/// model, running tools from the given hosts, and appending the turn to history. Callers only
/// choose the session, build the message and pick the tool hosts the turn may use.
///
/// Implementations must keep the session contract the rest of the node relies on: history is
/// append-only, a turn that fails leaves a history the next turn can continue, and a turn ends
/// promptly when the task-local stop signal ([`crate::with_stop_signal`]) fires.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Name used in logs and traces.
    fn name(&self) -> &str;

    /// Model and limits the agent answers with; what the console reports and routing compares.
    fn config(&self) -> &AgentConfig;

    /// Model endpoint the agent was configured with.
    ///
    /// Also serves one-shot completions outside any conversation (plugin `RequestLLM`, the Bash
    /// reviewer), which must use the same endpoint and credentials as the agent itself.
    fn provider(&self) -> &Arc<dyn LlmProvider>;

    /// Conversation history the agent reads and appends to.
    fn memory(&self) -> &Arc<dyn Memory>;

    /// Session metadata (persona, turn counts) the agent records turns in, when it keeps any.
    fn session_manager(&self) -> Option<&Arc<SessionManager>> {
        None
    }

    /// Persona catalog used by this agent, including embedded agents built without a factory.
    fn persona_registry(&self) -> Option<&Arc<crate::prompt::PersonaRegistry>> {
        None
    }

    /// Answers one turn whose message the caller built, with per-turn settings.
    ///
    /// The settings change only this turn; an agent that cannot honour one must fail the turn
    /// rather than quietly answer without it.
    async fn run_message_with(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<AgentOutput, AgentError>;

    /// Answers one turn whose message the caller built (text, or text with images).
    async fn run_message(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<AgentOutput, AgentError> {
        self.run_message_with(session_id, message, hosts, TurnOptions::default())
            .await
    }

    /// Answers one text turn, streaming model text while the same tool loop records the turn.
    ///
    /// Tools must use structured calls. Textual tool markup fails explicitly instead of executing
    /// text already delivered to the client. Response hooks cannot replace emitted text.
    async fn run_stream(
        &self,
        session_id: &str,
        user_input: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<ChatChunkStream<AgentError>, AgentError>;

    /// Folds a session's history into a summary now.
    ///
    /// Returns whether the history was replaced. An agent that keeps no history of its own has
    /// nothing to fold, which is the default.
    async fn compact_session(
        &self,
        session_id: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<bool, AgentError> {
        self.compact_session_with(session_id, hosts, TurnOptions::default())
            .await
    }

    /// Compacts with the same inherited persona and tool selection as the conversation's turn.
    ///
    /// Callers that supply an instance persona must resolve its current configuration first,
    /// exactly as they do for a turn. Unsupported overrides fail instead of using another prefix.
    async fn compact_session_with(
        &self,
        _session_id: &str,
        _hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<bool, AgentError> {
        if options != TurnOptions::default() {
            return Err(AgentError::InvalidRequest(
                "this agent does not support compaction settings".to_string(),
            ));
        }
        Ok(false)
    }

    /// Answers one text turn.
    async fn run(
        &self,
        session_id: &str,
        user_input: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<AgentOutput, AgentError> {
        self.run_message(session_id, ChatMessage::user(user_input), hosts)
            .await
    }

    /// Answers one text turn without plugin tools.
    async fn run_standalone(
        &self,
        session_id: &str,
        user_input: &str,
    ) -> Result<AgentOutput, AgentError> {
        self.run(session_id, user_input, &[]).await
    }

    /// Answers one text turn without plugin tools, streaming the final reply.
    async fn run_standalone_stream(
        &self,
        session_id: &str,
        user_input: &str,
    ) -> Result<ChatChunkStream<AgentError>, AgentError> {
        self.run_stream(session_id, user_input, &[]).await
    }
}

/// Settings that apply to one turn only, on top of the agent's [`AgentConfig`].
///
/// The default uses the session's explicit persona choice and the agent's tool settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnOptions {
    /// Resolved instance persona, overriding the session's explicit choice for this turn only.
    ///
    /// This immutable snapshot is reused for tool rounds and background compaction. Inherited
    /// instance configuration must never be copied into the session's durable persona binding.
    pub persona: Option<crate::prompt::Persona>,
    /// Stable mode instructions appended independently of the selected persona.
    /// Callers must keep runtime facts in the user message to preserve the cached prefix.
    pub instructions: Option<String>,
    /// Tool rounds allowed in this turn; `None` uses [`AgentConfig::max_iterations`].
    pub max_iterations: Option<usize>,
    /// Disables tool advertisement and execution in this turn, including native tools.
    ///
    /// The tool list heads every request, so a turn without it does not share the conversation's
    /// cached prefix; callers use it for a deliberate one-off (a plugin's tool-less agent run).
    pub without_tools: bool,
}

/// Configuration parameters for agent reasoning and execution.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Maximum tool reasoning loop iterations before forcing termination.
    pub max_iterations: usize,
    /// Whether the selected model supports tool calling. Disabled models never receive tool
    /// definitions or execute returned calls, even when a request or response hook adds them.
    pub tool_calling: bool,
    /// Default model identifier.
    pub default_model: String,
    /// Name of the provider endpoint that serves `default_model`, when the node routes by name.
    ///
    /// Kept separately from `default_model` because the wire request must carry the bare upstream
    /// id while the console and the built-in `/model` command address the model as
    /// `<provider>/<model-id>`.
    pub provider: Option<String>,
    /// Maximum context window of the model in tokens, when known.
    ///
    /// Descriptive metadata: it lets the console explain why history is compacted and lets the
    /// pipeline reason about how much media fits, rather than being enforced here.
    pub context_length: Option<u32>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Optional max tokens limit for completions.
    pub max_tokens: Option<u32>,
    /// Whether to halt immediately if a tool call returns an error.
    pub stop_on_tool_failure: bool,
    /// When a long conversation is compacted into a summary; `None` never compacts.
    ///
    /// On by default: history is append-only (see [`crate::memory`]), so without compaction a long
    /// conversation would eventually outgrow the model's context window.
    pub compaction: Option<CompactionPolicy>,
}

impl AgentConfig {
    /// Canonical `<provider>/<model-id>` reference addressed by this configuration.
    pub fn model_ref(&self) -> String {
        match self.provider.as_deref().filter(|name| !name.is_empty()) {
            Some(provider) => format!("{provider}/{}", self.default_model),
            None => self.default_model.clone(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 5,
            tool_calling: true,
            default_model: "gpt-4o-mini".to_string(),
            provider: None,
            context_length: None,
            temperature: None,
            max_tokens: None,
            stop_on_tool_failure: false,
            compaction: Some(CompactionPolicy::default()),
        }
    }
}

/// Result produced after an agent run concludes.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutput {
    /// Final natural language response generated by the agent.
    pub content: String,
    /// Model reasoning from every round of this turn, joined in order, without any tag markup.
    ///
    /// Kept apart from `content` so a delivery boundary decides whether users ever see it.
    pub reasoning: Option<String>,
    /// Sequence of tool calls executed during the reasoning loop.
    pub executed_tools: Vec<ExecutedToolCall>,
    /// Total conversational / reasoning turns executed.
    pub turns: usize,
    /// Model-reported finish reason (e.g. `stop`, `tool_calls`, `length`).
    pub finish_reason: Option<String>,
    /// Rich media produced by the executed tool calls, deduplicated in execution order.
    ///
    /// The pipeline turns these into image/file segments on the outbound message, which is how a
    /// drawing tool's output reaches the chat platform instead of only its text description.
    pub attachments: Vec<ToolAttachment>,
}

/// What a native tool hands back from a successful call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutput {
    /// Result text recorded as the tool response the model reads.
    pub text: String,
    /// Media for the user, delivered with the turn's reply exactly like plugin and MCP
    /// attachments. The model never sees these; describe them in `text` without file paths.
    pub attachments: Vec<ToolAttachment>,
}

impl From<String> for ToolOutput {
    /// A text-only result, which is what most tools return.
    fn from(text: String) -> Self {
        Self {
            text,
            attachments: Vec::new(),
        }
    }
}

/// Pluggable tool abstraction for in-process or native agent tools.
///
/// Allows developers, internal modules, or dynamic scripts to expose functions
/// directly to the agent without requiring external gRPC IPC processes.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Name, description, and JSON schema describing parameter expectations.
    fn definition(&self) -> ToolDefinition;

    /// Invokes the tool implementation in-process.
    ///
    /// The error string is shown to the model as the failed call's result.
    async fn call(
        &self,
        session_id: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, String>;
}

/// Helper type for asynchronous native tool closures.
pub type NativeToolFn = Arc<
    dyn Fn(&str, serde_json::Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// In-process tool constructed directly from a definition and an async closure.
pub struct NativeTool {
    definition: ToolDefinition,
    handler: NativeToolFn,
}

impl NativeTool {
    /// Creates a new native in-process tool from a definition and an async handler.
    pub fn new<F, Fut>(definition: ToolDefinition, f: F) -> Self
    where
        F: Fn(&str, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        Self {
            definition,
            handler: Arc::new(move |sid, args| Box::pin(f(sid, args))),
        }
    }
}

#[async_trait]
impl AgentTool for NativeTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn call(
        &self,
        session_id: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, String> {
        (self.handler)(session_id, arguments)
            .await
            .map(ToolOutput::from)
    }
}

/// Lifecycle interception hooks for agent reasoning and execution.
///
/// Enables plugins and middleware to implement:
/// - RAG / semantic context retrieval injection before calling the LLM;
/// - Guardrails, safety verification, and permission confirmation before tool execution;
/// - Observability, tracing, token budget tracking, and latency auditing.
#[async_trait]
pub trait AgentHook: Send + Sync {
    /// Prepares the static system text once per turn, before any model request.
    ///
    /// Persona instructions are already present; skills and plugin rules append or rewrite them.
    /// The turn owns the result through every tool round. Runtime data belongs in the user
    /// message, while per-request observation or mutation stays in [`Self::on_llm_request`].
    /// `tools` is the turn's enabled tool snapshot, so instructions never require unavailable tools.
    async fn on_system_prompt(
        &self,
        _session_id: &str,
        _prompt: &mut String,
        _tools: &[ToolDefinition],
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Enriches the originating user message once, before it is appended to durable history.
    ///
    /// Runtime metadata belongs here so tool-loop requests and compaction reuse unchanged history.
    async fn on_user_message(
        &self,
        _session_id: &str,
        _message: &mut ChatMessage,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Invoked immediately before transmitting the request payload to the model provider.
    async fn on_llm_request(
        &self,
        _session_id: &str,
        _request: &mut ChatRequest,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Invoked immediately upon receiving a complete response from the model.
    ///
    /// Streaming has already delivered its text by this point. Hooks may observe that text and
    /// inspect tool calls, but changing streamed content or reasoning fails the turn explicitly;
    /// use request hooks to shape streamed answers before generation.
    async fn on_llm_response(
        &self,
        _session_id: &str,
        _response: &mut ChatResponse,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Invoked before dispatching a tool execution.
    ///
    /// Return `Ok(true)` to permit execution, or `Ok(false)` to veto / reject execution.
    async fn on_before_tool_call(
        &self,
        _session_id: &str,
        _call: &ToolCall,
    ) -> Result<bool, AgentError> {
        Ok(true)
    }

    /// Invoked immediately following completion of a tool execution.
    async fn on_after_tool_call(
        &self,
        _session_id: &str,
        _call: &ToolCall,
        _result: &str,
        _success: bool,
    ) -> Result<(), AgentError> {
        Ok(())
    }
}

/// Fallback host used for standalone agent executions without external gRPC plugin processes.
#[derive(Debug, Clone, Default)]
pub struct NoopHost;

#[async_trait]
#[async_trait::async_trait]
impl ToolHost for NoopHost {
    fn host_id(&self) -> &str {
        "noop"
    }

    fn plugin_metas(&self) -> Vec<kanon_proto::v1::PluginMeta> {
        Vec::new()
    }

    async fn call_tool(
        &self,
        _req: ToolCallRequest,
    ) -> Result<kanon_proto::v1::ToolCallResponse, tonic::Status> {
        Err(tonic::Status::not_found(
            "No external plugin host available",
        ))
    }
}

/// Kanon's builtin agent runtime.
pub mod builtin;
/// Optional deepseek-harness agent runtime.
#[cfg(feature = "dsh")]
pub mod dsh;

/// Shared tool dispatch without builtin memory or model ownership.
pub mod tool_execution;
