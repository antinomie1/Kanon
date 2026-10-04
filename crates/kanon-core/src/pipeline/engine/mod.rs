//! Central Pipeline Engine and asynchronous worker loop.
//!
//! Consumes inbound events from the CoreApiService MPSC queue, sequences them through
//! the PreFilter interception chain and CommandRouter, and dispatches outbound replies to the
//! platform adapter that owns the destination platform.
//!
//! # Why outbound is queued instead of awaited
//! Platform delivery performs network I/O against an external service. Awaiting it inside the
//! pipeline worker would let one slow platform stall every other conversation (head-of-line
//! blocking), so replies are handed to a bounded queue drained by an independent dispatcher task.
//! The queue is deliberately bounded: when a platform cannot keep up, the overflow is reported as
//! an explicit `OutboundFailed` stage instead of growing memory without limit.

#[cfg(feature = "dsh")]
mod agent_commands;
mod commands;
mod inbound;
mod message;
mod outbound;
mod worker;
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
pub use message::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{AgentFactory, AgentSlot, ModelCapabilities, ModelRef, ModelSpec, visible_reply};
use kanon_proto::v1::event_notification::Detail;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AgentBeginEvent, AgentDoneEvent, CommandMeta, DeliverMessageRequest, DeliverMessageResponse,
    EventKind, IngestEventRequest, LlmResponseEvent, MessageSegment, MessageSentEvent,
    PipelineEventRequest, ReplySource,
};

use crate::access::{CommandAccess, CommandPolicyStore};
use crate::adapter::{AdapterDescriptor, AdapterError, AdapterKind};
use crate::conversation::{ContextPolicyStore, ConversationKind, ReplyPolicyStore, bot_mentioned};
use crate::instance::InstanceRegistry;
use crate::instance::SessionScope;
use crate::mcp::McpPool;
use crate::notice::{
    EventPolicyStore, META_NOTICE_ACTOR, META_NOTICE_TARGET, NoticeKind, RecallLedger, metadata_str,
};
use crate::pipeline::capture::CaptureRegistry;
use crate::pipeline::command::{CommandRouter, TriggerMatcher};
use crate::pipeline::context::build_user_message;
use crate::pipeline::dead_letter::DeadLetterWriter;
use crate::pipeline::group_log::GroupLog;
use crate::pipeline::hooks;
use crate::pipeline::observer::{PipelineObserver, PipelineStage};
use crate::pipeline::pre_filter::{PreFilterChain, PreFilterOutcome};
use crate::pipeline::reply::split_reply_lines;
use crate::supervisor::circuit_breaker::{CircuitBreaker, CircuitState};
use crate::supervisor::{AdapterRoute, Supervisor};
use crate::toggle::ToggleStore;

/// Maximum independently running inbound chats; queued work remains bounded by ingest capacity.
pub const MAX_CONCURRENT_CHATS: usize = 16;

mod simulation_runtime;

/// Default depth of the outbound delivery queue.
///
/// Sized so that a slow platform absorbs a burst of replies without dropping any, while keeping
/// worst-case memory bounded.
pub const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 1024;

/// Default depth of the dedicated outbound delivery queue for each platform partition.
///
/// Preserves sequential FIFO delivery per platform while providing strict cross-platform
/// concurrency isolation so one slow platform never starves others.
pub const DEFAULT_PLATFORM_QUEUE_CAPACITY: usize = 64;

/// Aggregate grace for every event already running when shutdown starts.
///
/// Container runtimes kill a process shortly after asking it to stop (Docker waits 10 s by
/// default), so this grace and [`SHUTDOWN_DELIVERY_GRACE`] together stay below that budget.
/// Events still queued are never started during shutdown: they go to the dead-letter log at once.
pub const SHUTDOWN_EVENT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// How long queued replies may take to reach their platforms once the pipeline has drained.
pub const SHUTDOWN_DELIVERY_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Shutdown progress of the pipeline, advanced only by [`PipelineEngine::drain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownPhase {
    /// Normal operation.
    Running,
    /// The ingest queue is closed; the worker records queued events and finishes active lanes.
    DrainingInbound,
    /// The outbound queue is closed; replies are delivered until the deadline, then recorded.
    DrainingOutbound(tokio::time::Instant),
}

/// Waits until the shutdown phase satisfies `reached` and returns it.
///
/// Never resolves if the engine (the only sender) is gone, which cannot happen while a loop that
/// borrows the engine is still running.
async fn wait_for_phase(
    phase: &mut watch::Receiver<ShutdownPhase>,
    reached: fn(ShutdownPhase) -> bool,
) -> ShutdownPhase {
    loop {
        let current = *phase.borrow_and_update();
        if reached(current) {
            return current;
        }
        if phase.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

/// Resolves once shutdown has set a delivery deadline and that deadline has passed.
async fn delivery_deadline_passed(phase: &mut watch::Receiver<ShutdownPhase>) {
    if let ShutdownPhase::DrainingOutbound(deadline) =
        wait_for_phase(phase, |p| matches!(p, ShutdownPhase::DrainingOutbound(_))).await
    {
        tokio::time::sleep_until(deadline).await;
    }
}

/// One queued reply, optionally split into deliveries and carrying a completion receipt.
///
/// Keeping the receipt with the request preserves FIFO ordering and makes queue
/// drops explicit without a second registry of messages to reconcile.
#[derive(Debug)]
pub struct OutboundMessage {
    /// Original platform event and reply segments, unchanged by queueing.
    pub request: DeliverMessageRequest,
    /// Split this model reply only after dequeueing, so all its lines share one queue slot.
    pub split_lines: bool,
    /// Resolved only after platform delivery or an explicit dispatch failure.
    pub receipt: Option<oneshot::Sender<DeliverMessageResponse>>,
}

impl From<DeliverMessageRequest> for OutboundMessage {
    fn from(request: DeliverMessageRequest) -> Self {
        Self {
            request,
            split_lines: false,
            receipt: None,
        }
    }
}

/// Result produced after processing an event through the pipeline engine.
#[derive(Debug, Clone)]
pub enum PipelineResult {
    /// A simulation owner or its mailbox accepted the event; speech uses explicit tools.
    SimulationHandled,
    /// Inbound event was intercepted and blocked by a PreFilter plugin.
    Blocked {
        /// Identifier of the host that blocked the event.
        host_id: String,
        /// Outbound reply messages generated by the blocking pre-filter.
        replies: Vec<MessageSegment>,
    },
    /// A registered slash command was matched and executed by a plugin host.
    CommandExecuted {
        /// The slash command name executed (without leading slash).
        command: String,
        /// Target plugin identifier that handled the command.
        plugin_id: String,
        /// Identifier of the host process running the plugin.
        host_id: String,
        /// Execution status returned by the plugin host.
        success: bool,
        /// Outbound reply segments generated by the command handler.
        replies: Vec<MessageSegment>,
    },
    /// A slash command syntax was parsed, but no registered plugin matched the command name.
    CommandNotFound {
        /// Command name that failed to match.
        command: String,
    },
    /// Inbound conversational message was processed by LLM reasoning and generated a reply.
    LlmReplied {
        /// Natural language text generated by the model.
        content: String,
        /// Outbound reply segments generated for the conversational response.
        replies: Vec<MessageSegment>,
        /// Delivery-only formatting captured from the effective policy for this event.
        split_lines: bool,
    },
    /// The model turn failed after the sender asked for an answer. They are told once; the turn
    /// is never re-run.
    LlmFailed {
        /// The failure, for logs and traces.
        error: String,
        /// Short notice delivered back to the conversation.
        replies: Vec<MessageSegment>,
    },
    /// The built-in `/new` command rotated the session of one conversation.
    SessionRotated {
        /// Instance whose conversation was rotated.
        instance_id: String,
        /// Freshly created session identifier; the previous session is retained.
        session_id: String,
        /// Confirmation delivered back to the conversation.
        replies: Vec<MessageSegment>,
    },
    /// The built-in `/model` command selected a model for one instance.
    ModelSelected {
        /// Instance whose model override was changed.
        instance_id: String,
        /// Canonical `<provider>/<model-id>` reference now in effect.
        model: String,
        /// Confirmation delivered back to the conversation.
        replies: Vec<MessageSegment>,
    },
    /// The built-in `/model` command listed the models an operator may switch to.
    ModelListed {
        /// Instance the listing was produced for.
        instance_id: String,
        /// Number of listed models.
        count: usize,
        /// Listing delivered back to the conversation.
        replies: Vec<MessageSegment>,
    },
    /// The instance's reply policy decided not to answer this event.
    ReplySuppressed {
        /// Instance whose policy suppressed the reply.
        instance_id: String,
        /// Human-readable reason, suitable for logs and traces.
        reason: String,
    },
    /// A built-in informational command (`/help`, `/info`) answered by the core itself.
    BuiltinReplied {
        /// Built-in command that produced the answer.
        command: String,
        /// Answer delivered back to the conversation.
        replies: Vec<MessageSegment>,
    },
    /// A command the sender is not allowed to run under the node's command policy.
    CommandDenied {
        /// Command that was refused (without the slash).
        command: String,
        /// Explanation delivered back to the sender.
        replies: Vec<MessageSegment>,
    },
    /// A platform notice (join, poke, recall) that the event policy does not answer.
    Notice {
        /// Notice kind, as reported by the adapter.
        kind: &'static str,
        /// What happened to it, suitable for logs and traces.
        outcome: String,
    },
    /// No enabled bot instance claims the event's platform, so nothing may answer it.
    NoInstance {
        /// Platform that nobody claimed.
        platform: String,
    },
    /// Inbound event passed through the pipeline without matching any slash command or LLM rule.
    Passed(PipelineEventRequest),
}

impl PipelineResult {
    /// The segments to deliver back to the conversation; empty when the event is answered by
    /// nothing (a passed event, a suppressed reply, an unclaimed platform).
    pub fn replies(&self) -> &[MessageSegment] {
        match self {
            Self::Blocked { replies, .. }
            | Self::CommandExecuted { replies, .. }
            | Self::LlmReplied { replies, .. }
            | Self::LlmFailed { replies, .. }
            | Self::SessionRotated { replies, .. }
            | Self::ModelSelected { replies, .. }
            | Self::ModelListed { replies, .. }
            | Self::BuiltinReplied { replies, .. }
            | Self::CommandDenied { replies, .. } => replies,
            Self::SimulationHandled
            | Self::CommandNotFound { .. }
            | Self::ReplySuppressed { .. }
            | Self::Notice { .. }
            | Self::NoInstance { .. }
            | Self::Passed(_) => &[],
        }
    }
}

/// A model conversation as stored, read by [`PipelineEngine::conversation_history`].
#[derive(Debug, Clone)]
pub struct ConversationHistory {
    /// Session the conversation is stored under.
    pub session_id: String,
    /// Summary of compacted older turns, if the conversation was ever compacted.
    pub summary: Option<String>,
    /// Stored messages, oldest first, including tool calls and tool results.
    pub messages: Vec<kanon_llm::ChatMessage>,
}

/// What happens to a message after a plugin command, trigger or continuation handled it.
enum CommandFlow {
    /// The handler answered; the pipeline ends with this result.
    Finished(PipelineResult),
    /// The handler handed the message on: the reply policy and the model see this event next.
    PassToModel(PipelineEventRequest),
}

/// Central event processing engine driving the message pipeline.
pub struct PipelineEngine {
    /// Reference to the process supervisor managing active plugin hosts and adapters.
    supervisor: Arc<Supervisor>,
    /// Live agent runtime driving LLM reasoning and cross-language tool calling.
    ///
    /// Held as a shared slot rather than a captured router so that configuring, replacing or
    /// clearing the model provider on a running node takes effect on the very next event.
    agent: Arc<AgentSlot>,
    /// Factory building per-instance agents that share the node's memory, personas and hooks.
    ///
    /// Optional: without it the pipeline cannot honour a per-instance model override and falls
    /// back to the node agent, which is the documented behaviour of embedded deployments.
    agent_factory: Option<Arc<AgentFactory>>,
    /// Bot instances deciding whether and how an inbound event is answered.
    ///
    /// Optional for the same reason: an unpartitioned pipeline (tests, embedded cores) processes
    /// every event exactly as before.
    instances: Option<Arc<InstanceRegistry>>,
    /// Global enable switches shared with the control plane.
    toggles: Option<Arc<ToggleStore>>,
    /// Node-wide reply policy, when the control plane provides one.
    ///
    /// Optional for the same reason as the instance catalog: an embedded pipeline without a policy
    /// store keeps answering everything, which is the pre-policy behaviour.
    reply_policy: Option<Arc<ReplyPolicyStore>>,
    /// Node-wide context-extras policy, when the control plane provides one.
    context_policy: Option<Arc<ContextPolicyStore>>,
    /// Node-wide notice policy; without one only the defaults apply (recall notes, no reactions).
    event_policy: Option<Arc<EventPolicyStore>>,
    /// Node-wide command permissions; without one only the default restrictions apply.
    command_policy: Option<Arc<CommandPolicyStore>>,
    /// Messages the model answered and recall notes awaiting their conversation's next turn.
    recalls: RecallLedger,
    /// Recent group lines for instances that observe their groups.
    group_log: GroupLog,
    /// Compiled plugin trigger patterns.
    triggers: TriggerMatcher,
    /// Plugins waiting for a sender's next message.
    captures: CaptureRegistry,
    /// Model turns in progress, which `/stop` can end.
    turns: crate::pipeline::turns::RunningTurns,
    /// Bounded mailboxes for active conversational participation.
    simulation: simulation_runtime::SimulationHub,
    /// MCP servers contributing tools alongside plugin hosts.
    mcp: Option<Arc<McpPool>>,
    /// Optional lifecycle observer used by the management control plane for tracing.
    observer: Option<Arc<dyn PipelineObserver>>,
    /// Persistent dead-letter queue writer for failed or dropped outbound messages.
    dead_letter: Arc<DeadLetterWriter>,
    /// Producer side of the bounded outbound delivery queue.
    outbound_sender: mpsc::Sender<OutboundMessage>,
    /// Consumer side, taken exactly once by [`PipelineEngine::start_outbound_dispatcher`].
    outbound_receiver: Mutex<Option<mpsc::Receiver<OutboundMessage>>>,
    /// Adaptive circuit breakers maintaining health status per platform outbound queue.
    platform_circuit_breakers: Arc<RwLock<HashMap<String, Arc<CircuitBreaker>>>>,
    /// Shutdown progress observed by the worker, the dispatcher and every platform worker.
    shutdown: watch::Sender<ShutdownPhase>,
}

impl PipelineEngine {
    /// Creates a new `PipelineEngine` bound to a supervisor.
    ///
    /// The outbound delivery queue is created here so producers always have a valid sender, even
    /// when the dispatcher task has not been started yet.
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        let (outbound_sender, outbound_receiver) = mpsc::channel(DEFAULT_OUTBOUND_QUEUE_CAPACITY);
        Self {
            supervisor,
            agent: Arc::new(AgentSlot::new()),
            agent_factory: None,
            instances: None,
            toggles: None,
            reply_policy: None,
            context_policy: None,
            event_policy: None,
            command_policy: None,
            recalls: RecallLedger::default(),
            group_log: GroupLog::default(),
            triggers: TriggerMatcher::default(),
            captures: CaptureRegistry::default(),
            turns: Default::default(),
            simulation: Default::default(),
            mcp: None,
            observer: None,
            dead_letter: Arc::new(DeadLetterWriter::default()),
            outbound_sender,
            outbound_receiver: Mutex::new(Some(outbound_receiver)),
            platform_circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            shutdown: watch::Sender::new(ShutdownPhase::Running),
        }
    }

    /// Overrides the default dead letter writer.
    pub fn with_dead_letter(mut self, dead_letter: Arc<DeadLetterWriter>) -> Self {
        self.dead_letter = dead_letter;
        self
    }

    /// Returns a reference to the active dead letter writer.
    pub fn dead_letter(&self) -> &Arc<DeadLetterWriter> {
        &self.dead_letter
    }

    /// Shares the node's agent slot, enabling multi-turn reasoning and tool calling.
    ///
    /// Sharing the slot (instead of a router snapshot) is what allows a provider configured
    /// later through the control plane to start answering without a restart.
    pub fn with_agent_slot(mut self, agent: Arc<AgentSlot>) -> Self {
        self.agent = agent;
        self
    }

    /// Returns the shared agent slot backing this pipeline.
    pub fn agent_slot(&self) -> &Arc<AgentSlot> {
        &self.agent
    }

    /// Shares the factory used to build per-instance agents.
    pub fn with_agent_factory(mut self, factory: Arc<AgentFactory>) -> Self {
        self.agent = factory.slot().clone();
        self.agent_factory = Some(factory);
        self
    }

    /// The factory building per-instance agents, when one is attached.
    pub fn agent_factory(&self) -> Option<&Arc<AgentFactory>> {
        self.agent_factory.as_ref()
    }

    /// The model turns running now, which `/stop` reaches.
    pub(crate) fn running_turns(&self) -> &crate::pipeline::turns::RunningTurns {
        &self.turns
    }

    /// Shares the bot-instance catalog that gates and partitions inbound events.
    pub fn with_instances(mut self, instances: Arc<InstanceRegistry>) -> Self {
        self.instances = Some(instances);
        self
    }

    /// Returns the instance catalog backing this pipeline, when one is attached.
    pub fn instances(&self) -> Option<&Arc<InstanceRegistry>> {
        self.instances.as_ref()
    }

    /// Shares the global enable switches used for per-instance policy resolution.
    pub fn with_toggles(mut self, toggles: Arc<ToggleStore>) -> Self {
        self.toggles = Some(toggles);
        self
    }

    /// Shares the node-wide reply policy used when an instance does not override it.
    pub fn with_reply_policy(mut self, policy: Arc<ReplyPolicyStore>) -> Self {
        self.reply_policy = Some(policy);
        self
    }

    /// Shares the node-wide command permissions with the control plane.
    pub fn with_command_policy(mut self, policy: Arc<CommandPolicyStore>) -> Self {
        self.command_policy = Some(policy);
        self
    }

    /// Shares the node-wide notice policy (welcomes, pokes, recall notes) with the control plane.
    pub fn with_event_policy(mut self, policy: Arc<EventPolicyStore>) -> Self {
        self.event_policy = Some(policy);
        self
    }

    /// Shares the node-wide context-extras policy used when an instance does not override it.
    pub fn with_context_policy(mut self, policy: Arc<ContextPolicyStore>) -> Self {
        self.context_policy = Some(policy);
        self
    }

    /// Shares the MCP pool so its tools join plugin tools in the same router.
    pub fn with_mcp_pool(mut self, mcp: Arc<McpPool>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Attaches an LLM [`ToolRouter`] snapshot to enable multi-turn reasoning and tool calling.
    ///
    /// Kept for callers that already hold a router (mostly tests); runtime wiring should prefer
    /// [`PipelineEngine::with_agent_slot`] so provider changes are observed.
    pub fn with_tool_router(self, tool_router: Arc<ToolRouter>) -> Self {
        self.with_agent_slot(Arc::new(AgentSlot::with_agent(tool_router.agent_arc())))
    }

    /// Attaches a lifecycle observer for control-plane tracing.
    pub fn with_observer(mut self, observer: Arc<dyn PipelineObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Returns a handle for enqueuing outbound messages from outside the worker loop.
    pub fn outbound_sender(&self) -> mpsc::Sender<OutboundMessage> {
        self.outbound_sender.clone()
    }

    /// Retrieves or instantiates the adaptive circuit breaker for a platform's outbound queue.
    pub async fn platform_circuit_breaker(&self, platform: &str) -> Arc<CircuitBreaker> {
        let mut breakers = self.platform_circuit_breakers.write().await;
        breakers
            .entry(platform.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::for_platform()))
            .clone()
    }

    /// Evaluates the current operational state of a platform's circuit breaker.
    pub async fn platform_circuit_state(&self, platform: &str) -> CircuitState {
        let breakers = self.platform_circuit_breakers.read().await;
        match breakers.get(platform) {
            Some(cb) => cb.state(),
            None => CircuitState::Closed,
        }
    }

    /// Builds the console-facing adapter catalog: built-ins first, then plugin adapters,
    /// annotated with real-time outbound circuit breaker states.
    pub async fn adapter_catalog(&self) -> Vec<AdapterDescriptor> {
        let mut catalog = self.supervisor.adapter_catalog().await;
        for adapter in &mut catalog {
            adapter.circuit_state = self.platform_circuit_state(&adapter.platform).await;
        }
        catalog
    }

    /// Publishes a lifecycle stage to the attached observer, if any.
    ///
    /// Observation is deliberately infallible: tracing must never alter pipeline outcomes.
    fn observe(&self, stage: PipelineStage) {
        if let Some(ref observer) = self.observer {
            observer.on_stage(&stage);
        }
    }

    /// The hosts whose plugins `instance` runs, by its plugin policy and the global toggles.
    ///
    /// Global toggles apply even when there is no instance, as in embedded pipeline callers.
    async fn instance_hosts(
        &self,
        instance: Option<&crate::instance::BotInstance>,
        hosts: Vec<Arc<crate::supervisor::ManagedHost>>,
    ) -> Vec<Arc<crate::supervisor::ManagedHost>> {
        crate::supervisor::filter_plugin_hosts(hosts, self.toggles.as_deref(), instance).await
    }

    /// The tool sources a turn of `instance` may use: the hosts of its plugins whose circuit
    /// breaker is closed, and the MCP servers it allows.
    ///
    /// `hosts` are the instance's plugin hosts (see [`Self::instance_hosts`]); a host with an open
    /// breaker is skipped, and the skip observed, so a failing plugin does not stall the model.
    pub(crate) async fn tool_hosts(
        &self,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        instance: Option<&crate::instance::BotInstance>,
        event_id: &str,
    ) -> Vec<Arc<dyn kanon_llm::tool_router::ToolHost>> {
        // Plugin hosts and MCP servers are both tool sources; the router sees one slice.
        let mut active: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>> = Vec::new();
        for host in hosts {
            if host.circuit_breaker.is_available() {
                active.push(Arc::clone(host) as Arc<dyn kanon_llm::tool_router::ToolHost>);
            } else {
                tracing::warn!(
                    host_id = %host.host_id,
                    event_id = %event_id,
                    "Circuit breaker is OPEN; fast-skipping host from ToolRouter candidates"
                );
                self.observe(PipelineStage::CircuitBreakerTripped {
                    event_id: event_id.to_string(),
                    host_id: host.host_id.clone(),
                    phase: "tool_router".to_string(),
                    reason: format!(
                        "Circuit breaker OPEN (state: {:?}, consecutive failures: {})",
                        host.circuit_breaker.state(),
                        host.circuit_breaker.consecutive_failures()
                    ),
                });
            }
        }

        // MCP servers contribute their tools under the same policy rules as plugins.
        if let Some(mcp) = &self.mcp {
            active.extend(mcp.hosts_for_instance(instance).await);
        }
        active
    }

    /// Plugin hosts for a turn outside any instance: those the node-wide toggles enable.
    pub(crate) async fn enabled_hosts(&self) -> Vec<Arc<crate::supervisor::ManagedHost>> {
        let hosts = self.supervisor.get_all_hosts().await;
        self.instance_hosts(None, hosts).await
    }

    /// The plugin hosts `instance` runs, resolved now (see [`Self::instance_hosts`]).
    pub(crate) async fn hosts_of(
        &self,
        instance: &crate::instance::BotInstance,
    ) -> Vec<Arc<crate::supervisor::ManagedHost>> {
        let hosts = self.supervisor.get_all_hosts().await;
        self.instance_hosts(Some(instance), hosts).await
    }

    /// Runs one agent turn that answers `turn.event` in a conversation.
    ///
    /// The turn is announced to subscribers (`AGENT_BEGIN`, then `AGENT_DONE` however it ends),
    /// registered so `/stop` reaches it, and run inside [`crate::pipeline::agent_hook::with_turn`] so its
    /// tool calls carry the message as context and the instance's plugins take part in it. The
    /// caller owns the session: it builds the message and holds the session's write lock.
    pub(crate) async fn run_conversation_turn(
        &self,
        turn: ConversationTurn<'_>,
        message: kanon_llm::ChatMessage,
    ) -> Result<kanon_llm::ToolRouterOutput, kanon_llm::ToolRouterError> {
        let ConversationTurn {
            agent,
            running,
            session_id,
            event,
            hosts,
            tool_hosts,
            bash_caller,
            mut options,
        } = turn;
        hooks::emit_event(
            hosts,
            EventKind::AgentBegin,
            Detail::AgentBegin(AgentBeginEvent {
                context: Some(event.clone()),
                session_id: session_id.to_string(),
            }),
        );
        // Configuration failures finish the announced turn too; keep resolution inside the
        // captured result so every AGENT_BEGIN receives its matching AGENT_DONE.
        let result = async {
            let agent = match agent {
                kanon_llm::ConversationBackend::Builtin(agent) => agent,
                #[cfg(feature = "dsh")]
                kanon_llm::ConversationBackend::Dsh(client) => {
                    if options != kanon_llm::TurnOptions::default() {
                        return Err(kanon_llm::ToolRouterError::InvalidRequest(
                            "DSH owns turn settings; configure them in DSH".into(),
                        ));
                    }
                    return kanon_llm::dsh::run_message(
                        &client,
                        session_id,
                        &event.event_id,
                        message,
                        &running.signal(),
                    )
                    .await;
                }
            };
            if let Some(instance_id) =
                crate::instance::BotInstance::instance_id_from_session(session_id)
                && let Some(instances) = self.instances()
            {
                // Resolve after waiting for this session's writer. The routing snapshot may
                // predate an edit; the current catalog owns inheritance, never session metadata.
                let personas = self
                    .agent_factory()
                    .map(|factory| factory.personas())
                    .or_else(|| agent.persona_registry());
                if instances
                    .get(instance_id)
                    .await
                    .is_some_and(|instance| instance.conversation_rules)
                {
                    let instructions = options.instructions.get_or_insert_with(String::new);
                    if !instructions.is_empty() {
                        instructions.push_str("\n\n");
                    }
                    instructions.push_str(crate::simulation::CONVERSATION_RULES);
                }
                options.persona = instances
                    .persona_for_instance(instance_id, personas.map(Arc::as_ref))
                    .await
                    .map_err(|error| {
                        kanon_llm::ToolRouterError::InvalidRequest(error.to_string())
                    })?;
            }
            let router = ToolRouter::from_arc(agent);
            crate::pipeline::agent_hook::with_turn(
                event.clone(),
                hosts.to_vec(),
                crate::with_bash_caller(
                    bash_caller,
                    kanon_llm::with_stop_signal(
                        running.signal(),
                        router.execute_message_with(session_id, message, &tool_hosts, options),
                    ),
                ),
            )
            .await
        }
        .await;

        let done = match &result {
            Ok(output) => AgentDoneEvent {
                context: Some(event.clone()),
                session_id: session_id.to_string(),
                success: true,
                content: visible_reply(&output.content),
                error: String::new(),
                tools: tool_names(&output.executed_tools),
            },
            Err(err) => AgentDoneEvent {
                context: Some(event.clone()),
                session_id: session_id.to_string(),
                success: false,
                content: String::new(),
                error: failure_category(err).to_string(),
                tools: Vec::new(),
            },
        };
        hooks::emit_event(hosts, EventKind::AgentDone, Detail::AgentDone(done));
        result
    }
}
