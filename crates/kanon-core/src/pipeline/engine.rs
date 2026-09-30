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

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{
    AgentFactory, AgentSlot, ModelCapabilities, ModelRef, ModelSpec, strip_reasoning_tags,
};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest, MessageSegment,
    PipelineEventRequest,
};

use crate::adapter::{AdapterDescriptor, AdapterError, AdapterKind};
use crate::conversation::{ContextPolicyStore, ConversationKind, ReplyPolicyStore, bot_mentioned};
use crate::instance::InstanceRegistry;
use crate::mcp::McpPool;
use crate::notice::{
    EventPolicyStore, META_NOTICE_ACTOR, META_NOTICE_TARGET, NoticeKind, RecallLedger, metadata_str,
};
use crate::pipeline::command::CommandRouter;
use crate::pipeline::context::build_user_message;
use crate::pipeline::dead_letter::DeadLetterWriter;
use crate::pipeline::observer::{PipelineObserver, PipelineStage};
use crate::pipeline::pre_filter::{PreFilterChain, PreFilterOutcome};
use crate::supervisor::circuit_breaker::{CircuitBreaker, CircuitState};
use crate::supervisor::{AdapterRoute, Supervisor};
use crate::toggle::{PLUGIN_SECTION, ToggleStore};

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

/// How long the event being processed when shutdown starts may take to finish.
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
    /// The ingest queue is closed; the worker records queued events and finishes the current one.
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

/// One queued delivery, optionally carrying a single-use completion receipt.
///
/// Keeping the receipt with the request preserves FIFO ordering and makes queue
/// drops explicit without a second registry of messages to reconcile.
#[derive(Debug)]
pub struct OutboundMessage {
    /// Original platform event and reply segments, unchanged by queueing.
    pub request: DeliverMessageRequest,
    /// Resolved only after platform delivery or an explicit dispatch failure.
    pub receipt: Option<oneshot::Sender<DeliverMessageResponse>>,
}

impl From<DeliverMessageRequest> for OutboundMessage {
    fn from(request: DeliverMessageRequest) -> Self {
        Self {
            request,
            receipt: None,
        }
    }
}

/// Result produced after processing an event through the pipeline engine.
#[derive(Debug, Clone)]
pub enum PipelineResult {
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

/// Name of the built-in session command, handled by the core and never by the model.
pub const NEW_SESSION_COMMAND: &str = "new";

/// Name of the built-in model command, handled by the core and never by the model.
///
/// `/model` lists the models the node knows about; `/model <index>` switches the model of the
/// instance the command was issued to. It is resolved before plugin commands so a plugin can never
/// shadow it, and long before the LLM, which must never see it as conversation text.
pub const MODEL_COMMAND: &str = "model";

/// Name of the built-in help command.
pub const HELP_COMMAND: &str = "help";

/// Name of the built-in system-info command.
pub const INFO_COMMAND: &str = "info";

/// Identity of one conversation inside an instance: channel plus sender.
///
/// Kept as a free function because both the instance gate and the LLM phase must derive exactly
/// the same key — the `/new` command and the messages that follow it have to agree.
fn conversation_key(event: &PipelineEventRequest) -> String {
    if event.channel_id.trim().is_empty() {
        if event.sender_id.trim().is_empty() {
            "default".to_string()
        } else {
            event.sender_id.clone()
        }
    } else if event.sender_id.trim().is_empty() {
        event.channel_id.clone()
    } else {
        format!("{}:{}", event.channel_id, event.sender_id)
    }
}

/// Result produced after delivering an outbound message through a platform adapter.
#[derive(Debug, Clone)]
pub struct DeliveryOutcome {
    /// Which adapter route served the message.
    pub kind: AdapterKind,
    /// Platform that accepted the message.
    pub platform: String,
    /// Platform-assigned message identifier, when reported.
    pub message_id: String,
}

/// Kernel release from macOS system APIs or Linux procfs, when readable.
fn kernel_release() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        sysinfo::System::kernel_version().or_else(|| {
            tracing::warn!("could not read macOS kernel version");
            None
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|release| release.trim().to_string())
            .filter(|release| !release.is_empty())
    }
}

/// Human-readable name of the running system.
///
/// "linux" says almost nothing to a user; the distribution (`Ubuntu 24.04.1 LTS`) identifies the
/// host far better. Linux exposes it through `os-release`; macOS exposes its product version
/// through native system APIs. Use the numeric macOS version instead of a release-name lookup
/// so newly released systems remain identifiable without updating a codename table.
fn distribution_name() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
            for key in ["PRETTY_NAME", "NAME"] {
                let prefix = format!("{key}=");
                if let Some(value) = release
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(&prefix))
                {
                    let value = value.trim().trim_matches('"');
                    if !value.is_empty() {
                        return value.to_string();
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(version) = sysinfo::System::os_version() {
            let version = version.trim();
            if !version.is_empty() {
                return format!("macOS {version}");
            }
        }
        tracing::warn!("could not read macOS product version");
    }

    std::env::consts::OS.to_string()
}

/// Removes leading `@mention` tokens from a message before slash-command parsing.
///
/// A group platform renders a mention as leading text, so a command typed at the bot arrives as
/// `@bot /model`. The core only uses this for *command* parsing: the model still receives the
/// mention, because knowing it was addressed is real context.
fn strip_leading_mentions(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(after_at) = rest.strip_prefix('@') {
        match after_at.find(char::is_whitespace) {
            Some(index) => rest = after_at[index..].trim_start(),
            // A mention with no following text leaves nothing that could be a command.
            None => return "",
        }
    }
    rest
}

/// Builds an outbound text reply segment.
fn text_reply(content: impl Into<String>) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
            content: content.into(),
        })),
    }
}

/// Renders the `/model` listing as plain text.
fn render_model_list(options: &[(String, Option<ModelSpec>)], current: Option<&str>) -> String {
    if options.is_empty() {
        return "当前没有可用模型；请先在控制台配置提供商和模型。".to_string();
    }

    let mut rendered = String::from("可用模型：\n");
    for (index, (reference, spec)) in options.iter().enumerate() {
        rendered.push_str(&format!("{}. {reference}", index + 1));
        if let Some(context_length) = spec.as_ref().and_then(|spec| spec.context_length) {
            rendered.push_str(&format!(" (上下文 {context_length})"));
        }
        if current == Some(reference.as_str()) {
            rendered.push_str(" ✓ 当前");
        }
        rendered.push('\n');
    }
    rendered.push_str("回复 /model <序号> 切换当前实例模型。");
    rendered
}

/// Draws the sample used by the probability reply mode.
///
/// Derived from the event id so the same event always yields the same decision (a retry cannot
/// flip a drop into a reply), while distinct events are independent. `RandomState` is randomly
/// seeded per process, so the sequence is not predictable from the outside.
fn reply_sample(event_id: &str) -> f32 {
    use std::hash::{BuildHasher, Hash, Hasher};

    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    event_id.hash(&mut hasher);
    (hasher.finish() % 10_000) as f32 / 10_000.0
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
    /// Messages the model answered and recall notes awaiting their conversation's next turn.
    recalls: RecallLedger,
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
            recalls: RecallLedger::default(),
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
        let mut catalog: Vec<AdapterDescriptor> = Vec::new();
        for adapter in self.supervisor.adapters().list().await {
            let circuit_state = self.platform_circuit_state(adapter.platform()).await;
            catalog.push(AdapterDescriptor {
                platform: adapter.platform().to_string(),
                display_name: adapter.display_name().to_string(),
                kind: AdapterKind::Builtin,
                connected: adapter.is_connected(),
                circuit_state,
                plugin_id: None,
                host_id: None,
            });
        }

        for host in self.supervisor.get_all_hosts().await {
            let platforms = host.adapter_platforms();
            if platforms.is_empty() {
                continue;
            }

            let display_name = host
                .adapter_display_name()
                .unwrap_or_else(|| host.host_id.clone());
            let plugin_id = host.adapter_plugin_id();

            for platform in platforms {
                let circuit_state = self.platform_circuit_state(&platform).await;
                catalog.push(AdapterDescriptor {
                    platform,
                    display_name: display_name.clone(),
                    kind: AdapterKind::Plugin,
                    connected: true,
                    circuit_state,
                    plugin_id: plugin_id.clone(),
                    host_id: Some(host.host_id.clone()),
                });
            }
        }

        catalog.sort_by(|a, b| a.platform.cmp(&b.platform));
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

    /// Delivers one outbound message to the adapter owning its platform.
    ///
    /// Routing order is built-in adapter first, then plugin host. Every failure path is explicit:
    /// an unrouted platform, an unreachable plugin host, or a plugin that reports `success=false`
    /// all become [`AdapterError`] values instead of a silent drop.
    pub async fn deliver_outbound(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliveryOutcome, AdapterError> {
        let platform = request.platform.clone();

        match self.supervisor.resolve_adapter(&platform).await {
            Some(AdapterRoute::Builtin(adapter)) => {
                let response = adapter.deliver(request).await?;
                if !response.success {
                    return Err(AdapterError::Delivery {
                        platform,
                        reason: response.error_message,
                    });
                }
                Ok(DeliveryOutcome {
                    kind: AdapterKind::Builtin,
                    platform,
                    message_id: response.message_id,
                })
            }
            Some(AdapterRoute::Plugin { host, plugin_id }) => {
                let host_id = host.host_id.clone();
                let response = host.deliver_message(request).await.map_err(|status| {
                    AdapterError::Delivery {
                        platform: platform.clone(),
                        reason: format!(
                            "plugin '{plugin_id}' on host '{host_id}' failed: {status}"
                        ),
                    }
                })?;

                if !response.success {
                    return Err(AdapterError::Delivery {
                        platform,
                        reason: format!(
                            "plugin '{plugin_id}' on host '{host_id}' rejected the message: {}",
                            response.error_message
                        ),
                    });
                }

                Ok(DeliveryOutcome {
                    kind: AdapterKind::Plugin,
                    platform,
                    message_id: response.message_id,
                })
            }
            None => Err(AdapterError::UnknownPlatform(platform)),
        }
    }

    /// Delivers a single outbound message and publishes the resulting observation stage,
    /// protected by the platform's adaptive circuit breaker and cold-storage dead-letter queue.
    pub async fn dispatch_outbound_request(&self, request: DeliverMessageRequest) {
        let breaker = self.platform_circuit_breaker(&request.platform).await;
        self.dispatch_outbound_request_with_breaker(request, &breaker)
            .await;
    }

    /// Delivers a single outbound message using the given platform circuit breaker.
    ///
    /// # Fault Tolerance & Dead-Letter Persistence
    /// - If the breaker is `Open`, fast-skips the call, publishes [`PipelineStage::OutboundFailed`],
    ///   and appends the dropped message to `data/dead_letter/<platform>_<date>.jsonl`.
    /// - If delivery succeeds, records latency in the breaker and publishes [`PipelineStage::OutboundDelivered`].
    /// - If delivery fails, increments consecutive failures in the breaker (tripping if threshold reached),
    ///   persists the message to the dead-letter log, and publishes [`PipelineStage::OutboundFailed`].
    pub async fn dispatch_outbound_request_with_breaker(
        &self,
        request: DeliverMessageRequest,
        breaker: &CircuitBreaker,
    ) -> DeliverMessageResponse {
        let platform = request.platform.clone();
        let channel_id = request.channel_id.clone();
        let segment_count = request.segments.len();

        if !breaker.allow_request() {
            let reason = "platform circuit breaker open".to_string();
            tracing::warn!(
                platform = %platform,
                channel_id = %channel_id,
                "Platform circuit breaker tripped (OPEN); short-circuiting delivery and persisting to dead letter"
            );
            if let Err(err) = self.dead_letter.write_record(&request, &reason).await {
                tracing::error!(
                    platform = %platform,
                    error = %err,
                    "Failed to record dead-letter entry for short-circuited message"
                );
            }
            self.observe(PipelineStage::OutboundFailed {
                platform,
                channel_id,
                reason: "circuit breaker open, persisted to dead letter".to_string(),
            });
            return DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: reason,
            };
        }

        let start = std::time::Instant::now();
        match self.deliver_outbound(request.clone()).await {
            Ok(outcome) => {
                let response = DeliverMessageResponse {
                    success: true,
                    message_id: outcome.message_id.clone(),
                    error_message: String::new(),
                };
                breaker.record_success(start.elapsed());
                tracing::info!(
                    platform = %outcome.platform,
                    channel_id = %channel_id,
                    message_id = %outcome.message_id,
                    "Outbound message delivered"
                );
                self.observe(PipelineStage::OutboundDelivered {
                    platform: outcome.platform,
                    channel_id,
                    segment_count,
                    target: outcome.kind,
                    message_id: outcome.message_id,
                });
                response
            }
            Err(err) => {
                let reason = err.to_string();
                breaker.record_failure(&reason);
                tracing::warn!(
                    platform = %platform,
                    channel_id = %channel_id,
                    error = %err,
                    "Outbound delivery failed; persisting to dead letter"
                );
                if let Err(write_err) = self.dead_letter.write_record(&request, &reason).await {
                    tracing::error!(
                        platform = %platform,
                        error = %write_err,
                        "Failed to persist dead letter record"
                    );
                }
                self.observe(PipelineStage::OutboundFailed {
                    platform,
                    channel_id,
                    reason: reason.clone(),
                });
                DeliverMessageResponse {
                    success: false,
                    message_id: String::new(),
                    error_message: reason,
                }
            }
        }
    }

    /// Spawns a dedicated sequential worker task for a single platform partition.
    fn spawn_platform_worker(
        self: &Arc<Self>,
        platform: String,
        mut rx: mpsc::Receiver<OutboundMessage>,
    ) -> JoinHandle<()> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            tracing::debug!(platform = %platform, "Platform outbound worker spawned");
            let breaker = engine.platform_circuit_breaker(&platform).await;
            let mut phase = engine.shutdown.subscribe();
            loop {
                // Workers retire after 30 seconds of inactivity to reclaim resources.
                match tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await {
                    Ok(Some(message)) => {
                        // Do not start a queued reply after its caller has gone away.
                        // Already-started platform I/O is never retried on cancellation.
                        if message
                            .receipt
                            .as_ref()
                            .is_some_and(|receipt| receipt.is_closed())
                        {
                            continue;
                        }
                        // During shutdown a delivery may run only until the drain deadline. The
                        // deadline branch is polled first so a reply is never started after it.
                        let request = message.request;
                        let response = tokio::select! {
                            biased;
                            () = delivery_deadline_passed(&mut phase) => {
                                engine
                                    .dead_letter_reply(
                                        &request,
                                        "node shut down before the reply was delivered; the platform may or may not have received it",
                                    )
                                    .await
                            }
                            response = engine
                                .dispatch_outbound_request_with_breaker(request.clone(), &breaker) => response,
                        };
                        if let Some(receipt) = message.receipt {
                            let _ = receipt.send(response);
                        }
                    }
                    Ok(None) => {
                        // All channel senders dropped (shutting down).
                        break;
                    }
                    Err(_) => {
                        // Idle timeout reached; retire this worker.
                        tracing::debug!(platform = %platform, "Platform outbound worker idle timeout; retiring");
                        break;
                    }
                }
            }
        })
    }

    /// Records a reply that will not be delivered and reports the failure to its caller.
    async fn dead_letter_reply(
        &self,
        request: &DeliverMessageRequest,
        reason: &str,
    ) -> DeliverMessageResponse {
        if let Err(err) = self.dead_letter.write_record(request, reason).await {
            tracing::error!(
                platform = %request.platform,
                channel_id = %request.channel_id,
                error = %err,
                "Failed to persist dead letter record; the reply is lost"
            );
        }
        self.observe(PipelineStage::OutboundFailed {
            platform: request.platform.clone(),
            channel_id: request.channel_id.clone(),
            reason: reason.to_string(),
        });
        DeliverMessageResponse {
            success: false,
            message_id: String::new(),
            error_message: reason.to_string(),
        }
    }

    /// Records an inbound event the node acknowledged but will never process.
    async fn dead_letter_event(&self, event: &PipelineEventRequest, reason: &str) {
        if let Err(err) = self.dead_letter.write_inbound(event, reason).await {
            tracing::error!(
                platform = %event.platform,
                event_id = %event.event_id,
                error = %err,
                "Failed to persist dead letter record; the event is lost"
            );
        }
    }

    /// Reports an outbound drop when a specific platform's worker queue is saturated,
    /// and durably flushes the dropped message to the dead-letter queue log before returning.
    async fn report_outbound_queue_full(&self, message: OutboundMessage) {
        let dropped = message.request;
        if let Some(receipt) = message.receipt {
            let _ = receipt.send(DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: "platform outbound queue full".to_string(),
            });
        }
        let platform = dropped.platform.clone();
        let channel_id = dropped.channel_id.clone();
        tracing::warn!(
            platform = %platform,
            channel_id = %channel_id,
            "Platform outbound queue saturated; persisting message to dead letter to prevent cross-platform backpressure"
        );
        if let Err(err) = self
            .dead_letter
            .write_record(&dropped, "platform outbound queue saturated")
            .await
        {
            tracing::error!(
                platform = %platform,
                channel_id = %channel_id,
                error = %err,
                "Failed to persist dead letter record for saturated outbound queue"
            );
        }
        self.observe(PipelineStage::OutboundFailed {
            platform,
            channel_id,
            reason: "platform outbound queue full".to_string(),
        });
    }

    /// Drains the global outbound queue and partitions requests into per-platform sequential workers.
    ///
    /// # Concurrency & Ordering Guarantee
    /// - **Per-platform FIFO ordering**: Each platform has an independent sequential worker. Messages
    ///   for platform `A` are processed strictly in arrival order.
    /// - **Cross-platform isolation**: Platform `A` being slow, stalled, or failing never blocks
    ///   outbound deliveries to platform `B`.
    /// - **Bounded queue protection**: If platform `A`'s queue reaches capacity, overflow messages
    ///   are dropped and emit [`PipelineStage::OutboundFailed`], without stalling the global dispatcher.
    ///
    /// # Shutdown
    /// Once [`PipelineEngine::drain`] reaches the outbound phase, the queue is closed, the replies
    /// still in it are handed to their platform workers, and the dispatcher waits for every worker
    /// to finish. Workers deliver until [`SHUTDOWN_DELIVERY_GRACE`] runs out and record the rest.
    pub async fn run_outbound_loop(self: Arc<Self>, mut receiver: mpsc::Receiver<OutboundMessage>) {
        tracing::info!("Partitioned outbound adapter dispatcher started");
        let mut workers: HashMap<String, (mpsc::Sender<OutboundMessage>, JoinHandle<()>)> =
            HashMap::new();
        let mut phase = self.shutdown.subscribe();

        loop {
            let request = tokio::select! {
                biased;
                _ = wait_for_phase(&mut phase, |p| matches!(p, ShutdownPhase::DrainingOutbound(_))) => break,
                request = receiver.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            self.route_outbound(request, &mut workers).await;
        }

        // Nothing new is accepted from here on; producers get an explicit `Closed` error.
        receiver.close();
        while let Some(request) = receiver.recv().await {
            self.route_outbound(request, &mut workers).await;
        }
        // Dropping each sender lets its worker finish the queue and stop; the delivery deadline
        // bounds how long that takes.
        for (platform, (sender, worker)) in workers {
            drop(sender);
            if let Err(err) = worker.await {
                tracing::error!(platform = %platform, error = %err, "Platform outbound worker failed during shutdown");
            }
        }
        tracing::info!("Partitioned outbound adapter dispatcher terminated");
    }

    /// Hands one reply to its platform's sequential worker, starting the worker if needed.
    async fn route_outbound(
        self: &Arc<Self>,
        request: OutboundMessage,
        workers: &mut HashMap<String, (mpsc::Sender<OutboundMessage>, JoinHandle<()>)>,
    ) {
        let platform = request.request.platform.clone();

        // Periodic cleanup of dead channels to avoid memory buildup when platforms are dynamic
        if workers.len() > 128 {
            workers.retain(|_, (tx, _)| !tx.is_closed());
        }

        let tx = match workers.get(&platform) {
            Some((tx, _)) if !tx.is_closed() => tx.clone(),
            _ => {
                let (new_tx, rx) = mpsc::channel(DEFAULT_PLATFORM_QUEUE_CAPACITY);
                let worker = self.spawn_platform_worker(platform.clone(), rx);
                workers.insert(platform.clone(), (new_tx.clone(), worker));
                new_tx
            }
        };

        match tx.try_send(request) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(req)) => {
                // The worker timed out immediately before the send; respawn and retry once
                let (new_tx, rx) = mpsc::channel(DEFAULT_PLATFORM_QUEUE_CAPACITY);
                let worker = self.spawn_platform_worker(platform.clone(), rx);
                workers.insert(platform, (new_tx.clone(), worker));
                if let Err(mpsc::error::TrySendError::Full(dropped)) = new_tx.try_send(req) {
                    self.report_outbound_queue_full(dropped).await;
                }
            }
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                self.report_outbound_queue_full(dropped).await;
            }
        }
    }

    /// Spawns the outbound dispatcher task.
    ///
    /// Returns `None` when the dispatcher was already started: the queue has exactly one consumer,
    /// so a second call cannot be honoured and must not silently steal messages.
    pub fn start_outbound_dispatcher(self: Arc<Self>) -> Option<JoinHandle<()>> {
        let receiver = self
            .outbound_receiver
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());

        let receiver = match receiver {
            Some(receiver) => receiver,
            None => {
                tracing::error!("Outbound dispatcher already running; ignoring duplicate start");
                return None;
            }
        };

        Some(tokio::spawn(async move {
            self.run_outbound_loop(receiver).await;
        }))
    }

    /// Processes a single inbound event through the PreFilter chain and command dispatcher.
    pub async fn process_event(&self, event: PipelineEventRequest) -> PipelineResult {
        // Capture identity before plugin pre-filters can rewrite the model-visible event.
        let bash_caller = crate::BashPrincipal {
            platform: event.platform.clone(),
            user_id: event.sender_id.clone(),
        };
        let event_id = event.event_id.clone();
        let platform = event.platform.clone();
        let hosts = self.supervisor.get_all_hosts().await;

        // Phase 0: Instance gate.
        //
        // Adapters only declare *where* messages come from; an instance decides whether a bot is
        // running there at all. With no enabled instance claiming the platform there is nothing
        // to answer as, so the event is dropped here — before pre-filters, commands and the LLM.
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&platform).await {
                Ok(Some(instance)) => Some(instance),
                Ok(None) => {
                    tracing::warn!(
                        platform = %platform,
                        event_id = %event_id,
                        "No enabled bot instance claims this platform; dropping inbound event"
                    );
                    return PipelineResult::NoInstance { platform };
                }
                Err(err) => {
                    // Ambiguous ownership must never be resolved by guessing.
                    tracing::error!(
                        platform = %platform,
                        event_id = %event_id,
                        error = %err,
                        "Instance routing is ambiguous; dropping inbound event"
                    );
                    return PipelineResult::NoInstance { platform };
                }
            },
            None => None,
        };

        // Phase 0a: Notices.
        //
        // A join, poke or recall is not a message. Only the node's event policy decides whether the
        // bot reacts at all; a notice it reacts to then skips commands and the reply policy (the
        // operator explicitly asked for the reaction) and reaches the model as a one-line event.
        let notice = NoticeKind::from_metadata(event.metadata.as_ref());
        if let Some(kind) = notice {
            let policy = self
                .event_policy
                .as_ref()
                .map(|store| store.get())
                .unwrap_or_default();
            let outcome = if kind == NoticeKind::Recall {
                let target = metadata_str(event.metadata.as_ref(), META_NOTICE_TARGET);
                let actor = metadata_str(event.metadata.as_ref(), META_NOTICE_ACTOR);
                match target {
                    Some(target) if policy.note_recalls => {
                        if self.recalls.note_recall(target, actor) {
                            "noted for the conversation's next turn".to_string()
                        } else {
                            "the model never saw the recalled message".to_string()
                        }
                    }
                    Some(_) => "recall notes are disabled by the event policy".to_string(),
                    None => "the recall names no message".to_string(),
                }
            } else if !policy.answers(kind) {
                "disabled by the event policy".to_string()
            } else {
                String::new()
            };
            if !outcome.is_empty() {
                return PipelineResult::Notice {
                    kind: kind.as_str(),
                    outcome,
                };
            }
        }

        // Phase 0b: Per-instance plugin policy.
        //
        // Filtering here (rather than inside each later phase) means a plugin this instance
        // disabled cannot pre-filter, answer commands or offer tools — one decision covers the
        // whole pipeline. Without a toggle store or an instance the node behaves as before.
        let hosts: Vec<Arc<crate::supervisor::ManagedHost>> =
            match (&self.toggles, instance.as_ref()) {
                (Some(toggles), Some(instance)) => {
                    let mut allowed = Vec::with_capacity(hosts.len());
                    for host in hosts {
                        let plugin_id = host.primary_plugin_id().unwrap_or_default();
                        let globally_enabled = toggles.is_enabled(PLUGIN_SECTION, &plugin_id).await;
                        if instance.allows_plugin(&plugin_id, globally_enabled) {
                            allowed.push(host);
                        } else {
                            tracing::debug!(
                                instance_id = %instance.id,
                                plugin_id = %plugin_id,
                                "Plugin skipped for this instance by its plugin policy"
                            );
                        }
                    }
                    allowed
                }
                _ => hosts,
            };

        // Phase 1: PreFilter Interception Chain
        self.observe(PipelineStage::PreFilterStarted {
            event_id: event_id.clone(),
            host_count: hosts.len(),
        });
        let filtered_event = match PreFilterChain::execute_with_observer(
            event,
            &hosts,
            self.observer.as_ref(),
        )
        .await
        {
            PreFilterOutcome::Blocked {
                host_id,
                reply_messages,
            } => {
                tracing::info!(
                    host_id = %host_id,
                    "Event blocked by PreFilter chain; halting pipeline execution"
                );
                self.observe(PipelineStage::PreFilterBlocked {
                    event_id,
                    host_id: host_id.clone(),
                });
                return PipelineResult::Blocked {
                    host_id,
                    replies: reply_messages,
                };
            }
            PreFilterOutcome::Passed(evt) => evt,
        };
        self.observe(PipelineStage::PreFilterPassed {
            event_id: event_id.clone(),
        });

        // Phase 2: Command Router matching
        // Extract raw text or inspect primary text segment if raw_text is empty.
        let text_candidate = if !filtered_event.raw_text.is_empty() {
            filtered_event.raw_text.clone()
        } else {
            filtered_event
                .segments
                .iter()
                .find_map(|s| match &s.segment {
                    Some(Segment::Text(t)) => Some(t.content.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        };

        // Phase 2a: Built-in commands, resolved by the core itself.
        //
        // `/new` rotates the session of the conversation that issued it and `/model` lists or
        // switches the instance's model; `/help` and `/info` report the command catalog and the
        // node's runtime facts. All are matched before plugin commands (so a plugin can never
        // shadow them) and long before the LLM, which must never receive them as conversation text.
        //
        // Group platforms render a mention as leading text (`@bot /model`), so mentions are
        // stripped before parsing; otherwise a command typed in a group would never be recognised.
        // A notice carries no user text, so it can never be a command.
        let command_text = if notice.is_some() {
            ""
        } else {
            strip_leading_mentions(&text_candidate)
        };
        if let Some((cmd_name, args)) = CommandRouter::parse_command(command_text) {
            if cmd_name.eq_ignore_ascii_case(NEW_SESSION_COMMAND)
                && let Some(instance) = instance.as_ref()
            {
                return self.handle_new_session(&filtered_event, instance).await;
            }
            if cmd_name.eq_ignore_ascii_case(MODEL_COMMAND)
                && let Some(instance) = instance.as_ref()
            {
                return self.handle_model_command(instance, &args).await;
            }
            if cmd_name.eq_ignore_ascii_case(HELP_COMMAND) {
                return self.handle_help_command(&hosts);
            }
            if cmd_name.eq_ignore_ascii_case(INFO_COMMAND) {
                return self.handle_info_command(instance.as_ref(), &filtered_event);
            }
        }

        if let Some((cmd_name, args)) = CommandRouter::parse_command(command_text) {
            if let Some(target) = CommandRouter::resolve(&cmd_name, &hosts) {
                tracing::debug!(
                    command = %cmd_name,
                    plugin_id = %target.plugin_id,
                    host_id = %target.host.host_id,
                    "Routing slash command to target host"
                );

                self.observe(PipelineStage::CommandMatched {
                    event_id: event_id.clone(),
                    command: cmd_name.clone(),
                    plugin_id: target.plugin_id.clone(),
                    host_id: target.host.host_id.clone(),
                });

                match CommandRouter::dispatch(&target, args, filtered_event.clone()).await {
                    Ok(response) => {
                        return PipelineResult::CommandExecuted {
                            command: cmd_name,
                            plugin_id: target.plugin_id,
                            host_id: target.host.host_id.clone(),
                            success: response.success,
                            replies: response.replies,
                        };
                    }
                    Err(status) => {
                        tracing::error!(
                            command = %cmd_name,
                            host_id = %target.host.host_id,
                            error = %status,
                            "Command execution failed with gRPC status"
                        );
                        return PipelineResult::CommandExecuted {
                            command: cmd_name,
                            plugin_id: target.plugin_id,
                            host_id: target.host.host_id.clone(),
                            success: false,
                            replies: vec![],
                        };
                    }
                }
            } else {
                tracing::debug!(
                    command = %cmd_name,
                    "Slash command detected but no matching plugin was registered"
                );
                self.observe(PipelineStage::CommandNotFound {
                    event_id,
                    command: cmd_name.clone(),
                });
                return PipelineResult::CommandNotFound { command: cmd_name };
            }
        }

        // Phase 2c: Reply policy gate.
        //
        // Evaluated after commands and before the model: built-in and plugin slash commands always
        // answer, while an unaddressed group message never reaches the LLM when the instance asked
        // to be mentioned first. Pre-filters have already run, so a plugin still observes every
        // inbound event — this gate only decides whether the *bot* answers.
        let kind = ConversationKind::from_metadata(filtered_event.metadata.as_ref());
        let node_reply_policy = self
            .reply_policy
            .as_ref()
            .map(|store| store.get())
            .unwrap_or_default();
        let reply_policy = instance.as_ref().map_or(node_reply_policy, |instance| {
            instance.effective_reply_policy(node_reply_policy)
        });
        // Quoting only makes sense for a message in a shared conversation; a notice has none.
        let quote_reply =
            reply_policy.quote_message && kind.is_policy_governed() && notice.is_none();
        if let Some(instance) = instance.as_ref()
            && notice.is_none()
        {
            let mentioned = bot_mentioned(filtered_event.metadata.as_ref());
            let policy = reply_policy;
            let sample = reply_sample(&filtered_event.event_id);
            if !policy.should_reply(kind, mentioned, sample) {
                let reason = format!(
                    "reply policy '{}' suppressed a {} conversation (mentioned={mentioned})",
                    policy.describe(),
                    kind.as_str()
                );
                tracing::info!(
                    instance_id = %instance.id,
                    event_id = %filtered_event.event_id,
                    reason = %reason,
                    "Reply suppressed by the instance reply policy"
                );
                return PipelineResult::ReplySuppressed {
                    instance_id: instance.id.clone(),
                    reason,
                };
            }
        }

        // Phase 3: Conversational message (unmatched by command router, routed to LLM if enabled)
        // The agent is resolved per event so a provider configured or cleared at runtime is
        // honoured immediately; an empty slot means "no conversational LLM" and passes through.
        let conversation = conversation_key(&filtered_event);

        // The agent is resolved per event: the node provider comes from the shared slot (so a
        // provider configured at runtime is honoured immediately) and a per-instance model
        // override comes from the factory, which shares memory, personas and hooks with it.
        let resolved_agent = match &self.agent_factory {
            Some(factory) => {
                factory.agent_for_model(instance.as_ref().and_then(|i| i.model.as_deref()))
            }
            None => self.agent.current(),
        };

        // The target model's catalog entry decides which modalities may be attached: an image only
        // when it accepts images, text only when it accepts text.
        let capabilities = match (self.agent_factory.as_ref(), resolved_agent.as_ref()) {
            (Some(factory), Some(agent)) => {
                factory
                    .models()
                    .settings_for(&ModelRef::parse(&agent.config().model_ref()))
                    .capabilities
            }
            _ => ModelCapabilities::default(),
        };
        // The instance may override whether the sender id and the message time are prepended.
        let context_policy = instance.as_ref().map_or_else(
            || {
                self.context_policy
                    .as_ref()
                    .map(|store| store.get())
                    .unwrap_or_default()
            },
            |instance| {
                instance.effective_context_policy(
                    self.context_policy
                        .as_ref()
                        .map(|store| store.get())
                        .unwrap_or_default(),
                )
            },
        );
        // Recall notes waiting for this conversation ride on its next model turn, as leading text
        // of the current user message: runtime facts belong to the current turn, never to the
        // cached prefix. They are only taken when a model will actually read them.
        let ledger_key = format!("{platform}\u{1f}{conversation}");
        let mut filtered_event = filtered_event;
        if resolved_agent.is_some() {
            let notes = self.recalls.take_notes(&ledger_key);
            // A text-only event is rendered from `raw_text` only when it has no segments; once a
            // note becomes a segment, the text must be one too or it would vanish.
            if !notes.is_empty()
                && filtered_event.segments.is_empty()
                && !filtered_event.raw_text.trim().is_empty()
            {
                let text = filtered_event.raw_text.clone();
                filtered_event.segments.push(text_reply(text));
            }
            for (index, note) in notes.into_iter().enumerate() {
                filtered_event.segments.insert(index, text_reply(note));
            }
        }
        let user_message = build_user_message(&filtered_event, &capabilities, &context_policy);

        if (user_message
            .content
            .as_deref()
            .is_some_and(|content| !content.is_empty())
            || user_message.has_parts())
            && let Some(agent) = resolved_agent
        {
            let router = ToolRouter::from_arc(agent.clone());

            // Remember what the model is shown, so a later recall of it can be noted.
            if notice.is_none() {
                self.recalls.record_turn(
                    &filtered_event.event_id,
                    &ledger_key,
                    &filtered_event.raw_text,
                );
            }

            // Let a built-in adapter show that an answer is coming (typing, a reaction). Spawned:
            // platform I/O must never delay the model call.
            if let Some(adapter) = self.supervisor.adapters().get(&platform).await {
                let event = filtered_event.clone();
                tokio::spawn(async move {
                    if let Err(err) = adapter.acknowledge(&event).await {
                        tracing::warn!(error = %err, "Adapter failed to acknowledge an event");
                    }
                });
            }

            // Sessions are namespaced by the instance that owns the conversation, so two bots can
            // never share context. An unpartitioned pipeline keeps the legacy conversation key.
            let session_id = match instance.as_ref() {
                Some(instance) => instance.conversation_session_id(&conversation),
                None => conversation.clone(),
            };

            // The instance decides the persona; sessions without one keep whatever the console
            // (or the default catalog) assigned to them.
            if let Some(instance) = instance.as_ref()
                && let Some(persona_id) = instance.effective_persona_id()
                && let Some(sessions) = agent.session_manager()
            {
                sessions.set_persona(&session_id, persona_id);
            }
            // Filter hosts whose circuit breaker is Open to fast-skip them and protect LLM throughput
            // Plugin hosts and MCP servers are both tool sources; the router sees one slice.
            let mut active_hosts: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>> = Vec::new();
            for host in &hosts {
                if host.circuit_breaker.allow_request() {
                    active_hosts
                        .push(Arc::clone(host) as Arc<dyn kanon_llm::tool_router::ToolHost>);
                } else {
                    tracing::warn!(
                        host_id = %host.host_id,
                        event_id = %filtered_event.event_id,
                        "Circuit breaker is OPEN; fast-skipping host from ToolRouter candidates"
                    );
                    self.observe(PipelineStage::CircuitBreakerTripped {
                        event_id: filtered_event.event_id.clone(),
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
            if let (Some(mcp), Some(toggles)) = (&self.mcp, &self.toggles) {
                active_hosts.extend(mcp.hosts_for_instance(toggles, instance.as_ref()).await);
            }

            // A model the catalog marks as not tool-capable is offered no external tools. Native
            // in-process tools stay available: they never leave the node and cost nothing to offer.
            let tool_hosts = if capabilities.tool_calling {
                active_hosts
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    "Model catalog disables tool calling; offering no plugin tools"
                );
                Vec::new()
            };

            match crate::with_bash_caller(
                bash_caller,
                router.execute_message(&session_id, user_message, &tool_hosts),
            )
            .await
            {
                Ok(output) => {
                    // The model's reasoning channel arrives folded into the completion text as a
                    // `<think>` block (a console display convention). Chat platforms must never
                    // receive it, so the delivered answer is stripped to the visible part only.
                    let answer = strip_reasoning_tags(&output.content);
                    if answer.is_empty() {
                        tracing::debug!("LLM produced no user-visible answer; passing downstream");
                        return PipelineResult::Passed(filtered_event);
                    }

                    let mut replies = vec![MessageSegment {
                        segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                            content: answer.to_string(),
                        })),
                    }];

                    // Rich media produced by a tool (an MCP server drawing a B50 card, for example)
                    // travels as its own segment: the platform then shows the picture instead of a
                    // sentence describing where it was written.
                    for attachment in &output.attachments {
                        let source = match (&attachment.file_path, &attachment.url) {
                            (Some(path), _) => Some(
                                kanon_proto::v1::image_segment::Source::FilePath(path.clone()),
                            ),
                            (None, Some(url)) => {
                                Some(kanon_proto::v1::image_segment::Source::Url(url.clone()))
                            }
                            (None, None) => None,
                        };
                        let Some(source) = source else {
                            tracing::warn!(
                                mime_type = %attachment.mime_type,
                                "Tool attachment has neither a file path nor a URL; dropping it"
                            );
                            continue;
                        };
                        replies.push(MessageSegment {
                            segment: Some(Segment::Image(kanon_proto::v1::ImageSegment {
                                source: Some(source),
                                mime_type: Some(attachment.mime_type.clone()),
                                filename: None,
                            })),
                        });
                    }

                    // The platform adapter turns this into its native quote of the triggering message.
                    if quote_reply {
                        replies.insert(
                            0,
                            MessageSegment {
                                segment: Some(Segment::Reply(kanon_proto::v1::ReplySegment {
                                    target_message_id: filtered_event.event_id.clone(),
                                    snippet: String::new(),
                                })),
                            },
                        );
                    }

                    self.observe(PipelineStage::LlmReplied {
                        event_id,
                        session_id,
                        content_length: answer.chars().count(),
                    });
                    return PipelineResult::LlmReplied {
                        content: answer.to_string(),
                        replies,
                    };
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "LLM reasoning and tool execution failed"
                    );
                }
            }
        }

        PipelineResult::Passed(filtered_event)
    }

    /// Handles the built-in `/new` command for one conversation.
    ///
    /// The previous session is never deleted: rotation only moves the conversation to a new
    /// session key, so the old history stays inspectable in the console.
    async fn handle_new_session(
        &self,
        event: &PipelineEventRequest,
        instance: &crate::instance::BotInstance,
    ) -> PipelineResult {
        let Some(registry) = self.instances.as_ref() else {
            // Unreachable in the node (the instance gate implies a registry), but a missing
            // registry must not silently pretend the command succeeded.
            tracing::error!(
                instance_id = %instance.id,
                "Built-in /new received without an instance catalog; ignoring command"
            );
            return PipelineResult::Passed(event.clone());
        };

        let conversation = conversation_key(event);
        match registry.rotate_session(&instance.id, &conversation).await {
            Ok(session_id) => {
                tracing::info!(
                    instance_id = %instance.id,
                    session_id = %session_id,
                    conversation = %conversation,
                    "Built-in /new started a new session for this conversation"
                );
                let reply = MessageSegment {
                    segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                        content: "已开启新会话。".to_string(),
                    })),
                };
                PipelineResult::SessionRotated {
                    instance_id: instance.id.clone(),
                    session_id,
                    replies: vec![reply],
                }
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %instance.id,
                    error = %err,
                    "Failed to rotate session for built-in /new; conversation unchanged"
                );
                PipelineResult::Passed(event.clone())
            }
        }
    }

    /// Handles the built-in `/model` command for one instance.
    ///
    /// With no argument — or one that is not a valid index — it lists the models an operator may
    /// switch to. With a valid 1-based index it persists the choice on the instance, so a restart
    /// does not silently move the conversation back to the previous model.
    async fn handle_model_command(
        &self,
        instance: &crate::instance::BotInstance,
        args: &[String],
    ) -> PipelineResult {
        let options = self.model_options(instance);
        let current = instance
            .model
            .clone()
            .or_else(|| self.node_model_reference());

        let selected = args
            .first()
            .and_then(|arg| arg.trim().parse::<usize>().ok())
            .filter(|index| *index >= 1 && *index <= options.len());

        let Some(index) = selected else {
            return PipelineResult::ModelListed {
                instance_id: instance.id.clone(),
                count: options.len(),
                replies: vec![text_reply(render_model_list(&options, current.as_deref()))],
            };
        };

        let target = options[index - 1].0.clone();
        let Some(registry) = self.instances.as_ref() else {
            // Unreachable in the node (the instance gate implies a registry), but a missing
            // registry must not silently pretend the switch succeeded.
            tracing::error!(
                instance_id = %instance.id,
                "Built-in /model received without an instance catalog; ignoring command"
            );
            return PipelineResult::ModelListed {
                instance_id: instance.id.clone(),
                count: options.len(),
                replies: vec![text_reply(render_model_list(&options, current.as_deref()))],
            };
        };

        match registry.set_model(&instance.id, Some(target.clone())).await {
            Ok(_) => {
                tracing::info!(
                    instance_id = %instance.id,
                    model = %target,
                    "Built-in /model switched the instance model"
                );
                PipelineResult::ModelSelected {
                    instance_id: instance.id.clone(),
                    model: target.clone(),
                    replies: vec![text_reply(format!("已切换当前实例模型为 {target}。"))],
                }
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %instance.id,
                    model = %target,
                    error = %err,
                    "Failed to persist the model selected by built-in /model"
                );
                PipelineResult::ModelListed {
                    instance_id: instance.id.clone(),
                    count: options.len(),
                    replies: vec![text_reply(format!("切换模型失败：{err}"))],
                }
            }
        }
    }

    /// Selectable models, in listing order, with their catalog settings when known.
    ///
    /// The node's default model and the instance's current model are always present even when the
    /// catalog is empty, so `/model` stays useful on a freshly configured node.
    fn model_options(
        &self,
        instance: &crate::instance::BotInstance,
    ) -> Vec<(String, Option<ModelSpec>)> {
        let mut options: Vec<(String, Option<ModelSpec>)> = self
            .agent_factory
            .as_ref()
            .map(|factory| {
                factory
                    .models()
                    .list()
                    .into_iter()
                    .map(|spec| (spec.full_name(), Some(spec)))
                    .collect()
            })
            .unwrap_or_default();

        for extra in [self.node_model_reference(), instance.model.clone()]
            .into_iter()
            .flatten()
        {
            if !options.iter().any(|(reference, _)| reference == &extra) {
                options.push((extra, None));
            }
        }

        options
    }

    /// Canonical model reference of the node's default agent, when configured.
    fn node_model_reference(&self) -> Option<String> {
        self.agent_factory
            .as_ref()
            .and_then(|factory| factory.default_model())
            .or_else(|| self.agent.current().map(|agent| agent.config().model_ref()))
    }

    /// Answers the built-in `/help` command.
    ///
    /// Lists the core's own commands first and then every command the active plugin hosts declare,
    /// so one message tells a user everything they can type without opening the console.
    fn handle_help_command(&self, hosts: &[Arc<crate::supervisor::ManagedHost>]) -> PipelineResult {
        let mut rendered = String::from("内置指令：\n");
        rendered.push_str("/new — 开始新会话\n");
        rendered.push_str("/model — 列出可用模型；/model <序号> 切换当前实例模型\n");
        rendered.push_str("/help — 显示本帮助\n");
        rendered.push_str("/info — 显示系统与运行信息\n");

        // Command names are deduplicated: two plugins claiming the same name would otherwise show
        // up twice, while routing already resolves that collision deterministically.
        let mut plugin_commands: Vec<(String, String, String)> = Vec::new();
        for host in hosts {
            for plugin in &host.meta {
                for command in &plugin.commands {
                    let name = command.name.trim().trim_start_matches('/').to_string();
                    if name.is_empty() || plugin_commands.iter().any(|(seen, ..)| *seen == name) {
                        continue;
                    }
                    plugin_commands.push((
                        name,
                        command.description.trim().to_string(),
                        command.usage.trim().to_string(),
                    ));
                }
            }
        }

        if !plugin_commands.is_empty() {
            plugin_commands.sort_by(|left, right| left.0.cmp(&right.0));
            rendered.push_str("\n插件指令：\n");
            for (name, description, usage) in plugin_commands {
                rendered.push('/');
                rendered.push_str(&name);
                if !description.is_empty() {
                    rendered.push_str(" — ");
                    rendered.push_str(&description);
                }
                if !usage.is_empty() {
                    rendered.push_str("（用法: ");
                    rendered.push_str(&usage);
                    rendered.push('）');
                }
                rendered.push('\n');
            }
        }

        PipelineResult::BuiltinReplied {
            command: HELP_COMMAND.to_string(),
            replies: vec![text_reply(rendered)],
        }
    }

    /// Answers the built-in `/info` command.
    ///
    /// Deliberately short: platform, one line of host facts, local time and the model this
    /// conversation is served by. Anything more belongs in the console, not in a chat message.
    fn handle_info_command(
        &self,
        instance: Option<&crate::instance::BotInstance>,
        event: &PipelineEventRequest,
    ) -> PipelineResult {
        let now = crate::time::now_unix();
        let timezone = match crate::time::local_offset_seconds(now) {
            Some(offset) => format!(" {}", crate::time::format_offset(offset)),
            None => String::new(),
        };

        let mut rendered = String::new();
        #[cfg(target_os = "macos")]
        {
            // Separate the product version from the Darwin kernel release on the same line.
            let architecture = match std::env::consts::ARCH {
                "aarch64" => "ARM64 (aarch64)",
                "x86_64" => "x86-64 (x86_64)",
                architecture => architecture,
            };
            rendered.push_str(&format!(
                "System: {} | Kernel: Darwin {} | Arch: {architecture}\n",
                distribution_name(),
                kernel_release().unwrap_or_else(|| "unknown".to_string())
            ));
        }
        #[cfg(not(target_os = "macos"))]
        rendered.push_str(&format!(
            "系统: {} {} ({})\n",
            distribution_name(),
            kernel_release().unwrap_or_default(),
            std::env::consts::ARCH
        ));
        rendered.push_str(&format!(
            "时间: {}{timezone}\n",
            crate::time::format_local(now)
        ));
        if let Some(instance) = instance {
            rendered.push_str(&format!("实例: {} ({})\n", instance.name, instance.id));
        }
        let model = instance
            .and_then(|instance| instance.model.clone())
            .or_else(|| self.node_model_reference())
            .unwrap_or_else(|| "未配置".to_string());
        rendered.push_str(&format!("模型: {model}\n"));
        rendered.push_str(&format!("适配器: {}", event.platform));

        PipelineResult::BuiltinReplied {
            command: INFO_COMMAND.to_string(),
            replies: vec![text_reply(rendered)],
        }
    }

    /// Runs the asynchronous worker loop, draining events from the ingest receiver.
    ///
    /// For every ingested event, the worker runs the pipeline and routes any outbound
    /// replies to the registered outbound message sender channel.
    ///
    /// # Shutdown
    /// When [`PipelineEngine::drain`] starts, the ingest queue is closed (producers get an
    /// explicit `Closed` error), every event still queued is written to the dead-letter log
    /// without being started, and the event in progress gets [`SHUTDOWN_EVENT_GRACE`] to finish
    /// before it is recorded as well. Nothing the node acknowledged disappears silently.
    pub async fn run_worker_loop(&self, mut event_receiver: mpsc::Receiver<IngestEventRequest>) {
        tracing::info!("Pipeline worker loop started");
        let mut phase = self.shutdown.subscribe();
        let draining = |p| p != ShutdownPhase::Running;

        loop {
            let req = tokio::select! {
                biased;
                _ = wait_for_phase(&mut phase, draining) => break,
                req = event_receiver.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
            };

            let in_flight = req.event.clone();
            let handled = self.handle_ingested(req);
            tokio::pin!(handled);
            tokio::select! {
                biased;
                () = &mut handled => {}
                _ = wait_for_phase(&mut phase, draining) => {
                    // Queued events are recorded before waiting on the current one, so a slow
                    // model cannot use up the time the process has left.
                    self.spill_queued_events(&mut event_receiver).await;
                    if tokio::time::timeout(SHUTDOWN_EVENT_GRACE, handled).await.is_err()
                        && let Some(event) = in_flight
                    {
                        self.dead_letter_event(
                            &event,
                            "node shut down while the event was being processed; its reply may be missing",
                        )
                        .await;
                    }
                    tracing::info!("Pipeline worker loop terminated");
                    return;
                }
            }
        }

        self.spill_queued_events(&mut event_receiver).await;
        tracing::info!("Pipeline worker loop terminated");
    }

    /// Closes the ingest queue and records every event still in it as a dead letter.
    async fn spill_queued_events(&self, event_receiver: &mut mpsc::Receiver<IngestEventRequest>) {
        event_receiver.close();
        while let Some(req) = event_receiver.recv().await {
            if let Some(event) = req.event {
                self.dead_letter_event(&event, "node shut down before the event was processed")
                    .await;
            }
        }
    }

    /// Runs one ingested event through the pipeline and queues its replies for delivery.
    async fn handle_ingested(&self, req: IngestEventRequest) {
        let event = match req.event {
            Some(evt) => evt,
            None => {
                tracing::warn!("Received IngestEventRequest with empty inner event; skipping");
                return;
            }
        };

        let platform = if !event.platform.is_empty() {
            event.platform.clone()
        } else {
            req.platform.clone()
        };
        let channel_id = event.channel_id.clone();
        let recipient_id = event.sender_id.clone();
        let event_id = event.event_id.clone();

        self.observe(PipelineStage::Ingested {
            event_id: event_id.clone(),
            platform: platform.clone(),
            channel_id: channel_id.clone(),
            sender_id: recipient_id.clone(),
        });

        tracing::info!(
            platform = %platform,
            channel_id = %channel_id,
            sender_id = %recipient_id,
            event_id = %event_id,
            "Pipeline received inbound event"
        );

        let result = self.process_event(event).await;

        match &result {
            PipelineResult::LlmReplied { content, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    content_len = content.len(),
                    "Pipeline generated LLM reply"
                );
            }
            PipelineResult::CommandExecuted {
                command, success, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    success = %success,
                    "Pipeline executed command"
                );
            }
            PipelineResult::Blocked { host_id, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    host_id = %host_id,
                    "Pipeline event blocked by PreFilter"
                );
            }
            PipelineResult::CommandNotFound { command } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline slash command not found"
                );
            }
            PipelineResult::SessionRotated {
                instance_id,
                session_id,
                ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    session_id = %session_id,
                    "Pipeline rotated conversation session via built-in /new"
                );
            }
            PipelineResult::ModelSelected {
                instance_id, model, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    model = %model,
                    "Pipeline switched the instance model via built-in /model"
                );
            }
            PipelineResult::ModelListed {
                instance_id, count, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    count = %count,
                    "Pipeline listed models via built-in /model"
                );
            }
            PipelineResult::ReplySuppressed {
                instance_id,
                reason,
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    reason = %reason,
                    "Pipeline suppressed a reply by policy"
                );
            }
            PipelineResult::BuiltinReplied { command, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline answered a built-in informational command"
                );
            }
            PipelineResult::Notice { kind, outcome } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    notice = %kind,
                    outcome = %outcome,
                    "Pipeline handled a platform notice without answering"
                );
            }
            PipelineResult::NoInstance { .. } => {
                // Already logged with the platform in `process_event`; nothing was delivered.
            }
            PipelineResult::Passed(_) => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    "Pipeline event passed (no matching slash command or active LLM provider)"
                );
            }
        }

        let replies = match &result {
            PipelineResult::Blocked { replies, .. } => replies,
            PipelineResult::CommandExecuted { replies, .. } => replies,
            PipelineResult::LlmReplied { replies, .. } => replies,
            PipelineResult::SessionRotated { replies, .. } => replies,
            PipelineResult::ModelSelected { replies, .. } => replies,
            PipelineResult::ModelListed { replies, .. } => replies,
            PipelineResult::BuiltinReplied { replies, .. } => replies,
            _ => &[][..],
        };

        if !replies.is_empty() {
            let deliver_req = DeliverMessageRequest {
                platform,
                channel_id,
                recipient_id,
                segments: replies.to_vec(),
                event_id: event_id.clone(),
            };

            let platform = deliver_req.platform.clone();
            let channel_id = deliver_req.channel_id.clone();
            let segment_count = deliver_req.segments.len();

            // Non-blocking hand-off: the pipeline worker must never await platform I/O.
            match self.outbound_sender.try_send(deliver_req.into()) {
                Ok(()) => {
                    self.observe(PipelineStage::OutboundQueued {
                        event_id,
                        platform,
                        channel_id,
                        segment_count,
                    });
                }
                Err(mpsc::error::TrySendError::Full(dropped)) => {
                    tracing::warn!(
                        platform = %platform,
                        channel_id = %channel_id,
                        "Outbound queue is full; dropping reply to dead letter to protect pipeline latency"
                    );
                    let dead_letter = Arc::clone(&self.dead_letter);
                    tokio::spawn(async move {
                        let _ = dead_letter
                            .write_record(&dropped.request, "outbound queue is full")
                            .await;
                    });
                    self.observe(PipelineStage::OutboundFailed {
                        platform,
                        channel_id,
                        reason: "outbound queue is full; reply dropped".to_string(),
                    });
                }
                Err(mpsc::error::TrySendError::Closed(dropped)) => {
                    tracing::warn!(
                        platform = %platform,
                        channel_id = %channel_id,
                        "Outbound dispatcher is not running; dropping reply to dead letter"
                    );
                    // Written off the worker, like the full-queue case, so the pipeline never
                    // waits on disk I/O for a reply it cannot send anyway.
                    let dead_letter = Arc::clone(&self.dead_letter);
                    tokio::spawn(async move {
                        if let Err(err) = dead_letter
                            .write_record(&dropped.request, "outbound dispatcher is not running")
                            .await
                        {
                            tracing::error!(error = %err, "Failed to persist dead letter record; the reply is lost");
                        }
                    });
                    self.observe(PipelineStage::OutboundFailed {
                        platform,
                        channel_id,
                        reason: "outbound dispatcher is not running; reply dropped".to_string(),
                    });
                }
            }
        }
    }

    /// Shuts the pipeline down without losing anything it already accepted.
    ///
    /// Inbound first: the worker closes the ingest queue, records queued events and gets
    /// [`SHUTDOWN_EVENT_GRACE`] for the event in progress, whose replies still reach the outbound
    /// queue. Then outbound: queued replies are delivered for up to [`SHUTDOWN_DELIVERY_GRACE`] and
    /// the rest are recorded. Adapters and plugin hosts must stay up until this returns, because
    /// the final deliveries go through them.
    pub async fn drain(&self, worker: JoinHandle<()>, dispatcher: Option<JoinHandle<()>>) {
        self.shutdown.send_replace(ShutdownPhase::DrainingInbound);
        if let Err(err) = worker.await {
            tracing::error!(error = %err, "Pipeline worker failed during shutdown");
        }
        self.shutdown.send_replace(ShutdownPhase::DrainingOutbound(
            tokio::time::Instant::now() + SHUTDOWN_DELIVERY_GRACE,
        ));
        if let Some(dispatcher) = dispatcher
            && let Err(err) = dispatcher.await
        {
            tracing::error!(error = %err, "Outbound dispatcher failed during shutdown");
        }
    }

    /// Spawns the pipeline worker loop as a background Tokio task.
    pub fn start_worker(
        self: Arc<Self>,
        event_receiver: mpsc::Receiver<IngestEventRequest>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run_worker_loop(event_receiver).await;
        })
    }
}
