//! Kanon Official Rust SDK.
//!
//! Provides the trait and runtime types for developing out-of-process Rust plugins, including
//! platform adapters: declare `[adapter] platform = "..."` in `plugin.toml`, implement
//! [`Plugin::on_deliver_message`] for outbound delivery, and push inbound messages back through
//! the [`CoreHandle`] exposed on [`PluginContext::core`].
//!
//! Most plugins do not implement [`Plugin`] by hand: [`Router`] builds one from handler
//! closures, with multi-turn conversations ([`event::CommandEvent::wait_next`]), triggers,
//! event subscriptions and reply decoration.

pub mod context;
pub mod event;
pub mod host;
pub mod json;
pub mod plugin;
pub mod router;
pub mod segment;
pub mod watchdog;

pub use context::CoreHandle;
pub use host::KanonHost;
pub use kanon_proto as proto;
pub use router::Router;
pub use watchdog::{CoreWatchdogConfig, StopReason, watch_core};

pub mod prelude {
    pub use super::context::*;
    pub use super::event::{CommandEvent, MessageEvent, WaitTimeout};
    pub use super::host::KanonHost;
    pub use super::plugin::*;
    pub use super::router::{
        CommandSpec, ContextSlot, Event, Reply, Router, ToolSpec, TriggerSpec,
    };
    pub use super::segment::{self, IntoReply};
    pub use super::watchdog::{CoreWatchdogConfig, StopReason, watch_core};
    pub use async_trait::async_trait;
    pub use kanon_proto::v1::*;
    pub use prost_types;
}
