//! Kanon Official Rust SDK.
//!
//! Provides the trait and runtime types for developing out-of-process Rust plugins, including
//! platform adapters: declare `[adapter] platform = "..."` in `plugin.toml`, implement
//! [`Plugin::on_deliver_message`] for outbound delivery, and push inbound messages back through
//! the [`CoreHandle`] exposed on [`PluginContext::core`].
//!
//! Most plugins do not implement [`Plugin`] by hand: [`Router`] builds one from handler
//! closures, with commands and command groups, triggers, typed tools, multi-turn conversations
//! ([`event::CommandEvent::wait_next`]), event subscriptions, reply decoration, system prompt
//! rewriting and HTTP routes. [`CoreHandle`] reaches back into the node: storage, the agent,
//! conversations, personas and rendering.
//!
//! [`Plugin`]: plugin::Plugin
//! [`Plugin::on_deliver_message`]: plugin::Plugin::on_deliver_message
//! [`PluginContext::core`]: context::PluginContext::core

pub mod agent;
pub mod context;
pub mod error;
pub mod event;
pub mod group;
pub mod host;
pub mod http;
pub mod json;
pub mod plugin;
pub mod router;
pub mod schema;
pub mod segment;
pub mod watchdog;

pub use context::CoreHandle;
pub use error::CoreError;
pub use host::KanonHost;
pub use kanon_proto as proto;
pub use router::Router;
pub use watchdog::{CoreWatchdogConfig, StopReason, watch_core};

// Re-exported so a plugin can derive tool argument types (`#[derive(Deserialize, JsonSchema)]`)
// against the exact versions the SDK generates schemas with; a plugin's own, different
// `schemars` major would derive a trait the SDK does not accept.
pub use schemars;
pub use serde;
pub use serde_json;

/// Everything a typical plugin needs: `use kanon_sdk::prelude::*;`.
pub mod prelude {
    pub use super::agent::{AgentReply, AgentRequest, IntoImage};
    pub use super::context::*;
    pub use super::error::CoreError;
    pub use super::event::{
        AgentBegin, AgentDone, CommandEvent, MessageEvent, ToolCall, ToolResult, WaitTimeout,
    };
    pub use super::group::CommandGroup;
    pub use super::host::KanonHost;
    pub use super::http;
    pub use super::plugin::*;
    pub use super::router::{
        CommandSpec, ContextSlot, Event, Reply, Router, SystemPrompt, ToolSpec, TriggerSpec,
    };
    pub use super::segment::{self, IntoReply};
    pub use super::watchdog::{CoreWatchdogConfig, StopReason, watch_core};
    pub use async_trait::async_trait;
    pub use kanon_proto::v1::*;
    pub use prost_types;
    pub use schemars::{self, JsonSchema};
    pub use serde::{self, Deserialize, Serialize};
    pub use serde_json::{self, json};
}
