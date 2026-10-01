//! Plugin lifecycle events and reply decoration.
//!
//! Both are opt-in through plugin metadata (`PluginMeta.events`, `PluginMeta.decorates_replies`),
//! so a host is only called for what one of its plugins asked for.
//!
//! Events are fire-and-forget: each notification runs on its own task with a timeout, because the
//! pipeline worker handles one event at a time and a slow subscriber must never delay the next
//! message. Decoration, in contrast, sits on the reply path and is awaited, so each decorator gets
//! a short deadline and any failure keeps the reply as it was — a broken decorator may cost its own
//! effect, never the reply.

use std::sync::Arc;
use std::time::Duration;

use kanon_proto::v1::{
    DecorateReplyRequest, EventKind, EventNotification, MessageSegment, PipelineEventRequest,
    ReplySource, event_notification::Detail,
};

use crate::supervisor::ManagedHost;

/// Deadline for one event notification.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadline for one decorator; it delays the reply, so it is kept short.
pub const DECORATE_TIMEOUT: Duration = Duration::from_secs(3);

/// Sends `detail` to every plugin on `hosts` that subscribed to `kind`, without waiting.
///
/// Must be called from within a Tokio runtime.
pub fn emit_event(hosts: &[Arc<ManagedHost>], kind: EventKind, detail: Detail) {
    for host in hosts {
        for plugin in host.metas() {
            if !plugin.events().any(|subscribed| subscribed == kind) {
                continue;
            }
            let host = host.clone();
            let notification = EventNotification {
                plugin_id: plugin.id.clone(),
                detail: Some(detail.clone()),
            };
            tokio::spawn(async move {
                match tokio::time::timeout(EVENT_TIMEOUT, host.notify_event(notification)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(status)) => tracing::debug!(
                        host_id = %host.host_id,
                        plugin_id = %plugin.id,
                        error = %status,
                        "Plugin event notification failed"
                    ),
                    Err(_) => tracing::debug!(
                        host_id = %host.host_id,
                        plugin_id = %plugin.id,
                        "Plugin event notification timed out"
                    ),
                }
            });
        }
    }
}

/// Runs the reply through every decorating plugin and returns the final segments.
///
/// Decorators run in host priority order (then `host_id`, for determinism), each seeing the
/// previous decorator's output. A decorator may return an empty list to suppress the reply; the
/// remaining decorators are then skipped, since there is nothing left to decorate.
pub async fn decorate_reply(
    hosts: &[Arc<ManagedHost>],
    context: &PipelineEventRequest,
    source: ReplySource,
    command: &str,
    mut segments: Vec<MessageSegment>,
) -> Vec<MessageSegment> {
    let mut ordered: Vec<&Arc<ManagedHost>> = hosts.iter().collect();
    ordered.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.host_id.cmp(&b.host_id))
    });

    for host in ordered {
        for plugin in host.metas() {
            if !plugin.decorates_replies || segments.is_empty() {
                continue;
            }
            let request = DecorateReplyRequest {
                plugin_id: plugin.id.clone(),
                context: Some(context.clone()),
                segments: segments.clone(),
                source: source as i32,
                command: command.to_string(),
            };
            match tokio::time::timeout(DECORATE_TIMEOUT, host.decorate_reply(request)).await {
                Ok(Ok(result)) if result.modified => segments = result.segments,
                Ok(Ok(_)) => {}
                Ok(Err(status)) => tracing::warn!(
                    host_id = %host.host_id,
                    plugin_id = %plugin.id,
                    error = %status,
                    "Reply decorator failed; keeping the reply as it was"
                ),
                Err(_) => tracing::warn!(
                    host_id = %host.host_id,
                    plugin_id = %plugin.id,
                    "Reply decorator timed out; keeping the reply as it was"
                ),
            }
        }
    }
    segments
}
