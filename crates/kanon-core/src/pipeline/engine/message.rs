//! Conversation identity, message normalization and user-facing command formatting.

use super::*;

/// Replaces the text of a message, keeping its other segments (images, mentions) in place.
///
/// The first text segment takes the new text and the other text segments are dropped, so the
/// model reads the replacement exactly once; a message without text gets the text in front.
pub(super) fn replace_message_text(event: &mut PipelineEventRequest, text: String) {
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
pub(super) const CONVERSATION_COMMANDS: [&str; 4] = [
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

/// Resolves the conversation once for commands, history access, workers and model turns.
/// Simulation has its own platform-qualified namespace; identical group IDs on two adapters
/// must never share a mailbox or transcript. Existing assistant session IDs remain unchanged.
pub(crate) fn instance_conversation_key(
    event: &PipelineEventRequest,
    instance: Option<&crate::instance::BotInstance>,
) -> String {
    let key = conversation_key(event, shares_session(instance, event));
    if instance.is_some_and(|instance| {
        instance.conversation_mode == crate::simulation::ConversationMode::Simulation
    }) {
        format!(
            "simulation:{}:{}:{key}",
            event.platform.len(),
            event.platform
        )
    } else {
        key
    }
}

/// Whether an event's conversation is one session shared by the whole group.
pub(crate) fn shares_session(
    instance: Option<&crate::instance::BotInstance>,
    event: &PipelineEventRequest,
) -> bool {
    instance.is_some_and(|instance| {
        instance.session_scope == SessionScope::Group
            || instance.conversation_mode == crate::simulation::ConversationMode::Simulation
    }) && ConversationKind::from_metadata(event.metadata.as_ref()).is_policy_governed()
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
pub(super) fn kernel_release() -> Option<String> {
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
pub(super) fn distribution_name() -> String {
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
pub(super) fn message_text(event: &PipelineEventRequest) -> String {
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
pub(super) fn is_stop_request(req: &IngestEventRequest) -> bool {
    let Some(event) = req.event.as_ref() else {
        return false;
    };
    if NoticeKind::from_metadata(event.metadata.as_ref()).is_some() {
        return false;
    }
    CommandRouter::parse_command(strip_leading_mentions(&message_text(event)))
        .is_some_and(|command| command.name.eq_ignore_ascii_case(STOP_COMMAND))
}

pub(super) fn strip_leading_mentions(text: &str) -> &str {
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
pub(super) fn text_reply(content: impl Into<String>) -> MessageSegment {
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
pub(super) fn tool_names(executed: &[kanon_llm::ExecutedToolCall]) -> Vec<String> {
    executed.iter().map(|call| call.tool_name.clone()).collect()
}

/// The failure category `AGENT_DONE` reports for a turn that ended without an answer.
///
/// Categories, not messages: an error's text can carry a provider's response body, which a
/// subscriber has no use for and should not receive.
pub(super) fn failure_category(error: &kanon_llm::ToolRouterError) -> &'static str {
    match error {
        kanon_llm::ToolRouterError::Stopped => "stopped",
        #[cfg(feature = "dsh")]
        kanon_llm::ToolRouterError::Dsh(_) => "dsh_error",
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

pub(super) fn failure_notice(error: &kanon_llm::ToolRouterError) -> String {
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
        #[cfg(feature = "dsh")]
        ToolRouterError::Dsh(_) => "DSH 后端执行失败，请查看运行日志".to_string(),
    };
    format!("这次没能回复：{reason}。不会自动重试，可以稍后再发。")
}

/// Renders the `/model` listing as plain text.
pub(super) fn render_model_list(
    options: &[(String, Option<ModelSpec>)],
    current: Option<&str>,
    scope: &str,
) -> String {
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
    rendered.push_str(&format!("回复 /model <序号> 切换当前{scope}模型。"));
    rendered
}

/// A conversation title for a chat reply; an empty conversation has none.
pub(super) fn display_title(title: &str) -> &str {
    if title.is_empty() {
        "（空会话）"
    } else {
        title
    }
}

/// Renders the `/ls` listing: oldest first, numbered from 1, the current one marked.
pub(super) fn render_conversation_list(
    conversations: &[crate::pipeline::conversations::ConversationInfo],
) -> String {
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
pub(super) fn reply_sample(event_id: &str) -> f32 {
    use std::hash::{BuildHasher, Hash, Hasher};

    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    event_id.hash(&mut hasher);
    (hasher.finish() % 10_000) as f32 / 10_000.0
}

/// One agent turn in a conversation, ready to run (see [`PipelineEngine::run_conversation_turn`]).
pub(crate) struct ConversationTurn<'a> {
    /// The agent answering the turn.
    pub(crate) agent: kanon_llm::ConversationBackend,
    /// Registration created before waiting for the session writer, so `/stop` reaches that wait.
    pub(crate) running: &'a crate::pipeline::turns::TurnGuard<'a>,
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
