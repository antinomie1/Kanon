//! Message lifecycle trace bus backing `/ws/v1/events`.
//!
//! The bus is the single choke point through which every observable lifecycle transition
//! flows: the core pipeline publishes [`PipelineStage`] records through the
//! [`PipelineObserver`] implementation, while the LLM agent publishes reasoning and tool
//! calling stages through [`TraceAgentHook`](super::hooks::TraceAgentHook). Because both
//! feed one broadcast channel, the console can reconstruct a full timeline — ingest,
//! pre-filter, command match, tool calling, outbound delivery — from a single ordered stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kanon_core::PipelineStage;
use kanon_llm::AgentHook;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::metrics::MetricsRegistry;

/// Lifecycle transitions published to console subscribers.
///
/// Every record carries a stable `kind` tag. Pipeline stages are nested as `kind: "pipeline"`
/// with a finer `stage` field (`ingested`, `pre_filter_blocked`, ...), so consoles can either
/// follow the whole pipeline (`kind=pipeline`) or subscribe to one stage (`kind=ingested`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceEvent {
    /// A pipeline lifecycle stage forwarded verbatim from the core engine.
    Pipeline(PipelineStage),
    /// The agent is about to transmit a request to the model provider.
    LlmRequest {
        /// Session key the request belongs to.
        session_id: String,
        /// Model identifier being invoked.
        model: String,
        /// Number of conversational messages in the request.
        message_count: usize,
        /// Number of tool definitions offered to the model.
        tool_count: usize,
    },
    /// The provider returned a completion.
    LlmResponse {
        /// Session key the response belongs to.
        session_id: String,
        /// Whether the model requested tool execution.
        requested_tools: bool,
        /// Character length of the returned text.
        content_length: usize,
        /// Provider finish reason when reported.
        finish_reason: Option<String>,
        /// Prompt tokens the provider reported for the request (0 when it reports none).
        prompt_tokens: u32,
        /// Part of `prompt_tokens` served from the provider's prompt cache (0 when unreported).
        cached_tokens: u32,
    },
    /// A tool call passed the policy gate and is being dispatched.
    ToolCallStarted {
        /// Session key the call belongs to.
        session_id: String,
        /// Provider-allocated call identifier.
        call_id: String,
        /// Tool being invoked.
        tool_name: String,
    },
    /// A tool call finished, successfully or otherwise.
    ToolCallFinished {
        /// Session key the call belongs to.
        session_id: String,
        /// Provider-allocated call identifier.
        call_id: String,
        /// Tool that was invoked.
        tool_name: String,
        /// Whether the tool reported success.
        success: bool,
    },
    /// A tool call was vetoed by an agent policy hook.
    ToolCallDenied {
        /// Session key the call belongs to.
        session_id: String,
        /// Tool that was refused.
        tool_name: String,
    },
    /// Conversation history was cleared while configuration and persona were preserved.
    SessionReset {
        /// Session that was reset.
        session_id: String,
    },
    /// The active persona of a session changed.
    PersonaSwitched {
        /// Session whose persona changed.
        session_id: String,
        /// Newly bound persona identifier.
        persona_id: String,
    },
    /// A plugin configuration update was accepted and pushed to its host.
    PluginConfigUpdated {
        /// Plugin whose configuration changed.
        plugin_id: String,
        /// Host process that received the reload.
        host_id: String,
    },
    /// A plugin host process was restarted by the control plane.
    PluginRestarted {
        /// Host process that was restarted.
        host_id: String,
    },
    /// A new plugin was installed and activated.
    PluginInstalled {
        /// Identifier of the installed plugin.
        plugin_id: String,
        /// Host process identifier.
        host_id: String,
    },
}

impl TraceEvent {
    /// Returns the stable event kind name used on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            TraceEvent::Pipeline(_) => "pipeline",
            TraceEvent::LlmRequest { .. } => "llm_request",
            TraceEvent::LlmResponse { .. } => "llm_response",
            TraceEvent::ToolCallStarted { .. } => "tool_call_started",
            TraceEvent::ToolCallFinished { .. } => "tool_call_finished",
            TraceEvent::ToolCallDenied { .. } => "tool_call_denied",
            TraceEvent::SessionReset { .. } => "session_reset",
            TraceEvent::PersonaSwitched { .. } => "persona_switched",
            TraceEvent::PluginConfigUpdated { .. } => "plugin_config_updated",
            TraceEvent::PluginRestarted { .. } => "plugin_restarted",
            TraceEvent::PluginInstalled { .. } => "plugin_installed",
        }
    }

    /// Returns the pipeline stage name for pipeline events, `None` otherwise.
    ///
    /// Subscribers may filter on either granularity: `kind=pipeline` for the whole lifecycle or
    /// `kind=ingested` for a single stage.
    pub fn stage_name(&self) -> Option<&'static str> {
        match self {
            TraceEvent::Pipeline(stage) => Some(stage.name()),
            _ => None,
        }
    }

    /// Returns `true` when this event satisfies a subscriber's kind filter entry.
    pub fn matches_kind(&self, filter: &str) -> bool {
        self.kind() == filter || self.stage_name() == Some(filter)
    }

    /// Returns the session key associated with this event, when applicable.
    ///
    /// Used by `/ws/v1/events` subscribers to isolate a single conversation timeline.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            TraceEvent::Pipeline(PipelineStage::LlmReplied { session_id, .. }) => Some(session_id),
            TraceEvent::LlmRequest { session_id, .. }
            | TraceEvent::LlmResponse { session_id, .. }
            | TraceEvent::ToolCallStarted { session_id, .. }
            | TraceEvent::ToolCallFinished { session_id, .. }
            | TraceEvent::ToolCallDenied { session_id, .. }
            | TraceEvent::SessionReset { session_id }
            | TraceEvent::PersonaSwitched { session_id, .. } => Some(session_id),
            _ => None,
        }
    }
}

/// Ordered envelope wrapping every published event.
#[derive(Debug, Clone, Serialize)]
pub struct TraceRecord {
    /// Monotonic sequence number assigned by the bus at publish time.
    pub seq: u64,
    /// Unix timestamp in milliseconds at publish time.
    pub timestamp_ms: u64,
    /// The lifecycle event itself.
    pub event: TraceEvent,
}

/// Broadcast bus for lifecycle trace records.
pub struct EventBus {
    sender: broadcast::Sender<TraceRecord>,
    metrics: Arc<MetricsRegistry>,
    sequence: AtomicU64,
}

impl EventBus {
    /// Creates a trace bus with the given broadcast buffer depth.
    pub fn new(capacity: usize, metrics: Arc<MetricsRegistry>) -> Arc<Self> {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Arc::new(Self {
            sender,
            metrics,
            sequence: AtomicU64::new(0),
        })
    }

    /// Subscribes a new console client to the trace stream.
    pub fn subscribe(&self) -> broadcast::Receiver<TraceRecord> {
        self.sender.subscribe()
    }

    /// Number of clients currently subscribed to the trace stream.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Publishes an event, returning the sequence number assigned to it.
    ///
    /// Metrics are updated here rather than at each call site so that the bus stays the
    /// single, auditable choke point for observability accounting.
    pub fn publish(&self, event: TraceEvent) -> u64 {
        self.account(&event);

        let seq = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let record = TraceRecord {
            seq,
            timestamp_ms: current_timestamp_ms(),
            event,
        };

        MetricsRegistry::incr(&self.metrics.trace_events);
        // An absent subscriber is a normal state (no console attached), not a failure.
        let _ = self.sender.send(record);
        seq
    }

    /// Updates the counters implied by an event kind.
    fn account(&self, event: &TraceEvent) {
        match event {
            TraceEvent::Pipeline(stage) => match stage {
                PipelineStage::Ingested { .. } => {
                    MetricsRegistry::incr(&self.metrics.events_ingested);
                }
                PipelineStage::PreFilterBlocked { .. } => {
                    MetricsRegistry::incr(&self.metrics.events_blocked);
                }
                PipelineStage::PreFilterPassed { .. } => {
                    MetricsRegistry::incr(&self.metrics.events_passed);
                }
                PipelineStage::CommandMatched { .. } => {
                    MetricsRegistry::incr(&self.metrics.commands_matched);
                }
                PipelineStage::CommandNotFound { .. } => {
                    MetricsRegistry::incr(&self.metrics.commands_not_found);
                }
                PipelineStage::LlmReplied { .. } => {
                    MetricsRegistry::incr(&self.metrics.llm_replies);
                }
                PipelineStage::OutboundQueued { segment_count, .. } => {
                    MetricsRegistry::add(&self.metrics.outbound_messages, *segment_count as u64);
                }
                PipelineStage::OutboundDelivered { .. } => {
                    MetricsRegistry::incr(&self.metrics.outbound_delivered);
                }
                PipelineStage::OutboundFailed { .. } => {
                    MetricsRegistry::incr(&self.metrics.outbound_failed);
                }
                PipelineStage::PreFilterStarted { .. }
                | PipelineStage::CircuitBreakerTripped { .. } => {}
            },
            TraceEvent::LlmRequest { .. } => {
                MetricsRegistry::incr(&self.metrics.llm_requests);
            }
            TraceEvent::ToolCallStarted { .. } => {
                MetricsRegistry::incr(&self.metrics.tool_calls);
            }
            TraceEvent::ToolCallFinished { success, .. } => {
                if !success {
                    MetricsRegistry::incr(&self.metrics.tool_call_failures);
                }
            }
            TraceEvent::LlmResponse {
                prompt_tokens,
                cached_tokens,
                ..
            } => {
                MetricsRegistry::add(&self.metrics.llm_prompt_tokens, u64::from(*prompt_tokens));
                MetricsRegistry::add(&self.metrics.llm_cached_tokens, u64::from(*cached_tokens));
            }
            TraceEvent::ToolCallDenied { .. }
            | TraceEvent::SessionReset { .. }
            | TraceEvent::PersonaSwitched { .. }
            | TraceEvent::PluginConfigUpdated { .. }
            | TraceEvent::PluginRestarted { .. }
            | TraceEvent::PluginInstalled { .. } => {}
        }
    }
}

impl kanon_core::PipelineObserver for EventBus {
    fn on_stage(&self, stage: &PipelineStage) {
        self.publish(TraceEvent::Pipeline(stage.clone()));
    }
}

#[async_trait::async_trait]
impl AgentHook for EventBus {
    async fn on_llm_request(
        &self,
        session_id: &str,
        request: &mut kanon_llm::ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        self.publish(TraceEvent::LlmRequest {
            session_id: session_id.to_string(),
            model: request.model.clone(),
            message_count: request.messages.len(),
            tool_count: request.tools.len(),
        });
        Ok(())
    }

    async fn on_llm_response(
        &self,
        session_id: &str,
        response: &mut kanon_llm::ChatResponse,
    ) -> Result<(), kanon_llm::AgentError> {
        self.publish(TraceEvent::LlmResponse {
            session_id: session_id.to_string(),
            requested_tools: !response.tool_calls.is_empty(),
            content_length: response.content.as_deref().map(str::len).unwrap_or(0),
            finish_reason: response.finish_reason.clone(),
            prompt_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.prompt_tokens),
            cached_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.cached_tokens),
        });
        Ok(())
    }

    async fn on_before_tool_call(
        &self,
        session_id: &str,
        call: &kanon_llm::ToolCall,
    ) -> Result<bool, kanon_llm::AgentError> {
        self.publish(TraceEvent::ToolCallStarted {
            session_id: session_id.to_string(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
        });
        // The bus observes; it never vetoes. Policy decisions belong to dedicated hooks.
        Ok(true)
    }

    async fn on_after_tool_call(
        &self,
        session_id: &str,
        call: &kanon_llm::ToolCall,
        _result: &str,
        success: bool,
    ) -> Result<(), kanon_llm::AgentError> {
        self.publish(TraceEvent::ToolCallFinished {
            session_id: session_id.to_string(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            success,
        });
        Ok(())
    }
}

/// Current Unix timestamp in milliseconds, saturating at the epoch on clock skew.
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}
