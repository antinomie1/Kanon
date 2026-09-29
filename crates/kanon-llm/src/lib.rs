//! # Kanon LLM Module
//!
//! Provides a unified, high-performance LLM gateway, concurrent pluggable
//! session memory, and a general-purpose agent engine with cross-language Tool Calling.
//!
//! ## Submodules
//! - [`agent`]: General-purpose agent engine for conversational bots and autonomous task workers.
//! - [`error`]: Granular error types for gateway, agent, and tool routing.
//! - [`gateway`]: Protocol-level LLM client implementations (OpenAI Chat, OpenAI Responses, Anthropic Messages).
//! - [`prompt`]: Static personas and the hook that places them at the top of every request.
//! - [`memory`]: Pluggable conversation memory subsystem with [`Memory`] trait and lock-free [`SlidingWindowMemory`].
//! - [`model`]: Model identity (`provider/model-id`), capabilities and the per-model settings catalog.
//! - [`provider`]: Named provider endpoints and the `provider/model` routing they enable.
//! - [`slot`]: Shared hot-swappable handle to the node's active agent runtime.
//! - [`tool_call_text`]: Recovery of tool calls a model emitted as text markup instead of structured calls.
//! - [`tool_router`]: Specialized pipeline router adapter, dynamic tool aggregation, and in-memory Protobuf/JSON translation.

pub mod agent;
pub mod error;
pub mod factory;
pub mod gateway;
pub mod memory;
pub mod model;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod slot;
pub mod sqlite_memory;
pub mod summary;
pub mod token;
pub mod tool_call_text;
pub mod tool_router;

pub use agent::{
    Agent, AgentBuilder, AgentConfig, AgentHook, AgentOutput, AgentTool, NativeTool, NativeToolFn,
    NoopHost,
};
pub use error::{AgentError, GatewayError, MemoryError, ToolRouterError};
pub use factory::{AgentFactory, ProviderRuntime};
pub use gateway::providers::{
    AnthropicMessagesProvider, AnthropicProvider, OpenAiChatProvider, OpenAiProvider,
    OpenAiResponsesProvider, SseDecoder, SseEvent,
};
pub use gateway::{
    ChatChunk, ChatChunkStream, ChatMessage, ChatRequest, ChatResponse, ContentPart, LlmGateway,
    LlmProvider, ProviderSetup, Role, SUPPORTED_PROTOCOLS, TokenUsage, ToolCall, ToolDefinition,
    build_provider, provider_from_env, strip_reasoning_tags,
};
pub use memory::{ConversationManager, Memory, SessionMemory, SlidingWindowMemory};
pub use model::{ModelCapabilities, ModelCatalog, ModelRef, ModelSettingsSource, ModelSpec};
pub use prompt::{
    BASE_PERSONA_ID, BASE_PERSONA_PROMPT, Persona, PersonaError, PersonaHook, PersonaKind,
    PersonaRegistry, is_valid_slug,
};
pub use provider::{ProviderEntry, ProviderRegistry, ResolvedProvider};
pub use session::{
    RuntimeSessionMetadata, SessionKey, SessionManager, SessionMetadata, SessionScope,
    SessionStatus,
};
pub use slot::AgentSlot;
pub use sqlite_memory::{PersistentMemory, SqliteMemory};
pub use summary::{ContextSummarizer, SummaryConfig, SummaryHook};
pub use token::{
    estimate_conversation_tokens, estimate_message_tokens, estimate_text_tokens, estimate_tokens,
};
pub use tool_call_text::extract_textual_tool_calls;
pub use tool_router::{
    ToolAttachment, ToolHost, ToolRouter, ToolRouterOutput, aggregate_tools, resolve_tools,
};
