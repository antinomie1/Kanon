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

use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
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

use crate::access::{CommandAccess, CommandPolicyStore, META_SENDER_NAME};
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
use crate::toggle::{PLUGIN_SECTION, ToggleStore};

/// Maximum independently running inbound chats; queued work remains bounded by ingest capacity.
pub const MAX_CONCURRENT_CHATS: usize = 16;

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
            Self::CommandNotFound { .. }
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

/// Replaces the text of a message, keeping its other segments (images, mentions) in place.
///
/// The first text segment takes the new text and the other text segments are dropped, so the
/// model reads the replacement exactly once; a message without text gets the text in front.
fn replace_message_text(event: &mut PipelineEventRequest, text: String) {
    let mut replaced = false;
    event
        .segments
        .retain_mut(|segment| match &mut segment.segment {
            Some(Segment::Text(existing)) if !replaced => {
                existing.content = text.clone();
                replaced = true;
                true
            }
            Some(Segment::Text(_)) => false,
            _ => true,
        });
    if !replaced && !event.segments.is_empty() {
        event.segments.insert(0, text_reply(text.clone()));
    }
    event.raw_text = text;
}

/// Name of the built-in session command, handled by the core and never by the model.
pub const NEW_SESSION_COMMAND: &str = "new";

/// Name of the built-in command listing the chat's conversations (`/ls`).
pub const LIST_SESSIONS_COMMAND: &str = "ls";

/// Name of the built-in command making another conversation current (`/switch <n>`).
pub const SWITCH_SESSION_COMMAND: &str = "switch";

/// Name of the built-in command deleting a conversation (`/del [n]`, the current one by default).
pub const DELETE_SESSION_COMMAND: &str = "del";

/// Built-in commands that act on the chat's conversations; like `/new` they need an instance.
const CONVERSATION_COMMANDS: [&str; 4] = [
    NEW_SESSION_COMMAND,
    LIST_SESSIONS_COMMAND,
    SWITCH_SESSION_COMMAND,
    DELETE_SESSION_COMMAND,
];

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

/// Name of the built-in command that stops the running model turns of an instance.
///
/// A turn can keep the bot busy for minutes (a model calling tools over and over, a hung
/// provider), while its chat lane holds later messages until the turn finishes.
/// `/stop` is therefore taken out of the queue while a turn runs (see
/// [`PipelineEngine::run_worker_loop`]) and handled at once. It acts on every chat of the
/// instance, so only administrators may use it unless the command policy says otherwise.
pub const STOP_COMMAND: &str = "stop";

/// Identity of one conversation inside an instance: channel plus sender, or the channel alone when
/// the instance shares group sessions.
///
/// Kept as a free function because both the instance gate and the LLM phase must derive exactly
/// the same key — the `/new` command and the messages that follow it have to agree.
pub(crate) fn conversation_key(event: &PipelineEventRequest, shared: bool) -> String {
    if shared && !event.channel_id.trim().is_empty() {
        return event.channel_id.clone();
    }
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

/// Whether an event's conversation is one session shared by the whole group.
pub(crate) fn shares_session(
    instance: Option<&crate::instance::BotInstance>,
    event: &PipelineEventRequest,
) -> bool {
    instance.is_some_and(|instance| instance.session_scope == SessionScope::Group)
        && ConversationKind::from_metadata(event.metadata.as_ref()).is_policy_governed()
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
/// The text a command is parsed from: the raw text, or the first text segment without one.
fn message_text(event: &PipelineEventRequest) -> String {
    if !event.raw_text.is_empty() {
        return event.raw_text.clone();
    }
    event
        .segments
        .iter()
        .find_map(|s| match &s.segment {
            Some(Segment::Text(t)) => Some(t.content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Whether an ingested event is a `/stop` command, read the way the pipeline reads commands.
///
/// Only the shape is checked here; the pipeline itself decides whether an instance owns the
/// platform and whether the sender may stop it.
fn is_stop_request(req: &IngestEventRequest) -> bool {
    let Some(event) = req.event.as_ref() else {
        return false;
    };
    if NoticeKind::from_metadata(event.metadata.as_ref()).is_some() {
        return false;
    }
    CommandRouter::parse_command(strip_leading_mentions(&message_text(event)))
        .is_some_and(|command| command.name.eq_ignore_ascii_case(STOP_COMMAND))
}

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

/// The chat notice for a model turn that failed, naming the cause without the provider's raw
/// error body, which can be long and is meant for the operator's log rather than a group chat.
///
/// It says outright that nothing retries: the sender should know that sending again is theirs to
/// decide, and the node never repeats a failing request on its own.
/// Names of the tools a turn called, in call order.
fn tool_names(executed: &[kanon_llm::ExecutedToolCall]) -> Vec<String> {
    executed.iter().map(|call| call.tool_name.clone()).collect()
}

/// The failure category `AGENT_DONE` reports for a turn that ended without an answer.
///
/// Categories, not messages: an error's text can carry a provider's response body, which a
/// subscriber has no use for and should not receive.
fn failure_category(error: &kanon_llm::ToolRouterError) -> &'static str {
    match error {
        kanon_llm::ToolRouterError::Stopped => "stopped",
        kanon_llm::ToolRouterError::Gateway(_) => "model_error",
        kanon_llm::ToolRouterError::Rpc(_)
        | kanon_llm::ToolRouterError::ToolNotFound(_)
        | kanon_llm::ToolRouterError::ToolFailed(_) => "tool_error",
        kanon_llm::ToolRouterError::InvalidRequest(_) => "request_error",
        kanon_llm::ToolRouterError::Memory(_) => "memory_error",
        kanon_llm::ToolRouterError::Busy(_) => "busy",
        kanon_llm::ToolRouterError::Compaction(_) => "compaction_error",
    }
}

fn failure_notice(error: &kanon_llm::ToolRouterError) -> String {
    use kanon_llm::{GatewayError, ToolRouterError};
    let reason = match error {
        ToolRouterError::Gateway(GatewayError::ApiStatus { status, .. }) => {
            format!("模型服务返回错误（HTTP {status}）")
        }
        ToolRouterError::Gateway(GatewayError::Http(err)) if err.is_timeout() => {
            "模型服务响应超时".to_string()
        }
        ToolRouterError::Gateway(GatewayError::Http(_)) => "无法连接模型服务".to_string(),
        ToolRouterError::Gateway(GatewayError::Json(_) | GatewayError::InvalidResponse(_)) => {
            "模型的结果无法处理".to_string()
        }
        ToolRouterError::Rpc(_) => "插件工具调用失败".to_string(),
        ToolRouterError::ToolFailed(_) => "工具执行失败".to_string(),
        ToolRouterError::ToolNotFound(name) => format!("模型调用了未注册的工具 {name}"),
        ToolRouterError::InvalidRequest(_) => "请求配置无效，请检查节点配置".to_string(),
        ToolRouterError::Memory(_) => "会话存储失败".to_string(),
        ToolRouterError::Busy(_) => "会话正在处理其他任务".to_string(),
        ToolRouterError::Compaction(_) => "会话压缩失败".to_string(),
        // A stopped turn is answered by `/stop` itself; the caller never asks for this notice.
        ToolRouterError::Stopped => "任务已停止".to_string(),
    };
    format!("这次没能回复：{reason}。不会自动重试，可以稍后再发。")
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

/// A conversation title for a chat reply; an empty conversation has none.
fn display_title(title: &str) -> &str {
    if title.is_empty() {
        "（空会话）"
    } else {
        title
    }
}

/// Renders the `/ls` listing: oldest first, numbered from 1, the current one marked.
fn render_conversation_list(conversations: &[super::conversations::ConversationInfo]) -> String {
    let mut rendered = String::from("会话列表：\n");
    for (index, conversation) in conversations.iter().enumerate() {
        rendered.push_str(&format!(
            "{}. {}",
            index + 1,
            display_title(&conversation.title)
        ));
        let mut details = vec![format!("{} 条消息", conversation.message_count)];
        if conversation.last_active_at > 0 {
            // `YYYY-MM-DD HH:MM:SS` → `MM-DD HH:MM`: the year and seconds are noise in a chat.
            let time = crate::time::format_local(conversation.last_active_at as i64);
            details.push(time.get(5..16).unwrap_or(&time).to_string());
        }
        rendered.push_str(&format!("（{}）", details.join("，")));
        if conversation.current {
            rendered.push_str(" ✓ 当前");
        }
        rendered.push('\n');
    }
    rendered.push_str("/switch <序号> 切换，/del <序号> 删除，/new 新建。");
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

/// One agent turn in a conversation, ready to run (see [`PipelineEngine::run_conversation_turn`]).
pub(crate) struct ConversationTurn<'a> {
    /// The agent answering the turn.
    pub(crate) agent: Arc<dyn kanon_llm::Agent>,
    /// Registration created before waiting for the session writer, so `/stop` reaches that wait.
    pub(crate) running: super::turns::TurnGuard<'a>,
    /// The conversation's session.
    pub(crate) session_id: &'a str,
    /// The inbound message the turn answers.
    pub(crate) event: &'a PipelineEventRequest,
    /// Hosts of the plugins the instance runs: they hear the turn's events and hooks.
    pub(crate) hosts: &'a [Arc<crate::supervisor::ManagedHost>],
    /// Tool sources offered to the model.
    pub(crate) tool_hosts: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>>,
    /// Who may be running Bash through this turn; `None` refuses it.
    pub(crate) bash_caller: Option<crate::BashCaller>,
    /// Per-turn agent settings.
    pub(crate) options: kanon_llm::TurnOptions,
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
    turns: super::turns::RunningTurns,
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
    pub(crate) fn running_turns(&self) -> &super::turns::RunningTurns {
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
    /// Without a toggle store or an instance every host is kept, as the node behaved before
    /// instances existed.
    async fn instance_hosts(
        &self,
        instance: Option<&crate::instance::BotInstance>,
        hosts: Vec<Arc<crate::supervisor::ManagedHost>>,
    ) -> Vec<Arc<crate::supervisor::ManagedHost>> {
        let (Some(toggles), Some(instance)) = (&self.toggles, instance) else {
            return hosts;
        };
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
        let Some(toggles) = &self.toggles else {
            return hosts;
        };
        let mut enabled = Vec::with_capacity(hosts.len());
        for host in hosts {
            let plugin_id = host.primary_plugin_id().unwrap_or_default();
            if toggles.is_enabled(PLUGIN_SECTION, &plugin_id).await {
                enabled.push(host);
            }
        }
        enabled
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
    /// registered so `/stop` reaches it, and run inside [`super::agent_hook::with_turn`] so its
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
                options.persona = instances
                    .persona_for_instance(instance_id, personas.map(Arc::as_ref))
                    .await
                    .map_err(|error| {
                        kanon_llm::ToolRouterError::InvalidRequest(error.to_string())
                    })?;
            }
            let router = ToolRouter::from_arc(agent);
            super::agent_hook::with_turn(
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
        drop(running);

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

    /// Tells subscribed plugins that the bot sent a message on `request.platform`.
    ///
    /// The plugins are those of the instance serving the platform, as for inbound events; with
    /// an instance registry but no instance claiming the platform nobody is told.
    async fn emit_message_sent(&self, request: &DeliverMessageRequest, message_id: &str) {
        let hosts = self.supervisor.get_all_hosts().await;
        if !hosts.iter().any(|host| {
            host.metas()
                .iter()
                .any(|plugin| plugin.events().any(|kind| kind == EventKind::MessageSent))
        }) {
            return;
        }
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&request.platform).await {
                Ok(Some(instance)) => Some(instance),
                _ => return,
            },
            None => None,
        };
        let hosts = self.instance_hosts(instance.as_ref(), hosts).await;
        hooks::emit_event(
            &hosts,
            EventKind::MessageSent,
            Detail::MessageSent(MessageSentEvent {
                message: Some(request.clone()),
                message_id: message_id.to_string(),
            }),
        );
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

        // Keep the reservation alive across delivery so cancellation cannot strand the
        // platform's sole half-open recovery probe.
        let Some(permit) = breaker.try_acquire() else {
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
        };

        let start = std::time::Instant::now();
        match self.deliver_outbound(request.clone()).await {
            Ok(outcome) => {
                let response = DeliverMessageResponse {
                    success: true,
                    message_id: outcome.message_id.clone(),
                    error_message: String::new(),
                };
                permit.success(start.elapsed());
                self.emit_message_sent(&request, &outcome.message_id).await;
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
                permit.failure(&reason);
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
                        // A complete answer occupies one queue slot. Expand it here, then finish
                        // all its parts before taking another answer to preserve platform FIFO.
                        let mut request = message.request;
                        let messages = if message.split_lines {
                            let limit = match engine.supervisor.adapters().get(&platform).await {
                                Some(adapter) => adapter.reply_message_limit(&request),
                                None => usize::MAX,
                            };
                            split_reply_lines(&std::mem::take(&mut request.segments), limit)
                        } else {
                            vec![std::mem::take(&mut request.segments)]
                        };
                        let mut messages = messages.into_iter();
                        let mut response = DeliverMessageResponse {
                            success: true,
                            ..Default::default()
                        };
                        while let Some(segments) = messages.next() {
                            let part = DeliverMessageRequest {
                                segments,
                                ..request.clone()
                            };
                            // Poll the deadline first for every part, including within a batch.
                            response = tokio::select! {
                                biased;
                                () = delivery_deadline_passed(&mut phase) => {
                                    engine.dead_letter_reply(
                                        &part,
                                        "node shut down before the reply was delivered; the platform may or may not have received it",
                                    ).await
                                }
                                response = engine.dispatch_outbound_request_with_breaker(
                                    part.clone(), &breaker,
                                ) => response,
                            };
                            if !response.success {
                                // The attempted part was already recorded. Persist only the
                                // unsent suffix: replaying the original batch duplicates its prefix.
                                let reason = format!(
                                    "preceding part of this reply failed: {}",
                                    response.error_message,
                                );
                                for segments in messages {
                                    engine
                                        .dead_letter_reply(
                                            &DeliverMessageRequest {
                                                segments,
                                                ..request.clone()
                                            },
                                            &reason,
                                        )
                                        .await;
                                }
                                break;
                            }
                        }
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

        // Phase 0b: Per-instance plugin policy.
        //
        // Filtering here (rather than inside each later phase) means a plugin this instance
        // disabled cannot pre-filter, answer commands, offer tools or observe events — one
        // decision covers the whole pipeline. It runs before notices so a notice reaches only
        // the plugins this instance runs.
        let hosts = self.instance_hosts(instance.as_ref(), hosts).await;

        // Phase 0a: Notices.
        //
        // A join, poke or recall is not a message. Only the node's event policy decides whether the
        // bot reacts at all; a notice it reacts to then skips commands and the reply policy (the
        // operator explicitly asked for the reaction) and reaches the model as a one-line event.
        let notice = NoticeKind::from_metadata(event.metadata.as_ref());
        // Bash identity is taken from the adapter's event before plugin pre-filters can rewrite
        // it. A notice is not a message: its "sender" is a joining member or a poker, who never
        // asked for anything, so a notice turn has no caller.
        let bash_sender = (notice.is_none() && !event.sender_id.trim().is_empty())
            .then(|| format!("{platform}:{}", event.sender_id));
        if let Some(kind) = notice {
            // Subscribers observe every notice, whether or not the bot itself reacts to it.
            hooks::emit_event(&hosts, EventKind::Notice, Detail::Notice(event.clone()));
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
            } else if matches!(kind, NoticeKind::FriendRequest | NoticeKind::GroupInvite) {
                if !policy.accepts(kind) {
                    "left for a human: the event policy does not accept it".to_string()
                } else if let Some(adapter) = self.supervisor.adapters().get(&platform).await {
                    // Spawned: accepting is platform I/O and must not hold up the pipeline.
                    let request = event.clone();
                    tokio::spawn(async move {
                        match adapter.accept_request(&request).await {
                            Ok(()) => tracing::info!(
                                event_id = %request.event_id,
                                "Request accepted automatically by the event policy"
                            ),
                            Err(err) => tracing::warn!(
                                event_id = %request.event_id,
                                error = %err,
                                "Request could not be accepted"
                            ),
                        }
                    });
                    "accepted automatically".to_string()
                } else {
                    "no built-in adapter serves the platform to accept it".to_string()
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
        let text_candidate = message_text(&filtered_event);

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
        // The policy that decides who may run commands and fire triggers. Without a policy store
        // (an embedded pipeline) everything stays open, which is the pre-policy behaviour; the
        // node always installs one. An instance with its own command policy replaces the node's,
        // administrators included.
        let command_policy = self.command_policy.as_ref().map(|store| {
            instance.as_ref().map_or_else(
                || store.get(),
                |instance| instance.effective_command_policy(store.get()),
            )
        });

        // Phase 2a: Conversation captures.
        //
        // A plugin that asked a question gets the sender's next message before anything else may
        // interpret it — even text that looks like a command is the answer it is waiting for.
        // The capture is consumed here either way, so a plugin that has been disabled or whose
        // host is gone cannot keep swallowing the sender's messages.
        //
        // Phases 2a–2b run in a labelled block: a handler that hands the message on to the model
        // (`pass_to_model`) leaves it with the event the model should read, skipping the
        // remaining handlers, while every other outcome returns from the pipeline directly.
        let filtered_event = 'handlers: {
            if notice.is_none()
                && let Some(capture) = self.captures.take(&filtered_event)
            {
                match hosts.iter().find(|host| host.host_id == capture.host_id) {
                    Some(host) => {
                        tracing::debug!(
                            command = %capture.command,
                            plugin_id = %capture.plugin_id,
                            host_id = %host.host_id,
                            "Routing captured message to the waiting plugin"
                        );
                        self.observe(PipelineStage::CommandMatched {
                            event_id: event_id.clone(),
                            command: capture.command.clone(),
                            plugin_id: capture.plugin_id.clone(),
                            host_id: host.host_id.clone(),
                        });
                        let outcome = CommandRouter::dispatch_continuation(
                            host,
                            &capture,
                            command_text,
                            filtered_event.clone(),
                        )
                        .await;
                        match self
                            .command_result(
                                &hosts,
                                capture.command,
                                &capture.plugin_id,
                                host,
                                &filtered_event,
                                outcome,
                            )
                            .await
                        {
                            CommandFlow::Finished(result) => return result,
                            CommandFlow::PassToModel(event) => break 'handlers event,
                        }
                    }
                    None => tracing::warn!(
                        plugin_id = %capture.plugin_id,
                        host_id = %capture.host_id,
                        "Captured conversation's plugin is no longer active here; processing the message normally"
                    ),
                }
            }

            if let Some(parsed) = CommandRouter::parse_command(command_text) {
                let name = parsed.name.clone();
                // The conversation commands, `/model` and `/stop` act on an instance, so without
                // one they are ordinary names a plugin may claim; `/help` and `/info` are always
                // the core's.
                let builtin = ((CONVERSATION_COMMANDS
                    .iter()
                    .any(|command| name.eq_ignore_ascii_case(command))
                    || name.eq_ignore_ascii_case(MODEL_COMMAND)
                    || name.eq_ignore_ascii_case(STOP_COMMAND))
                    && instance.is_some())
                    || name.eq_ignore_ascii_case(HELP_COMMAND)
                    || name.eq_ignore_ascii_case(INFO_COMMAND);
                let target = if builtin {
                    None
                } else {
                    CommandRouter::resolve(&name, &hosts, &filtered_event)
                };

                // Phase 2-: Command permissions, checked once for built-in and plugin commands alike.
                // A plugin command is checked under its canonical name, so an alias can never bypass
                // a restriction, and with the access level its plugin declared as the default.
                let (policy_name, default_access) = match &target {
                    Some(target) => (target.name().to_string(), target.access()),
                    // Stopping cuts off answers in every chat of the instance, not only the
                    // sender's own.
                    None if name.eq_ignore_ascii_case(STOP_COMMAND) => {
                        (STOP_COMMAND.to_string(), CommandAccess::Admins)
                    }
                    // In a group that shares one session, switching or deleting the conversation
                    // does it for every member, so only administrators may by default.
                    None if instance.is_some()
                        && (name.eq_ignore_ascii_case(SWITCH_SESSION_COMMAND)
                            || name.eq_ignore_ascii_case(DELETE_SESSION_COMMAND)) =>
                    {
                        (name.to_ascii_lowercase(), CommandAccess::AdminsInGroups)
                    }
                    None => (name.clone(), CommandAccess::Everyone),
                };
                if let Some(policy) = command_policy.as_ref()
                    && !policy.allows_with_default(&policy_name, default_access, &filtered_event)
                {
                    // The sender's own ID is part of the answer: it is exactly what an operator adds
                    // to the administrator list to grant access.
                    let reply = format!(
                        "/{name} 仅限管理员使用（你的 ID：{}:{}）",
                        filtered_event.platform, filtered_event.sender_id
                    );
                    return PipelineResult::CommandDenied {
                        command: policy_name,
                        replies: vec![text_reply(reply)],
                    };
                }

                if builtin {
                    if name.eq_ignore_ascii_case(HELP_COMMAND) {
                        return self.handle_help_command(&hosts);
                    }
                    if name.eq_ignore_ascii_case(INFO_COMMAND) {
                        return self.handle_info_command(instance.as_ref(), &filtered_event);
                    }
                    if let Some(instance) = instance.as_ref() {
                        if name.eq_ignore_ascii_case(NEW_SESSION_COMMAND) {
                            return self.handle_new_session(&filtered_event, instance).await;
                        }
                        if let Some(command) = CONVERSATION_COMMANDS
                            .iter()
                            .find(|command| name.eq_ignore_ascii_case(command))
                        {
                            return self
                                .handle_conversation_command(
                                    command,
                                    &filtered_event,
                                    instance,
                                    &parsed.args,
                                )
                                .await;
                        }
                        if name.eq_ignore_ascii_case(STOP_COMMAND) {
                            return self.handle_stop_command(instance);
                        }
                        return self.handle_model_command(instance, &parsed.args).await;
                    }
                }

                let Some(target) = target else {
                    tracing::debug!(
                        command = %name,
                        "Slash command detected but no matching plugin was registered"
                    );
                    self.observe(PipelineStage::CommandNotFound {
                        event_id,
                        command: name.clone(),
                    });
                    return PipelineResult::CommandNotFound { command: name };
                };

                let command = target.name().to_string();
                tracing::debug!(
                    command = %command,
                    typed = %name,
                    plugin_id = %target.plugin_id,
                    host_id = %target.host.host_id,
                    "Routing slash command to target host"
                );
                self.observe(PipelineStage::CommandMatched {
                    event_id: event_id.clone(),
                    command: command.clone(),
                    plugin_id: target.plugin_id.clone(),
                    host_id: target.host.host_id.clone(),
                });
                let outcome =
                    CommandRouter::dispatch(&target, parsed, filtered_event.clone()).await;
                match self
                    .command_result(
                        &hosts,
                        command,
                        &target.plugin_id,
                        &target.host,
                        &filtered_event,
                        outcome,
                    )
                    .await
                {
                    CommandFlow::Finished(result) => return result,
                    CommandFlow::PassToModel(event) => break 'handlers event,
                }
            }

            // Phase 2b: Plugin triggers.
            //
            // A trigger is a plugin's claim on a plain message by pattern. It runs where a command
            // would — after pre-filters, before the reply policy and the model — because a plugin that
            // asked for "messages matching X" wants them whether or not the bot was mentioned.
            if notice.is_none()
                && !command_text.trim().is_empty()
                && let Some(target) =
                    self.triggers
                        .resolve(command_text, &filtered_event, &hosts, |meta| {
                            command_policy.as_ref().is_none_or(|policy| {
                                policy.allows_with_default(
                                    &meta.name,
                                    CommandAccess::from_proto(meta.access()),
                                    &filtered_event,
                                )
                            })
                        })
            {
                let trigger = target.meta.name.clone();
                tracing::debug!(
                    trigger = %trigger,
                    plugin_id = %target.plugin_id,
                    host_id = %target.host.host_id,
                    "Routing message to the plugin trigger it matched"
                );
                self.observe(PipelineStage::CommandMatched {
                    event_id: event_id.clone(),
                    command: trigger.clone(),
                    plugin_id: target.plugin_id.clone(),
                    host_id: target.host.host_id.clone(),
                });
                let outcome = CommandRouter::dispatch_trigger(
                    &target,
                    command_text.trim(),
                    filtered_event.clone(),
                )
                .await;
                match self
                    .command_result(
                        &hosts,
                        trigger,
                        &target.plugin_id,
                        &target.host,
                        &filtered_event,
                        outcome,
                    )
                    .await
                {
                    CommandFlow::Finished(result) => return result,
                    CommandFlow::PassToModel(event) => break 'handlers event,
                }
            }

            filtered_event
        };

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

        // Group context: a shared session labels every message with its speaker, and an observing
        // instance records every group message — answered or not — for its next turn.
        let shared = shares_session(instance.as_ref(), &filtered_event);
        let observing = instance
            .as_ref()
            .is_some_and(|instance| instance.observe_group)
            && kind.is_policy_governed()
            && notice.is_none();
        let group_key = format!("{platform}\u{1f}{}", filtered_event.channel_id);
        let speaker = metadata_str(filtered_event.metadata.as_ref(), META_SENDER_NAME)
            .map(str::to_owned)
            .unwrap_or_else(|| filtered_event.sender_id.clone());

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
                if observing {
                    self.group_log
                        .record(&group_key, &speaker, &filtered_event.raw_text);
                }
                return PipelineResult::ReplySuppressed {
                    instance_id: instance.id.clone(),
                    reason,
                };
            }
        }

        // Phase 3: Conversational message (unmatched by command router, routed to LLM if enabled)
        // The agent is resolved per event so a provider configured or cleared at runtime is
        // honoured immediately; an empty slot means "no conversational LLM" and passes through.
        let conversation = conversation_key(&filtered_event, shared);

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
        // Sessions are namespaced by the instance that owns the conversation, so two bots can
        // never share context. An unpartitioned pipeline keeps the legacy conversation key.
        let session_id = match instance.as_ref() {
            Some(instance) => instance.conversation_session_id(&conversation),
            None => conversation.clone(),
        };
        let mut filtered_event = filtered_event;
        let answering = resolved_agent.is_some();
        let mut lead: Vec<String> = Vec::new();
        if answering {
            lead.extend(self.recalls.take_notes(&ledger_key));
            // Plugin context joins the current turn only, after the core's own notes and before
            // the speaker label, which must stay directly in front of the message it introduces.
            // Preparers see the message as the user sent it, before any of this leading text.
            if notice.is_none() {
                lead.extend(hooks::prepare_turn(&hosts, &filtered_event, &session_id).await);
            }
        }
        // Observed group lines this session has not seen come first, then the speaker of the
        // current message; the current message is recorded so other sessions see it later.
        let mut log_block = None;
        if observing {
            let unseen = if answering {
                self.group_log.unseen(&group_key, &ledger_key)
            } else {
                Vec::new()
            };
            let seq = self
                .group_log
                .record(&group_key, &speaker, &filtered_event.raw_text);
            if answering {
                self.group_log.mark_seen(&group_key, &ledger_key, seq);
            }
            if !unseen.is_empty() {
                let lines: Vec<String> = unseen
                    .iter()
                    .map(|(who, text)| format!("{who}: {text}"))
                    .collect();
                log_block = Some(format!(
                    "[群聊记录]\n{}\n[当前消息] {speaker}:",
                    lines.join("\n")
                ));
            }
        }
        if answering {
            match log_block {
                Some(block) => lead.push(block),
                None if shared && notice.is_none() => lead.push(format!("{speaker}:")),
                None => {}
            }
        }
        // A text-only event is rendered from `raw_text` only when it has no segments; once leading
        // text becomes a segment, the message text must be one too or it would vanish.
        if !lead.is_empty()
            && filtered_event.segments.is_empty()
            && !filtered_event.raw_text.trim().is_empty()
        {
            let text = filtered_event.raw_text.clone();
            filtered_event.segments.push(text_reply(text));
        }
        for (index, text) in lead.into_iter().enumerate() {
            filtered_event.segments.insert(index, text_reply(text));
        }
        let mut user_message = match build_user_message(
            &filtered_event,
            &capabilities,
            &context_policy,
        ) {
            Ok(message) => message,
            Err(error) => {
                tracing::error!(event_id = %filtered_event.event_id, %error, "Invalid message payload");
                let asked = notice.is_none()
                    && (!kind.is_policy_governed()
                        || bot_mentioned(filtered_event.metadata.as_ref()));
                if answering && asked {
                    return PipelineResult::LlmFailed {
                        error: error.to_string(),
                        replies: vec![text_reply("消息格式无效，本轮无法处理。")],
                    };
                }
                return PipelineResult::Passed(filtered_event);
            }
        };
        // Only a turn that reaches a model needs its pictures; see `media` for why the node
        // downloads them instead of passing the platform's URLs on.
        if answering {
            super::media::inline_images(&mut user_message).await;
        }

        if (user_message
            .content
            .as_deref()
            .is_some_and(|content| !content.is_empty())
            || user_message.has_parts())
            && let Some(agent) = resolved_agent
        {
            // Remember what the model is shown, so a later recall of it can be noted.
            if notice.is_none() {
                self.recalls.record_turn(
                    &filtered_event.event_id,
                    &ledger_key,
                    &filtered_event.raw_text,
                );
            }

            // Let a built-in adapter show that an answer is coming (typing, a reaction) when the
            // reply policy asks for it. Spawned: platform I/O must never delay the model call.
            if reply_policy.acknowledge
                && let Some(adapter) = self.supervisor.adapters().get(&platform).await
            {
                let event = filtered_event.clone();
                tokio::spawn(async move {
                    if let Err(err) = adapter.acknowledge(&event).await {
                        tracing::warn!(error = %err, "Adapter failed to acknowledge an event");
                    }
                });
            }

            // Register before waiting: a console stream or background compaction can own the
            // writer, and /stop must cancel this lane rather than leave a delayed turn behind.
            let running = self
                .turns
                .begin(instance.as_ref().map(|instance| instance.id.clone()));
            let signal = running.signal();
            let writing = match agent.session_manager() {
                Some(sessions) => Some(tokio::select! {
                    biased;
                    () = signal.stopped() => return PipelineResult::Passed(filtered_event),
                    writing = sessions.write(&session_id) => writing,
                }),
                None => None,
            };
            // A model the catalog marks as not tool-capable is offered no external tools. Native
            // in-process tools stay available: they never leave the node and cost nothing to offer.
            let tool_hosts = if capabilities.tool_calling {
                self.tool_hosts(&hosts, instance.as_ref(), &filtered_event.event_id)
                    .await
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    "Model catalog disables tool calling; offering no plugin tools"
                );
                Vec::new()
            };

            // A shared or observed group session puts other members' words into this turn's
            // context, and they could steer an administrator's shell. The turn records that fact
            // and the Bash gate refuses it unless the instance explicitly allows shared contexts.
            let bash_caller = bash_sender.map(|id| crate::BashCaller {
                id,
                instance: instance.as_ref().map(|instance| instance.id.clone()),
                shared_context: shared || observing,
            });
            // The turn writes its messages as it goes; holding the session's lock keeps a plugin
            // from deleting the conversation or appending to it in between. Plugins only ever
            // try the lock, so a tool call of this very turn cannot deadlock on it.
            let turn = self.run_conversation_turn(
                ConversationTurn {
                    agent,
                    running,
                    session_id: &session_id,
                    event: &filtered_event,
                    hosts: &hosts,
                    tool_hosts,
                    bash_caller,
                    options: kanon_llm::TurnOptions::default(),
                },
                user_message,
            );
            let result = match writing {
                Some(writing) => writing.scope(turn).await,
                None => turn.await,
            };
            match result {
                Ok(output) => {
                    // Reasoning already has its own channel and parsed tool calls were removed by
                    // the agent. Delimiters or tool markup left in answer text are literal
                    // content, so delivery must not reinterpret them.
                    let answer = visible_reply(&output.content);
                    let answer = answer.as_str();
                    let mut replies = Vec::new();
                    if !answer.is_empty() {
                        replies.push(MessageSegment {
                            segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                                content: answer.to_string(),
                            })),
                        });
                    }

                    // Rich media produced by a tool (an MCP server drawing a B50 card, a TTS voice
                    // clip, a generated PDF) travels as its own segment of the kind its MIME type
                    // names, limited to the kinds the platform's adapter declares it can send.
                    if !output.attachments.is_empty() {
                        let declared = self
                            .supervisor
                            .resolve_adapter(&filtered_event.platform)
                            .await
                            .map(|route| route.capabilities());
                        let media = super::attachment::attachment_segments(
                            &output.attachments,
                            declared.as_deref(),
                        );
                        replies.extend(media.segments);
                        if !media.notes.is_empty() {
                            replies.push(text_reply(media.notes.join("\n")));
                        }
                    }

                    // Tool media is a complete reply even when the model produced only reasoning.
                    if replies.is_empty() {
                        tracing::debug!(
                            "LLM produced no user-visible answer or attachment; passing downstream"
                        );
                        return PipelineResult::Passed(filtered_event);
                    }

                    // Opt-in: the reasoning goes first as its own plain-text segment, content only.
                    // It is checked after the empty-reply gate so reasoning never becomes a reply
                    // on its own.
                    if reply_policy.send_reasoning {
                        let reasoning = output
                            .reasoning
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        if !reasoning.is_empty() {
                            replies.insert(
                                0,
                                MessageSegment {
                                    segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                                        content: reasoning,
                                    })),
                                },
                            );
                        }
                    }

                    // The bot's own words belong to the group's record too, and this session has
                    // just seen them.
                    if observing {
                        let seq = self.group_log.record(&group_key, "你", answer);
                        self.group_log.mark_seen(&group_key, &ledger_key, seq);
                    }

                    hooks::emit_event(
                        &hosts,
                        EventKind::LlmResponse,
                        Detail::LlmResponse(LlmResponseEvent {
                            context: Some(filtered_event.clone()),
                            content: answer.to_string(),
                        }),
                    );
                    // Decoration changes only what is delivered; memory keeps the model's own
                    // words, so the conversation the model sees stays exactly what it said.
                    let mut replies = hooks::decorate_reply(
                        &hosts,
                        &filtered_event,
                        ReplySource::Llm,
                        "",
                        replies,
                    )
                    .await;

                    // The platform adapter turns this into its native quote of the triggering
                    // message; a reply a decorator suppressed gets no lone quote.
                    if quote_reply && !replies.is_empty() {
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
                        split_lines: reply_policy.split_lines,
                    };
                }
                Err(kanon_llm::ToolRouterError::Stopped) => {
                    tracing::info!(
                        session_id = %session_id,
                        "Turn stopped by /stop; nothing is sent for it"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "LLM reasoning and tool execution failed"
                    );
                    // Someone who asked hears that no answer is coming instead of waiting for one.
                    // A turn the bot started on its own (a sampled group message, a notice) fails
                    // quietly: while a provider is down, every such turn would fail, and a notice
                    // for each would flood the group.
                    let asked = notice.is_none()
                        && (!kind.is_policy_governed()
                            || bot_mentioned(filtered_event.metadata.as_ref()));
                    if asked {
                        return PipelineResult::LlmFailed {
                            replies: vec![text_reply(failure_notice(&e))],
                            error: e.to_string(),
                        };
                    }
                }
            }
        }

        PipelineResult::Passed(filtered_event)
    }

    /// Reads the model conversation that answering `event` would continue.
    ///
    /// The session is derived exactly as the model phase derives it — owning instance, group
    /// session scope, `/new` generation — so a plugin reads the same history the model would see.
    /// Read-only by construction: history is append-only and only the pipeline writes it.
    pub async fn conversation_history(
        &self,
        event: &PipelineEventRequest,
    ) -> Result<ConversationHistory, super::conversations::ConversationError> {
        use super::conversations::ConversationError;
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&event.platform).await {
                Ok(Some(instance)) => Some(instance),
                Ok(None) => return Err(ConversationError::NoInstance(event.platform.clone())),
                Err(err) => return Err(ConversationError::Ambiguous(err.to_string())),
            },
            None => None,
        };
        let agent = match &self.agent_factory {
            Some(factory) => {
                factory.agent_for_model(instance.as_ref().and_then(|i| i.model.as_deref()))
            }
            None => self.agent.current(),
        }
        .ok_or(ConversationError::NoModel)?;

        let conversation = conversation_key(event, shares_session(instance.as_ref(), event));
        let session_id = match instance.as_ref() {
            Some(instance) => instance.conversation_session_id(&conversation),
            None => conversation,
        };
        let snapshot = agent
            .memory()
            .snapshot(&session_id)
            .await
            .map_err(|err| ConversationError::Storage(err.to_string()))?;
        Ok(ConversationHistory {
            session_id,
            summary: snapshot.summary,
            messages: snapshot.messages,
        })
    }

    /// Turns a plugin's command (or trigger) response into the pipeline result, recording the
    /// conversation capture the plugin asked for, or hands the message on to the model.
    ///
    /// A transport failure is logged and reported as an unsuccessful execution without replies:
    /// the plugin never answered, so there is nothing truthful to send on its behalf.
    async fn command_result(
        &self,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        command: String,
        plugin_id: &str,
        host: &Arc<crate::supervisor::ManagedHost>,
        event: &PipelineEventRequest,
        outcome: Result<kanon_proto::v1::CommandExecuteResponse, tonic::Status>,
    ) -> CommandFlow {
        match outcome {
            Ok(response) => {
                let pass_to_model = response.pass_to_model && response.capture_seconds == 0;
                if response.pass_to_model && !pass_to_model {
                    tracing::warn!(
                        command = %command,
                        plugin_id = %plugin_id,
                        "Plugin both captured the conversation and passed the message to the model; keeping the capture"
                    );
                }
                if response.capture_seconds > 0
                    && !self.captures.capture(
                        event,
                        &host.host_id,
                        plugin_id,
                        &command,
                        response.capture_seconds,
                    )
                {
                    tracing::warn!(
                        command = %command,
                        plugin_id = %plugin_id,
                        "Plugin asked to capture a conversation whose event names no sender; ignored"
                    );
                }
                let replies = hooks::decorate_reply(
                    hosts,
                    event,
                    ReplySource::Command,
                    &command,
                    response.replies,
                )
                .await;
                if pass_to_model {
                    tracing::debug!(
                        command = %command,
                        plugin_id = %plugin_id,
                        rewritten = response.model_text.is_some(),
                        "Plugin handed the message on to the model"
                    );
                    if !response.success {
                        tracing::warn!(
                            command = %command,
                            plugin_id = %plugin_id,
                            error = %response.error_message,
                            "Plugin reported a failure while handing the message on"
                        );
                    }
                    // The handler's own replies go out first, through the same FIFO the model's
                    // answer will take, so they arrive in the order they were produced.
                    if !replies.is_empty() {
                        self.enqueue_reply(
                            DeliverMessageRequest {
                                platform: event.platform.clone(),
                                channel_id: event.channel_id.clone(),
                                recipient_id: event.sender_id.clone(),
                                segments: replies,
                                event_id: event.event_id.clone(),
                            },
                            false,
                        );
                    }
                    let mut event = event.clone();
                    if let Some(text) = response.model_text {
                        replace_message_text(&mut event, text);
                    }
                    return CommandFlow::PassToModel(event);
                }
                CommandFlow::Finished(PipelineResult::CommandExecuted {
                    command,
                    plugin_id: plugin_id.to_string(),
                    host_id: host.host_id.clone(),
                    success: response.success,
                    replies,
                })
            }
            Err(status) => {
                tracing::error!(
                    command = %command,
                    host_id = %host.host_id,
                    error = %status,
                    "Command execution failed with gRPC status"
                );
                CommandFlow::Finished(PipelineResult::CommandExecuted {
                    command,
                    plugin_id: plugin_id.to_string(),
                    host_id: host.host_id.clone(),
                    success: false,
                    replies: vec![],
                })
            }
        }
    }

    /// Handles the built-in `/stop` command: stops every running model turn of `instance`.
    ///
    /// A stopped turn ends at its next wait on the model or a tool and sends no reply; what it
    /// already did (commands run, files written) stays done.
    fn handle_stop_command(&self, instance: &crate::instance::BotInstance) -> PipelineResult {
        let stopped = self.turns.stop_instance(&instance.id);
        tracing::info!(
            instance_id = %instance.id,
            stopped_turns = stopped,
            "Built-in /stop stopped the instance's running turns"
        );
        let reply = if stopped == 0 {
            "当前没有正在运行的任务。".to_string()
        } else {
            format!("已停止 {stopped} 个正在运行的任务。")
        };
        PipelineResult::BuiltinReplied {
            command: STOP_COMMAND.to_string(),
            replies: vec![text_reply(reply)],
        }
    }

    /// Handles the built-in `/new` command: starts a new, empty conversation for the chat.
    ///
    /// The previous conversation is kept: `/ls` lists it and `/switch` returns to it.
    async fn handle_new_session(
        &self,
        event: &PipelineEventRequest,
        instance: &crate::instance::BotInstance,
    ) -> PipelineResult {
        let chat = super::conversations::Chat {
            instance: instance.clone(),
            conversation: conversation_key(event, shares_session(Some(instance), event)),
        };
        match self.start_conversation(&chat).await {
            Ok(session_id) => {
                tracing::info!(
                    instance_id = %instance.id,
                    session_id = %session_id,
                    conversation = %chat.conversation,
                    "Built-in /new started a new conversation for this chat"
                );
                PipelineResult::SessionRotated {
                    instance_id: instance.id.clone(),
                    session_id,
                    replies: vec![text_reply("已开启新会话。")],
                }
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %instance.id,
                    error = %err,
                    "Built-in /new could not start a conversation; the chat is unchanged"
                );
                PipelineResult::BuiltinReplied {
                    command: NEW_SESSION_COMMAND.to_string(),
                    replies: vec![text_reply(format!("开启新会话失败：{err}"))],
                }
            }
        }
    }

    /// Handles `/ls`, `/switch <n>` and `/del [n]` for the chat that sent `event`.
    ///
    /// Conversations are numbered from 1 in `/ls` order (oldest first), and `/switch` and `/del`
    /// take those numbers. Every outcome, failures included, is answered in the chat.
    async fn handle_conversation_command(
        &self,
        command: &str,
        event: &PipelineEventRequest,
        instance: &crate::instance::BotInstance,
        args: &[String],
    ) -> PipelineResult {
        let chat = super::conversations::Chat {
            instance: instance.clone(),
            conversation: conversation_key(event, shares_session(Some(instance), event)),
        };
        let reply = match self.conversation_command_reply(command, &chat, args).await {
            Ok(reply) => reply,
            Err(err) => {
                tracing::warn!(
                    instance_id = %instance.id,
                    command = %command,
                    error = %err,
                    "Built-in conversation command failed"
                );
                match err {
                    super::conversations::ConversationError::Busy(_) => {
                        "该会话正在运行任务，请稍后再试或先发送 /stop。".to_string()
                    }
                    other => format!("操作失败：{other}"),
                }
            }
        };
        PipelineResult::BuiltinReplied {
            command: command.to_string(),
            replies: vec![text_reply(reply)],
        }
    }

    /// The chat reply of one conversation command.
    async fn conversation_command_reply(
        &self,
        command: &str,
        chat: &super::conversations::Chat,
        args: &[String],
    ) -> Result<String, super::conversations::ConversationError> {
        let conversations = self.chat_conversations(chat).await?;
        if command == LIST_SESSIONS_COMMAND {
            return Ok(render_conversation_list(&conversations));
        }
        // `/del` without a number means the current conversation; `/switch` needs one.
        let index = match args.first() {
            Some(arg) => match arg.trim().parse::<usize>() {
                Ok(index) if (1..=conversations.len()).contains(&index) => Some(index),
                _ => {
                    return Ok(format!(
                        "没有第 {} 个会话（共 {} 个），发送 /ls 查看序号。",
                        arg.trim(),
                        conversations.len()
                    ));
                }
            },
            None => None,
        };
        if command == SWITCH_SESSION_COMMAND {
            let Some(index) = index else {
                return Ok("用法：/switch <序号>，序号见 /ls。".to_string());
            };
            let target = &conversations[index - 1];
            if target.current {
                return Ok(format!("会话 {index} 已是当前会话。"));
            }
            self.switch_conversation(chat, &target.session_id).await?;
            return Ok(format!(
                "已切换到会话 {index}：{}",
                display_title(&target.title)
            ));
        }
        let index = index.unwrap_or_else(|| {
            conversations
                .iter()
                .position(|conversation| conversation.current)
                .map_or(1, |position| position + 1)
        });
        let target = self
            .delete_conversation(chat, &conversations[index - 1].session_id)
            .await?;
        let mut reply = format!("已删除会话 {index}：{}", display_title(&target.title));
        if target.current {
            reply.push_str("\n已开启新会话。");
        }
        Ok(reply)
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
        rendered.push_str("/ls — 列出本聊天的会话\n");
        rendered.push_str("/switch <序号> — 切换到另一个会话\n");
        rendered.push_str("/del [序号] — 删除会话（默认当前会话）\n");
        rendered.push_str("/model — 列出可用模型；/model <序号> 切换当前实例模型\n");
        rendered.push_str("/stop — 停止当前实例正在运行的任务\n");
        rendered.push_str("/help — 显示本帮助\n");
        rendered.push_str("/info — 显示系统与运行信息\n");

        // Command names are deduplicated: two plugins claiming the same name would otherwise show
        // up twice, while routing already resolves that collision deterministically.
        let mut plugin_commands: Vec<(String, String, String, Vec<String>, Vec<CommandMeta>)> =
            Vec::new();
        let mut triggers: Vec<(String, String)> = Vec::new();
        for host in hosts {
            for plugin in host.metas() {
                for command in &plugin.commands {
                    let name = command.name.trim().trim_start_matches('/').to_string();
                    if name.is_empty() || plugin_commands.iter().any(|(seen, ..)| *seen == name) {
                        continue;
                    }
                    let aliases = command
                        .aliases
                        .iter()
                        .map(|alias| alias.trim().trim_start_matches('/').to_string())
                        .filter(|alias| !alias.is_empty())
                        .collect();
                    plugin_commands.push((
                        name,
                        command.description.trim().to_string(),
                        command.usage.trim().to_string(),
                        aliases,
                        command.subcommands.clone(),
                    ));
                }
                // A trigger without a description is an implementation detail of its plugin, not
                // something a user can usefully be told about.
                for trigger in &plugin.triggers {
                    let description = trigger.description.trim();
                    if !description.is_empty()
                        && !triggers.iter().any(|(name, _)| *name == trigger.name)
                    {
                        triggers.push((trigger.name.clone(), description.to_string()));
                    }
                }
            }
        }

        if !plugin_commands.is_empty() {
            plugin_commands.sort_by(|left, right| left.0.cmp(&right.0));
            rendered.push_str("\n插件指令：\n");
            for (name, description, usage, aliases, subcommands) in plugin_commands {
                rendered.push('/');
                rendered.push_str(&name);
                if !aliases.is_empty() {
                    rendered.push_str("（别名: /");
                    rendered.push_str(&aliases.join(", /"));
                    rendered.push('）');
                }
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
                // A command group lists its subcommands under it, indented, in declaration order:
                // routing still goes to the group (`/name sub args`) and the plugin dispatches.
                // Like a command's, a subcommand's usage is the full line (`/todo add <text>`).
                for sub in &subcommands {
                    let sub_name = sub.name.trim();
                    if sub_name.is_empty() {
                        continue;
                    }
                    rendered.push_str("  ");
                    let sub_usage = sub.usage.trim();
                    if sub_usage.is_empty() {
                        rendered.push('/');
                        rendered.push_str(&name);
                        rendered.push(' ');
                        rendered.push_str(sub_name);
                    } else {
                        rendered.push_str(sub_usage);
                    }
                    let sub_description = sub.description.trim();
                    if !sub_description.is_empty() {
                        rendered.push_str(" — ");
                        rendered.push_str(sub_description);
                    }
                    rendered.push('\n');
                }
            }
        }

        if !triggers.is_empty() {
            triggers.sort();
            rendered.push_str("\n消息触发：\n");
            for (_, description) in triggers {
                rendered.push_str(&description);
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
                "系统: {} | Kernel: Darwin {} | Arch: {architecture}\n",
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
    /// without being started, and all active events share [`SHUTDOWN_EVENT_GRACE`] to finish
    /// before it is recorded as well. Nothing the node acknowledged disappears silently.
    ///
    /// # Scheduling
    /// Each chat runs in arrival order, including commands that change its current session.
    /// Distinct chats have bounded concurrency. `/stop` bypasses waiting chat lanes so it can
    /// interrupt the turn it addresses. Read-ahead is bounded by the ingest queue capacity;
    /// excess accepted events go to dead letter so a full queue cannot trap a later `/stop`.
    pub async fn run_worker_loop(&self, mut event_receiver: mpsc::Receiver<IngestEventRequest>) {
        let mut phase = self.shutdown.subscribe();
        let draining = |p| p != ShutdownPhase::Running;
        let mut waiting: VecDeque<(String, IngestEventRequest)> = VecDeque::new();
        let mut active = HashMap::<String, Option<PipelineEventRequest>>::new();
        let mut running: FuturesUnordered<BoxFuture<'_, String>> = FuturesUnordered::new();
        let read_ahead = event_receiver.max_capacity();
        let mut queue_open = true;

        loop {
            if draining(*phase.borrow()) {
                break;
            }
            // A lane key excludes the generation: /new and /switch must finish before the next
            // event resolves the live session. Removing only eligible entries preserves FIFO
            // within a lane while allowing a different chat past a blocked one.
            while running.len() < MAX_CONCURRENT_CHATS {
                let Some(index) = waiting
                    .iter()
                    .position(|(key, _)| !active.contains_key(key))
                else {
                    break;
                };
                let (key, req) = waiting.remove(index).expect("eligible queued chat");
                active.insert(key.clone(), req.event.clone());
                running.push(Box::pin(async move {
                    self.handle_ingested(req).await;
                    key
                }));
            }
            if !queue_open && waiting.is_empty() && running.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                _ = wait_for_phase(&mut phase, draining) => break,
                Some(key) = running.next(), if !running.is_empty() => { active.remove(&key); }
                req = event_receiver.recv(), if queue_open => {
                    match req {
                        Some(req) if is_stop_request(&req) => self.handle_ingested(req).await,
                        Some(req) if waiting.len() >= read_ahead => {
                            // Continue inspecting ingress for /stop even when chat lanes are full.
                            // Accepted overload is recorded explicitly instead of growing an
                            // unbounded staging queue or trapping stop behind a hung model.
                            if let Some(event) = req.event {
                                self.dead_letter_event(&event, "inbound chat waiting queue is full").await;
                            }
                        }
                        Some(req) => {
                            let key = match req.event.as_ref() {
                                Some(event) => match self.resolve_chat(event).await {
                                    Ok(chat) => format!("{}:{}", chat.instance.id, chat.conversation),
                                    Err(_) => conversation_key(event, false),
                                },
                                None => String::new(),
                            };
                            waiting.push_back((key, req));
                        }
                        None => queue_open = false,
                    }
                }
            }
        }

        // All running lanes share one deadline; N hung providers never multiply shutdown grace.
        let deadline = tokio::time::Instant::now() + SHUTDOWN_EVENT_GRACE;
        self.spill_queued_events(&mut waiting, &mut event_receiver)
            .await;
        while !running.is_empty() {
            match tokio::time::timeout_at(deadline, running.next()).await {
                Ok(Some(key)) => {
                    active.remove(&key);
                }
                _ => break,
            }
        }
        // Drop futures before recording their events so no canceled lane can append a late reply.
        drop(running);
        for event in active.into_values().flatten() {
            self.dead_letter_event(
                &event,
                "node shut down while the event was being processed; its reply may be missing",
            )
            .await;
        }
        tracing::info!("Pipeline worker loop terminated");
    }

    /// Closes the ingest queue and records every event not yet started as a dead letter: first
    /// those already read ahead, then those still queued, so the log keeps arrival order.
    async fn spill_queued_events(
        &self,
        waiting: &mut VecDeque<(String, IngestEventRequest)>,
        event_receiver: &mut mpsc::Receiver<IngestEventRequest>,
    ) {
        event_receiver.close();
        for (_, req) in waiting.drain(..) {
            if let Some(event) = req.event {
                self.dead_letter_event(&event, "node shut down before the event was processed")
                    .await;
            }
        }
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
            PipelineResult::LlmFailed { error, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    error = %error,
                    "Pipeline told the sender the model turn failed"
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
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "reply_policy",
                    reason: reason.clone(),
                });
            }
            PipelineResult::BuiltinReplied { command, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline answered a built-in informational command"
                );
            }
            PipelineResult::CommandDenied { command, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline refused a command under the command policy"
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
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "notice",
                    reason: outcome.clone(),
                });
            }
            PipelineResult::NoInstance { platform } => {
                // Already logged with the platform in `process_event`; nothing was delivered.
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "no_instance",
                    reason: format!("no enabled bot instance serves platform '{platform}'"),
                });
            }
            PipelineResult::Passed(_) => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    "Pipeline event passed without a reply"
                );
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "nothing_to_say",
                    reason: "the pipeline produced nothing to send".to_string(),
                });
            }
        }

        let replies = result.replies();

        let split_lines = matches!(
            &result,
            PipelineResult::LlmReplied {
                split_lines: true,
                ..
            }
        );

        if !replies.is_empty() {
            self.enqueue_reply(
                DeliverMessageRequest {
                    platform,
                    channel_id,
                    recipient_id,
                    segments: replies.to_vec(),
                    event_id,
                },
                split_lines,
            );
        }
    }

    /// Hands a reply to the outbound queue without waiting.
    ///
    /// The pipeline worker must never await platform I/O; a full or closed queue sends the reply
    /// to the dead-letter log instead, written off the worker.
    fn enqueue_reply(&self, deliver_req: DeliverMessageRequest, split_lines: bool) {
        let event_id = deliver_req.event_id.clone();
        let platform = deliver_req.platform.clone();
        let channel_id = deliver_req.channel_id.clone();
        let segment_count = deliver_req.segments.len();

        // Non-blocking hand-off: the pipeline worker must never await platform I/O.
        match self.outbound_sender.try_send(OutboundMessage {
            request: deliver_req,
            split_lines,
            receipt: None,
        }) {
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

    /// Shuts the pipeline down without losing anything it already accepted.
    ///
    /// Inbound first: the worker closes the ingest queue, records queued events and gets
    /// one shared [`SHUTDOWN_EVENT_GRACE`] for active events, whose replies still reach the outbound
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
