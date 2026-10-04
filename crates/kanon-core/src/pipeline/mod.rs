//! Message pipeline and command/tool routing engine.
//!
//! Provides the central message processing pipeline for Kanon Core, including:
//! - [`PreFilterChain`]: Ordered PreFilter interception chain with priority scheduling and strict 30ms total deadline.
//! - [`CommandRouter`]: Slash command parser and router dispatching to plugin hosts.
//! - [`PipelineEngine`]: Asynchronous worker loop consuming ingested events and managing outbound dispatch.
//! - [`PipelineObserver`]: Fire-and-forget lifecycle observation hook for control-plane tracing.
//! - [`build_user_message`]: Translation of inbound segments into the model-visible user message.
//! - [`inline_images`]: Inbound images downloaded by the node, so the provider never fetches them.
//! - [`attachment_segments`]: Outbound segments for tool media, limited to what the adapter can send.
//! - [`PluginAgentHook`]: Plugins inside the agent's turn (system prompt rewrites, tool events).
//! - [`AgentRun`]: The node's agent run on a plugin's behalf (`RunAgent`).

pub mod agent_hook;
mod agent_run;
pub mod attachment;
pub mod capture;
pub mod command;
pub mod context;
pub mod conversations;
pub mod dead_letter;
pub mod engine;
pub mod group_log;
pub mod hooks;
pub mod identity;
pub mod media;
pub mod observer;
pub mod pre_filter;
mod reply;
mod turns;

pub use agent_hook::{PluginAgentHook, with_turn};
pub use agent_run::{AgentRun, AgentRunError, AgentRunOutput};
pub use attachment::{AttachmentSegments, MediaKind, attachment_segments};
pub use capture::{Capture, CaptureRegistry, MAX_CAPTURE};
pub use command::{
    CommandRouter, MatchedCommand, MatchedTrigger, ParsedCommand, TriggerMatcher, split_args,
};
pub use context::build_user_message;
pub use conversations::{ConversationError, ConversationInfo};
pub use dead_letter::{
    DEFAULT_DEAD_LETTER_DIR, DeadLetterDirection, DeadLetterRecord, DeadLetterWriter,
};
pub use engine::{
    DEFAULT_OUTBOUND_QUEUE_CAPACITY, DELETE_SESSION_COMMAND, DeliveryOutcome, HELP_COMMAND,
    INFO_COMMAND, LIST_SESSIONS_COMMAND, MODEL_COMMAND, NEW_SESSION_COMMAND, PipelineEngine,
    PipelineResult, STOP_COMMAND, SWITCH_SESSION_COMMAND,
};
pub use media::{MAX_INBOUND_IMAGE_BYTES, inline_images};
pub use observer::{PipelineObserver, PipelineStage};
pub use pre_filter::{
    PREFILTER_TOTAL_DEADLINE, PREFILTER_WARN_THRESHOLD, PreFilterChain, PreFilterOutcome,
};
