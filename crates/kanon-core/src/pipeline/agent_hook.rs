//! Plugins inside the agent's turn: system prompt rewrites and tool call events.
//!
//! A turn that answers a chat runs inside [`with_turn`], which names the inbound message and the
//! plugins the answering instance runs (its plugin policy already applied). [`PluginAgentHook`],
//! registered on every agent of the node, reads that scope from within the agent's loop:
//!
//! - before each model request it lets plugins with `PluginMeta.rewrites_system_prompt` rewrite
//!   the system prompt (`OnLlmRequest`);
//! - around each tool call it tells subscribers `TOOL_CALL` and `TOOL_RESULT`.
//!
//! Outside a scope (the console's chat, a plugin's private agent run, a background compaction)
//! plugins are never called, so a plugin's own model calls cannot recurse into its hooks.
//!
//! # Why rewrites are remembered per session
//! The system prompt heads every request, so its bytes decide the provider's prompt cache. The
//! plugins are asked once per turn, at its first request; the turn's later tool rounds and the
//! compaction that may follow it (which runs on its own task, outside the scope) reuse that
//! answer, so every request of the turn and its summary share one prefix. An answer is reused only
//! while the system prompt it was given is unchanged: when the operator edits the persona or a
//! skill, the stale rewrite is dropped instead of masking the edit.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::{AgentError, AgentHook, ChatMessage, ChatRequest, Role, ToolCall};
use kanon_proto::v1::{
    EventKind, LlmRequestHookRequest, PipelineEventRequest, ToolCallEvent, ToolResultEvent,
    event_notification::Detail,
};

use super::hooks::{PREPARE_TIMEOUT, emit_event};
use crate::supervisor::ManagedHost;

/// Most sessions whose rewrite is remembered; the oldest is forgotten first.
///
/// A rewrite is only needed again by its own turn's tool rounds and by the compaction right after
/// the turn, so recent sessions are all that matter. A forgotten one costs one cache miss on a
/// compaction, never a wrong prompt.
const REMEMBERED_SESSIONS: usize = 256;

tokio::task_local! {
    // Task scope (not a field) because one agent serves overlapping turns of different chats;
    // each turn's hooks must see their own message and plugins, never a neighbour's.
    static TURN: TurnScope;
}

/// What a turn's hooks need to reach its plugins.
#[derive(Clone)]
struct TurnScope {
    /// The inbound message the turn answers.
    event: PipelineEventRequest,
    /// Hosts of the plugins the answering instance runs.
    hosts: Vec<Arc<ManagedHost>>,
}

/// Runs `turn` as the answer to `event`, with `hosts` taking part in it.
///
/// Inside, plugin tool calls carry `event` as their context (see
/// [`crate::supervisor::with_tool_event`]) and [`PluginAgentHook`] calls the plugins of `hosts`.
pub async fn with_turn<F: Future>(
    event: PipelineEventRequest,
    hosts: Vec<Arc<ManagedHost>>,
    turn: F,
) -> F::Output {
    let scope = TurnScope {
        event: event.clone(),
        hosts,
    };
    crate::supervisor::with_tool_event(event, TURN.scope(scope, turn)).await
}

/// The agent hook that brings plugins into a turn; see the module documentation.
#[derive(Default)]
pub struct PluginAgentHook {
    /// The last rewrite of each recent session.
    ///
    /// A synchronous lock: it is held only to look up or replace an entry, never across an await
    /// (the plugins are called with the lock released).
    rewrites: Mutex<Rewrites>,
}

#[derive(Default)]
struct Rewrites {
    /// Stamp of the next entry; larger is newer.
    next_stamp: u64,
    sessions: HashMap<String, Rewrite>,
}

/// One turn's answer from the session's rewriting plugins.
struct Rewrite {
    /// The inbound message of the turn that asked.
    event_id: String,
    /// Hash of the system prompt the plugins were shown (the full text is not worth keeping).
    base: u64,
    /// What they made of it; `None` when none changed it.
    rewritten: Option<String>,
    /// When the entry was stored, for forgetting the oldest.
    stamp: u64,
}

impl PluginAgentHook {
    /// Creates the hook with nothing remembered.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Rewrites> {
        // Each critical section is one map lookup, insert or removal and cannot leave the map
        // half-updated, so a panic elsewhere is no reason to stop rewriting prompts.
        self.rewrites
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The remembered answer for `session_id`, if it was given `base` (and, when `event_id` is
    /// given, in that turn).
    fn remembered(
        &self,
        session_id: &str,
        event_id: Option<&str>,
        base: u64,
    ) -> Option<Option<String>> {
        let rewrites = self.lock();
        let rewrite = rewrites.sessions.get(session_id)?;
        (rewrite.base == base && event_id.is_none_or(|id| id == rewrite.event_id))
            .then(|| rewrite.rewritten.clone())
    }

    fn remember(&self, session_id: &str, event_id: String, base: u64, rewritten: Option<String>) {
        let mut rewrites = self.lock();
        if !rewrites.sessions.contains_key(session_id)
            && rewrites.sessions.len() >= REMEMBERED_SESSIONS
            && let Some(oldest) = rewrites
                .sessions
                .iter()
                .min_by_key(|(_, rewrite)| rewrite.stamp)
                .map(|(session, _)| session.clone())
        {
            rewrites.sessions.remove(&oldest);
        }
        let stamp = rewrites.next_stamp;
        rewrites.next_stamp += 1;
        rewrites.sessions.insert(
            session_id.to_string(),
            Rewrite {
                event_id,
                base,
                rewritten,
                stamp,
            },
        );
    }

    fn forget(&self, session_id: &str) {
        self.lock().sessions.remove(session_id);
    }
}

#[async_trait]
impl AgentHook for PluginAgentHook {
    async fn on_llm_request(
        &self,
        session_id: &str,
        request: &mut ChatRequest,
    ) -> Result<(), AgentError> {
        let base = kanon_llm::layout::system_text(&request.messages);
        let base_hash = hash(&base);
        let rewritten = match TURN.try_with(Clone::clone) {
            Ok(scope) => {
                let rewriters = rewriters(&scope.hosts);
                if rewriters.is_empty() {
                    // Nothing rewrites this session's prompt (any more); a remembered rewrite
                    // must not resurface in its compaction.
                    self.forget(session_id);
                    return Ok(());
                }
                match self.remembered(session_id, Some(&scope.event.event_id), base_hash) {
                    Some(remembered) => remembered,
                    None => {
                        let rewritten =
                            rewrite_system_prompt(&rewriters, &scope.event, session_id, &base)
                                .await;
                        self.remember(
                            session_id,
                            scope.event.event_id.clone(),
                            base_hash,
                            rewritten.clone(),
                        );
                        rewritten
                    }
                }
            }
            // Outside a turn only a remembered answer applies: the same session's compaction
            // must send the prefix its turn sent.
            Err(_) => self.remembered(session_id, None, base_hash).flatten(),
        };
        if let Some(text) = rewritten {
            let leading = request
                .messages
                .iter()
                .take_while(|message| message.role == Role::System)
                .count();
            request.messages.drain(..leading);
            request.messages.insert(0, ChatMessage::system(text));
        }
        Ok(())
    }

    async fn on_before_tool_call(
        &self,
        session_id: &str,
        call: &ToolCall,
    ) -> Result<bool, AgentError> {
        if let Ok(scope) = TURN.try_with(Clone::clone) {
            emit_event(
                &scope.hosts,
                EventKind::ToolCall,
                Detail::ToolCall(ToolCallEvent {
                    context: Some(scope.event),
                    session_id: session_id.to_string(),
                    tool_name: call.name.clone(),
                    arguments: kanon_llm::tool_router::json_to_prost_struct(&call.arguments),
                }),
            );
        }
        // Observation only: subscribers cannot veto a call.
        Ok(true)
    }

    async fn on_after_tool_call(
        &self,
        session_id: &str,
        call: &ToolCall,
        result: &str,
        success: bool,
    ) -> Result<(), AgentError> {
        if let Ok(scope) = TURN.try_with(Clone::clone) {
            emit_event(
                &scope.hosts,
                EventKind::ToolResult,
                Detail::ToolResult(ToolResultEvent {
                    context: Some(scope.event),
                    session_id: session_id.to_string(),
                    tool_name: call.name.clone(),
                    success,
                    result: result.to_string(),
                }),
            );
        }
        Ok(())
    }
}

fn hash(text: &str) -> u64 {
    // SipHash with fixed keys: stable for the life of the process, which is all a cache key needs.
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The plugins of `hosts` that rewrite system prompts, in the order they are asked: host
/// priority, then host id, then the plugin's position in its host — the order of
/// [`super::hooks::decorate_reply`], so the same plugins always produce the same prompt.
fn rewriters(hosts: &[Arc<ManagedHost>]) -> Vec<(Arc<ManagedHost>, String)> {
    let mut ordered: Vec<&Arc<ManagedHost>> = hosts.iter().collect();
    ordered.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.host_id.cmp(&b.host_id))
    });
    ordered
        .into_iter()
        .flat_map(|host| {
            host.metas()
                .into_iter()
                .filter(|plugin| plugin.rewrites_system_prompt)
                .map(|plugin| (host.clone(), plugin.id))
        })
        .collect()
}

/// Passes `base` through every rewriter in turn; `None` when the result is unchanged.
///
/// Each plugin sees the previous one's result. One that fails, is late (the turn preparers'
/// deadline) or answers with an empty prompt contributes nothing, and the next one goes on from
/// the prompt as it was: a broken plugin may cost its own effect, never the turn.
async fn rewrite_system_prompt(
    rewriters: &[(Arc<ManagedHost>, String)],
    context: &PipelineEventRequest,
    session_id: &str,
    base: &str,
) -> Option<String> {
    let mut prompt = base.to_string();
    for (host, plugin_id) in rewriters {
        let request = LlmRequestHookRequest {
            plugin_id: plugin_id.clone(),
            context: Some(context.clone()),
            session_id: session_id.to_string(),
            system_prompt: prompt.clone(),
        };
        match tokio::time::timeout(PREPARE_TIMEOUT, host.rewrite_system_prompt(request)).await {
            Ok(Ok(result)) => match result.system_prompt {
                Some(text) if text.trim().is_empty() => tracing::warn!(
                    host_id = %host.host_id,
                    plugin_id = %plugin_id,
                    "Plugin returned an empty system prompt; keeping the prompt as it was"
                ),
                Some(text) => prompt = text,
                None => {}
            },
            Ok(Err(status)) => tracing::warn!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                error = %status,
                "System prompt hook failed; keeping the prompt as it was"
            ),
            Err(_) => tracing::warn!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                "System prompt hook timed out; keeping the prompt as it was"
            ),
        }
    }
    (prompt != base).then_some(prompt)
}
