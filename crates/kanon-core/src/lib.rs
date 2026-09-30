//! Kanon Core Microkernel Engine.
//!
//! Provides the central event pipeline, IPC gRPC server on `core.sock`,
//! static manifest parser, process supervisor for managing out-of-process
//! plugin hosts, and the platform adapter contract that connects the
//! microkernel to chat platforms.
//!
//! This crate is a **library only**: it owns no process entrypoint. The node
//! executable that assembles this engine with the management gateway lives in
//! `crates/kanon`.

pub mod access;
pub mod adapter;
pub mod bash;
pub mod conversation;
pub mod instance;
pub mod ipc;
pub mod manifest;
pub mod mcp;
pub mod notice;
pub mod pipeline;
pub mod shutdown;
pub mod skill;
pub mod supervisor;
mod time;
pub mod toggle;

pub use access::{
    CommandAccess, CommandPolicy, CommandPolicyStore, META_SENDER_NAME, META_SENDER_ROLE,
};
pub use adapter::{
    AdapterDescriptor, AdapterError, AdapterKind, AdapterRegistry, Capability, EventIngress,
    IngestError, PlatformAdapter,
};
pub use bash::{
    BashAvailabilityHook, BashCaller, BashExecutionMode, BashLocalConfig, BashPolicy,
    BashPolicyStore, BashReviewDecision, BashReviewRequest, BashReviewer, BashSandboxConfig,
    BashTool, DEFAULT_BASH_WORKSPACE, ModelBashReviewer, with_bash_caller,
};
pub use conversation::{
    ContextPolicy, ContextPolicyStore, ConversationKind, META_BOT_MENTIONED,
    META_CONVERSATION_KIND, META_TIMESTAMP, META_TIMESTAMP_TEXT, ReplyMode, ReplyPolicy,
    ReplyPolicyStore, bot_mentioned,
};
pub use instance::{
    BashScope, BotInstance, DEFAULT_INSTANCE_CATALOG, InstanceDraft, InstanceError,
    InstanceRegistry, SessionScope, instance_persona_id, sync_instance_personas,
};
pub use ipc::{CoreApiService, CoreIpcServer};
pub use manifest::{
    AdapterSection, DiscoveredPlugin, PluginManifest, PluginScanner, PluginSection,
    ToolDefinitionEntry,
};
pub use mcp::{
    ATTACHMENT_RETENTION, DEFAULT_ATTACHMENT_DIR, DEFAULT_MCP_CONFIG, MCP_WATCHDOG_INTERVAL,
    McpConfigStore, McpError, McpHealth, McpPool, McpServer, McpServerConfig, McpTransport,
    prune_attachments,
};
pub use notice::{
    EventPolicy, EventPolicyStore, META_NOTICE, META_NOTICE_ACTOR, META_NOTICE_TARGET,
    META_REQUEST_TOKEN, NoticeKind, RecallLedger,
};
pub use pipeline::{
    CommandRouter, DEFAULT_OUTBOUND_QUEUE_CAPACITY, DeliveryOutcome, HELP_COMMAND, INFO_COMMAND,
    MODEL_COMMAND, MatchedCommand, NEW_SESSION_COMMAND, PipelineEngine, PipelineObserver,
    PipelineResult, PipelineStage, PreFilterChain, PreFilterOutcome, build_user_message,
};
pub use shutdown::shutdown_signal;
pub use skill::{
    DEFAULT_SKILLS_DIR, MAX_SKILL_BYTES, ReadSkillTool, SkillCatalogHook, SkillError, SkillMeta,
    SkillStore, allowed_skills, catalog_prompt,
};
pub use supervisor::{
    AdapterRoute, HOST_WATCHDOG_INTERVAL, HOST_WATCHDOG_MAX_RESTARTS, HostHealth, HostRegistration,
    LaunchSpec, ManagedHost, Supervisor, SupervisorError, UnavailablePlugin,
    circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState},
};
pub use toggle::{DEFAULT_TOGGLE_STATE, MCP_SECTION, PLUGIN_SECTION, SKILL_SECTION, ToggleStore};
