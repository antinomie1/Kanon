//! Plugin lifecycle events, turn preparation and reply decoration.
//!
//! All are opt-in through plugin metadata (`PluginMeta.events`, `PluginMeta.prepares_turns`,
//! `PluginMeta.decorates_replies`), so a host is only called for what one of its plugins asked for.
//!
//! Events are fire-and-forget: each notification runs on its own task with a timeout, because the
//! pipeline worker handles one event at a time and a slow subscriber must never delay the next
//! message. Decoration, in contrast, sits on the reply path and is awaited, so each decorator gets
//! a short deadline and any failure keeps the reply as it was — a broken decorator may cost its own
//! effect, never the reply. Turn preparation sits in front of the model call and follows the same
//! rule: a preparer that fails or is late contributes nothing, and the turn goes ahead without it.

use std::sync::Arc;
use std::time::Duration;

use kanon_proto::v1::{
    DecorateReplyRequest, EventKind, EventNotification, MessageSegment, PipelineEventRequest,
    PrepareTurnRequest, ReplySource, event_notification::Detail,
};

use crate::supervisor::ManagedHost;

/// Deadline for one event notification.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadline for one decorator; it delays the reply, so it is kept short.
pub const DECORATE_TIMEOUT: Duration = Duration::from_secs(3);

/// Deadline for one turn preparer. Preparers run concurrently, so this bounds the whole phase.
pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(3);

/// Collects the context every preparing plugin adds to the turn the model is about to answer.
///
/// Preparers run concurrently — each is a remote lookup (memory, retrieval) and the user is
/// waiting — but the texts are returned in host priority order (then `host_id`, then the plugin's
/// position in its host), so the same plugins always produce the same layout. Empty texts,
/// failures and late answers contribute nothing.
pub async fn prepare_turn(
    hosts: &[Arc<ManagedHost>],
    context: &PipelineEventRequest,
    session_id: &str,
) -> Vec<String> {
    let mut ordered: Vec<&Arc<ManagedHost>> = hosts.iter().collect();
    ordered.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.host_id.cmp(&b.host_id))
    });

    let mut calls = Vec::new();
    for host in ordered {
        for plugin in host.metas() {
            if !plugin.prepares_turns {
                continue;
            }
            let host = host.clone();
            let request = PrepareTurnRequest {
                plugin_id: plugin.id.clone(),
                context: Some(context.clone()),
                session_id: session_id.to_string(),
            };
            calls.push(async move {
                match tokio::time::timeout(PREPARE_TIMEOUT, host.prepare_turn(request)).await {
                    Ok(Ok(result)) => Some(result.text).filter(|text| !text.trim().is_empty()),
                    Ok(Err(status)) => {
                        tracing::warn!(
                            host_id = %host.host_id,
                            plugin_id = %plugin.id,
                            error = %status,
                            "Turn preparer failed; the turn goes ahead without its context"
                        );
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            host_id = %host.host_id,
                            plugin_id = %plugin.id,
                            "Turn preparer timed out; the turn goes ahead without its context"
                        );
                        None
                    }
                }
            });
        }
    }
    futures_util::future::join_all(calls)
        .await
        .into_iter()
        .flatten()
        .collect()
}

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
