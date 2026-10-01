//! Plugin trait definition for Kanon out-of-process plugins.
//!
//! Provides the primary interface for defining plugin lifecycle hooks,
//! command dispatchers, pre-filters, and LLM tools.

use crate::context::PluginContext;
use async_trait::async_trait;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DecorateReplyRequest, DeliverMessageRequest,
    DeliverMessageResponse, EventNotification, MessageSegment, PipelineEventRequest, PluginMeta,
    PreFilterResult, ToolCallRequest, ToolCallResponse,
};

/// Result alias for plugin operations.
pub type PluginResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Core trait representing an out-of-process Kanon plugin.
///
/// Implementors specify their static metadata via [`meta`](Plugin::meta) and
/// override lifecycle and pipeline handlers as needed.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Returns the static metadata descriptor of this plugin.
    fn meta(&self) -> PluginMeta;

    /// Invoked once when the plugin host is initialized and ready.
    async fn on_load(&mut self, _ctx: &mut PluginContext) -> PluginResult<()> {
        Ok(())
    }

    /// Invoked when the operator saves a new configuration for this plugin.
    ///
    /// The host acknowledges the reload only after this returns `Ok`; an error is reported back
    /// to the console and the core does not persist the rejected configuration.
    async fn on_config_reload(&mut self, _config: prost_types::Struct) -> PluginResult<()> {
        Ok(())
    }

    /// Invoked during graceful shutdown before the host process terminates.
    async fn on_unload(&mut self) -> PluginResult<()> {
        Ok(())
    }

    /// Intercepts inbound events before command dispatch or LLM reasoning.
    async fn on_pre_filter(
        &self,
        _req: PipelineEventRequest,
    ) -> PluginResult<Option<PreFilterResult>> {
        Ok(None)
    }

    /// Executes a registered slash command.
    async fn on_execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> PluginResult<CommandExecuteResponse> {
        Ok(CommandExecuteResponse {
            success: true,
            replies: vec![],
            error_message: format!("Command '{}' executed by default stub handler", req.command),
            capture_seconds: 0,
        })
    }

    /// Executes a registered LLM tool call.
    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {
        Ok(ToolCallResponse {
            call_id: req.call_id,
            success: true,
            error_message: String::new(),
            payload: None,
            // Tools that draw or fetch files declare them here; the core forwards them to the
            // outbound message.
            attachments: Vec::new(),
        })
    }

    /// Publishes an outbound message for the platform this plugin serves as an adapter.
    ///
    /// The core routes every reply whose `platform` matches the plugin's `[adapter]` manifest
    /// declaration to this hook. The default implementation reports failure instead of pretending
    /// to deliver: a plugin that declares itself an adapter but never overrides this hook would
    /// otherwise silently swallow user-visible replies.
    async fn on_deliver_message(
        &self,
        req: DeliverMessageRequest,
    ) -> PluginResult<DeliverMessageResponse> {
        Ok(DeliverMessageResponse {
            success: false,
            message_id: String::new(),
            error_message: format!(
                "plugin does not implement on_deliver_message for platform '{}'",
                req.platform
            ),
        })
    }

    /// Receives a lifecycle event this plugin subscribed to through `PluginMeta.events`.
    ///
    /// Events are fire-and-forget: the core does not wait for the outcome, and an error is only
    /// logged by the host.
    async fn on_event(&self, _event: EventNotification) -> PluginResult<()> {
        Ok(())
    }

    /// Rewrites a reply before delivery; called only when `PluginMeta.decorates_replies` is set.
    ///
    /// Return `Ok(None)` to leave the reply untouched, or `Ok(Some(segments))` to replace it (an
    /// empty list suppresses it). An error leaves the reply untouched.
    async fn on_decorate_reply(
        &self,
        _req: DecorateReplyRequest,
    ) -> PluginResult<Option<Vec<MessageSegment>>> {
        Ok(None)
    }
}
