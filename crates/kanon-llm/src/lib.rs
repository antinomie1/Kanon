//! # Kanon LLM Module
//!
//! Provides a unified, high-performance LLM gateway, concurrent pluggable
//! session memory, and a general-purpose agent engine with cross-language Tool Calling.
//!
//! ## Submodules
//! - [`agent`]: The [`Agent`] trait that answers conversation turns, and the types agents share.
//! - [`builtin`]: [`BuiltinAgent`], Kanon's own tool-loop agent and the node's only implementation.
//! - [`error`]: Granular error types for gateway, agent, and tool routing.
//! - [`gateway`]: Protocol-level LLM client implementations (OpenAI Chat, OpenAI Responses, Anthropic Messages).
//! - [`prompt`]: Static personas resolved once per turn before request composition.
//! - [`layout`]: The static-first request layout and the normalization that keeps prompt prefixes stable.
//! - [`memory`]: Append-only conversation memory: the [`Memory`] trait and the lock-free [`InMemory`] backend.
//! - [`compaction`]: Cache-safe context compaction, the only way history gets shorter.
//! - [`session`]: Session records (persona binding, counters, status) with optional write-through durability.
//! - [`sqlite_memory`]: SQLite backends for history ([`SqliteMemory`]) and session records ([`SqliteSessionStore`]).
//! - [`model`]: Model identity (`provider/model-id`), capabilities and the per-model settings catalog.
//! - [`provider`]: Named provider endpoints and the `provider/model` routing they enable.
//! - [`slot`]: Shared hot-swappable handle to the node's active agent runtime.
//! - [`tool_call_text`]: Recovery of tool calls a model emitted as text markup instead of structured calls.
//! - [`tool_router`]: Specialized pipeline router adapter, dynamic tool aggregation, and in-memory Protobuf/JSON translation.

pub mod agent;
pub use agent::builtin;
pub mod compaction;
pub mod error;
pub mod factory;
pub mod gateway;
pub mod layout;
pub mod memory;
pub mod model;
pub mod persona_store;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod slot;
pub mod sqlite_memory;
pub mod stop;
pub mod token;
pub mod tool_call_text;
pub mod tool_router;
pub mod visible_reply;

pub use agent::{
    Agent, AgentConfig, AgentHook, AgentOutput, AgentTool, BUILTIN_AGENT, NativeTool, NativeToolFn,
    NoopHost, ToolOutput, TurnOptions, check_agent_id, selectable_agents,
};
pub use builtin::{AgentBuilder, BuiltinAgent, FAILED_TOOL_RESULT, STOPPED_TOOL_RESULT};
pub use compaction::{COMPACTION_INSTRUCTION, CompactionPolicy};
pub use error::{AgentError, GatewayError, MemoryError, ToolRouterError};
pub use factory::{AgentFactory, ConversationBackend, ProviderRuntime};
pub use gateway::providers::{
    AnthropicMessagesProvider, AnthropicProvider, OpenAiChatProvider, OpenAiProvider,
    OpenAiResponsesProvider, SseDecoder, SseEvent,
};
pub use gateway::{
    ChatChunk, ChatChunkStream, ChatMessage, ChatRequest, ChatResponse, ContentPart, LlmGateway,
    LlmProvider, ProviderSetup, Role, SUPPORTED_PROTOCOLS, TokenUsage, ToolCall, ToolDefinition,
    build_provider, strip_reasoning_tags,
};
pub use layout::{canonical_json, canonical_tools, normalize_request};
pub use memory::{InMemory, Memory, MemorySnapshot, SessionMemory, StoredSession};
pub use model::{ModelCapabilities, ModelCatalog, ModelRef, ModelSettingsSource, ModelSpec};
pub use persona_store::{DEFAULT_PERSONA_FILE, PersonaChangeError, PersonaStore};
pub use prompt::{
    BASE_PERSONA_ID, BASE_PERSONA_PROMPT, Persona, PersonaError, PersonaKind, PersonaRegistry,
    is_valid_slug,
};
pub use provider::{ProviderEntry, ProviderRegistry, ResolvedProvider};
pub use session::{
    RuntimeSessionMetadata, SessionKey, SessionManager, SessionMetadata, SessionOverview,
    SessionScope, SessionStatus, SessionStore,
};
pub use slot::AgentSlot;
pub use sqlite_memory::{PersistentMemory, SqliteMemory, SqliteSessionStore};
pub use stop::{StopSignal, with_stop_signal};
pub use token::{
    estimate_conversation_tokens, estimate_message_tokens, estimate_request_tokens,
    estimate_text_tokens, estimate_tokens,
};
pub use tool_call_text::extract_textual_tool_calls;
pub use tool_router::{
    ExecutedToolCall, ToolAttachment, ToolHost, ToolRouter, ToolRouterOutput, aggregate_tools,
    resolve_tools,
};
pub use visible_reply::visible_reply;

/// Optional deepseek-harness transport and remote session ownership.
#[cfg(feature = "dsh")]
pub use agent::dsh;
