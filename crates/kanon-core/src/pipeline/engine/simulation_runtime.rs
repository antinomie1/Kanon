//! Bounded participation: ingress keeps running while one owner observes, speaks and listens.
//!
//! The worker owns every future, including the listener. No detached model task can outlive
//! shutdown. A mailbox holds only post-filter messages; commands retain their FIFO lane barrier.

use super::*;
use crate::adapter::ConversationDeliveryLimits;
use crate::conversation::{ContextPolicy, ReplyPolicy};
use crate::instance::BotInstance;
use crate::simulation::{ConversationMode, SIMULATION_PROTOCOL, SimulationPolicy};
use async_trait::async_trait;
use kanon_llm::tool_router::{ToolHost, json_to_prost_struct, prost_struct_to_json};
use kanon_llm::{ChatMessage, ContentPart, StopSignal, TurnOptions};
use kanon_proto::v1::{
    PluginMeta, ToolCallRequest, ToolCallResponse, ToolMeta, tool_call_request, tool_call_response,
};
use std::collections::HashSet;
use std::sync::Mutex as StdMutex;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

const INBOX_CAPACITY: usize = 64;

/// Active mailboxes, retained only for the lifetime of a participation.
#[derive(Default)]
pub(super) struct SimulationHub {
    inboxes: StdMutex<HashMap<String, Arc<Inbox>>>,
    /// Wakes the worker when a blocked lane can admit a follow-up.
    pub(super) changed: Notify,
}

#[derive(Default)]
struct Inbox {
    pending: StdMutex<VecDeque<PipelineEventRequest>>,
    /// Kept until the agent acknowledges the batch, so a canceled preparation is recoverable.
    in_flight: StdMutex<Vec<PipelineEventRequest>>,
    changed: Notify,
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Preparation has no history to repair yet and can be canceled at once. Model execution
/// instead uses the agent's stop signal, which closes outstanding tool-call results.
async fn prepare_until<F: std::future::Future>(
    future: F,
    signal: &StopSignal,
    deadline: Instant,
) -> Result<F::Output, kanon_llm::ToolRouterError> {
    tokio::select! {
        biased;
        () = signal.stopped() => Err(kanon_llm::ToolRouterError::Stopped),
        () = tokio::time::sleep_until(deadline) => Err(kanon_llm::ToolRouterError::Stopped),
        output = future => Ok(output),
    }
}

impl SimulationHub {
    /// Whether a lane owns a live mailbox and can accept ordered follow-ups.
    pub(super) fn active(&self, key: &str) -> bool {
        lock(&self.inboxes).contains_key(key)
    }

    /// Atomically assigns one owner and deposits an event; no await can reorder deposits.
    fn offer(
        &self,
        key: &str,
        event: PipelineEventRequest,
        wake: bool,
    ) -> Result<Option<(Arc<Inbox>, bool)>, &'static str> {
        let mut inboxes = lock(&self.inboxes);
        let owner = !inboxes.contains_key(key);
        if owner && !wake {
            return Ok(None);
        }
        let inbox = inboxes.entry(key.into()).or_default().clone();
        let mut pending = lock(&inbox.pending);
        if pending.len() >= INBOX_CAPACITY {
            return Err("simulation inbox is full");
        }
        pending.push_back(event);
        drop(pending);
        inbox.changed.notify_one();
        self.changed.notify_one();
        Ok(Some((inbox, owner)))
    }

    fn finish(&self, key: &str) -> Vec<PipelineEventRequest> {
        let Some(inbox) = lock(&self.inboxes).remove(key) else {
            return Vec::new();
        };
        let mut events: Vec<_> = lock(&inbox.in_flight).drain(..).collect();
        events.extend(lock(&inbox.pending).drain(..));
        events
    }

    /// Events accepted into an owner whose future was canceled during shutdown.
    pub(super) fn drain(&self) -> Vec<PipelineEventRequest> {
        lock(&self.inboxes)
            .drain()
            .flat_map(|(_, inbox)| {
                let mut events: Vec<_> = lock(&inbox.in_flight).drain(..).collect();
                events.extend(lock(&inbox.pending).drain(..));
                events
            })
            .collect()
    }
}

#[derive(Clone, Copy, Default)]
enum NextAction {
    #[default]
    None,
    Wait(u64),
    Leave,
}

#[derive(Default)]
struct Actions {
    next: NextAction,
    sent: usize,
    seen: HashSet<String>,
    per_event: HashMap<String, usize>,
    spoke: bool,
    next_send: Option<Instant>,
}

/// Explicit speech is the commit point: enqueue once and await its delivery receipt.
struct ConversationTools {
    outbound: mpsc::Sender<OutboundMessage>,
    observer: Option<Arc<dyn PipelineObserver>>,
    instances: Arc<InstanceRegistry>,
    instance: BotInstance,
    event: StdMutex<PipelineEventRequest>,
    actions: StdMutex<Actions>,
    delivery_limits: StdMutex<ConversationDeliveryLimits>,
    hosts: Vec<Arc<crate::supervisor::ManagedHost>>,
    signal: StopSignal,
    deadline: Instant,
    session_id: String,
}

impl ConversationTools {
    async fn current(&self) -> Result<(), String> {
        if self.signal.is_stopped() || Instant::now() >= self.deadline {
            return Err("Participation stopped or expired".into());
        }
        let event = lock(&self.event).clone();
        let live = self
            .instances
            .resolve_by_platform(&event.platform)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Instance is disabled or has no route")?;
        if live.id != self.instance.id
            || live.conversation_mode != ConversationMode::Simulation
            || live.conversation_rules != self.instance.conversation_rules
            || live.simulation != self.instance.simulation
            || live.model != self.instance.model
            || live.persona_id != self.instance.persona_id
            || live.system_prompt != self.instance.system_prompt
            || live.plugins != self.instance.plugins
            || live.skills != self.instance.skills
            || live.mcp != self.instance.mcp
            || live.command_policy != self.instance.command_policy
            || live.reply_policy != self.instance.reply_policy
            || live.context_policy != self.instance.context_policy
            || live.conversation_session_id(&instance_conversation_key(&event, Some(&live)))
                != self.session_id
        {
            return Err(
                "Instance configuration or conversation changed; participation ended".into(),
            );
        }
        Ok(())
    }

    async fn send(&self, segments: Vec<MessageSegment>) -> Result<String, String> {
        self.current().await?;
        let event = lock(&self.event).clone();
        let limits = lock(&self.delivery_limits).clone();
        let segments =
            hooks::decorate_reply(&self.hosts, &event, ReplySource::Llm, "", segments).await;
        if segments.is_empty() {
            return Ok("Message suppressed by reply decorator".into());
        }
        let cost = if limits.separate_media {
            let media = segments
                .iter()
                .filter(|segment| {
                    matches!(
                        segment.segment,
                        Some(
                            Segment::Image(_)
                                | Segment::Audio(_)
                                | Segment::Video(_)
                                | Segment::File(_)
                        )
                    )
                })
                .count();
            let text = segments.iter().any(|segment| {
                matches!(&segment.segment,
                Some(Segment::Text(text)) if !text.content.trim().is_empty())
            }) || segments
                .iter()
                .any(|segment| matches!(segment.segment, Some(Segment::Mention(_))));
            (media + usize::from(text)).max(1)
        } else {
            1
        };
        let next_send = lock(&self.actions).next_send;
        if let Some(next_send) = next_send {
            prepare_until(
                tokio::time::sleep_until(next_send),
                &self.signal,
                self.deadline,
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        self.current().await?;
        let delivery_deadline = match limits.expires_at {
            Some(expiry) => {
                let remaining = expiry.duration_since(std::time::SystemTime::now())
                    .map_err(|_| "Platform passive reply window expired; wait for a new incoming message")?;
                self.deadline.min(Instant::now() + remaining)
            }
            None => self.deadline,
        };
        {
            let mut state = lock(&self.actions);
            if cost
                > self
                    .instance
                    .simulation
                    .max_messages
                    .saturating_sub(state.sent)
            {
                return Err("Participation message limit reached, including media".into());
            }
            let used = state
                .per_event
                .get(&event.event_id)
                .copied()
                .unwrap_or_default();
            if cost > limits.max_messages.saturating_sub(used) {
                return Err("Platform reply budget exhausted, including media; wait for a new incoming message".into());
            }
            // Reserve attempts before platform I/O. Failed or uncertain deliveries must never
            // be retried as free messages; changing the quoted target cannot reset this budget.
            state.sent += cost;
            state.per_event.insert(event.event_id.clone(), used + cost);
        }
        if let Some(observer) = &self.observer {
            observer.on_stage(&PipelineStage::LlmReplied {
                event_id: event.event_id.clone(),
                session_id: self.session_id.clone(),
                content_length: segments
                    .iter()
                    .filter_map(|segment| match &segment.segment {
                        Some(Segment::Text(text)) => Some(text.content.chars().count()),
                        _ => None,
                    })
                    .sum(),
            });
        }
        let (receipt, delivered) = oneshot::channel();
        self.outbound
            .try_send(OutboundMessage {
                request: DeliverMessageRequest {
                    platform: event.platform,
                    channel_id: event.channel_id,
                    recipient_id: event.sender_id,
                    event_id: event.event_id,
                    segments,
                },
                split_lines: false,
                receipt: Some(receipt),
            })
            .map_err(|error| format!("Message was not queued: {error}"))?;
        // Once queued it may be delivered even if /stop or shutdown interrupts the receipt.
        // That boundary is deliberately visible in the tool result; never automatically retry.
        let receipt = tokio::select! {
            biased;
            () = self.signal.stopped() => return Err("Stopped after enqueue; delivery is uncertain. Do not retry.".into()),
            receipt = tokio::time::timeout_at(delivery_deadline.min(Instant::now() + Duration::from_secs(30)), delivered) => receipt,
        };
        lock(&self.actions).next_send = Some(Instant::now() + limits.min_interval);
        match receipt {
            Ok(Ok(result)) if result.success => {
                Ok(format!("Delivered message {}", result.message_id))
            }
            Ok(Ok(result)) => Err(format!("Delivery failed: {}", result.error_message)),
            _ => Err(
                "Delivery receipt unavailable; message may have been delivered. Do not retry."
                    .into(),
            ),
        }
    }

    async fn action(&self, request: &ToolCallRequest) -> Result<String, String> {
        self.current().await?;
        let args = match &request.payload {
            Some(tool_call_request::Payload::StructuredArgs(args)) => {
                prost_struct_to_json(args.clone()).map_err(|e| e.to_string())?
            }
            _ => return Err("Expected structured arguments".into()),
        };
        if !matches!(lock(&self.actions).next, NextAction::None) {
            return Err("A wait/leave action is already selected. End this internal turn.".into());
        }
        match request.tool_name.as_str() {
            "conversation_say" => {
                if lock(&self.actions).spoke {
                    return Err("You have already spoken for this batch. Listen or leave; do not split a response into more messages.".into());
                }
                let content = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("text must be nonempty")?;
                let expanded = match args.get("expanded") {
                    Some(value) => value.as_bool().ok_or("expanded must be boolean")?,
                    None => false,
                };
                if !expanded && content.chars().count() > crate::simulation::CASUAL_MESSAGE_CHARS {
                    return Err(format!(
                        "Casual speech exceeds {} characters. Shorten it to one thought; do not split it into multiple calls. expanded=true is only for an explicitly requested detailed answer.",
                        crate::simulation::CASUAL_MESSAGE_CHARS
                    ));
                }
                if content.chars().count() > 8000 {
                    return Err("text exceeds 8000 characters".into());
                }
                let mut segments = vec![text_reply(content)];
                if let Some(id) = args
                    .get("reply_to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    if !lock(&self.actions).seen.contains(id) {
                        return Err(
                            "reply_to must name a message observed in this participation".into(),
                        );
                    }
                    segments.insert(
                        0,
                        MessageSegment {
                            segment: Some(Segment::Reply(kanon_proto::v1::ReplySegment {
                                target_message_id: id.into(),
                                snippet: String::new(),
                            })),
                        },
                    );
                }
                // A failed or uncertain send is still this batch's attempt to speak.
                lock(&self.actions).spoke = true;
                let result = self.send(segments).await?;
                hooks::emit_event(
                    &self.hosts,
                    EventKind::LlmResponse,
                    Detail::LlmResponse(LlmResponseEvent {
                        context: Some(lock(&self.event).clone()),
                        content: content.into(),
                    }),
                );
                Ok(result)
            }
            "conversation_wait" => {
                let seconds = match args.get("seconds") {
                    None => self.instance.simulation.listen_seconds,
                    Some(value) => value.as_u64().ok_or("seconds must be a positive integer")?,
                };
                if seconds == 0 || seconds > self.instance.simulation.listen_seconds {
                    return Err("seconds exceeds the current listen limit".into());
                }
                lock(&self.actions).next = NextAction::Wait(seconds);
                Ok("Listen selected. End this internal turn now; new messages will arrive in the next turn. If nobody speaks, participation ends silently.".into())
            }
            "conversation_leave" => {
                lock(&self.actions).next = NextAction::Leave;
                Ok("Participation ended. End this internal turn now.".into())
            }
            _ => Err("Unknown conversation action".into()),
        }
    }
}

#[async_trait]
impl ToolHost for ConversationTools {
    fn host_id(&self) -> &str {
        "kanon-conversation"
    }
    fn plugin_metas(&self) -> Vec<PluginMeta> {
        let definitions = [
            (
                "conversation_say",
                "Speak once for this batch, only if a contribution is called for. Usually one or two short sentences, at most 120 characters. Use expanded only for explicitly requested detail. Otherwise listen. No automatic retries.",
                serde_json::json!({"type":"object","properties":{"text":{"type":"string"},"reply_to":{"type":"string"},"expanded":{"type":"boolean","description":"Only true if a member explicitly requested a detailed explanation, code or long artifact."}},"required":["text"],"additionalProperties":false}),
            ),
            (
                "conversation_wait",
                "Listen for new messages; end the internal model turn after calling this. An idle timeout ends participation silently.",
                serde_json::json!({"type":"object","properties":{"seconds":{"type":"integer","minimum":1}},"additionalProperties":false}),
            ),
            (
                "conversation_leave",
                "Stop participating without sending anything; end the internal model turn after calling this.",
                serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
        ];
        vec![PluginMeta {
            id: "kanon-conversation".into(),
            tools: definitions
                .into_iter()
                .map(|(name, description, schema)| ToolMeta {
                    name: name.into(),
                    description: description.into(),
                    parameters: json_to_prost_struct(&schema),
                })
                .collect(),
            ..Default::default()
        }]
    }
    async fn call_tool(&self, request: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        let result = self.action(&request).await;
        Ok(match result {
            Ok(message) => ToolCallResponse {
                call_id: request.call_id,
                success: true,
                payload: json_to_prost_struct(&serde_json::json!({"result":message}))
                    .map(tool_call_response::Payload::StructuredResult),
                ..Default::default()
            },
            Err(error_message) => ToolCallResponse {
                call_id: request.call_id,
                error_message,
                ..Default::default()
            },
        })
    }
}

impl PipelineEngine {
    /// Applies the wake policy or joins the existing owner after all ingress filters.
    pub(super) async fn process_simulation(
        &self,
        instance: &BotInstance,
        event: PipelineEventRequest,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        reply: ReplyPolicy,
        context: ContextPolicy,
    ) -> PipelineResult {
        let conversation = instance_conversation_key(&event, Some(instance));
        let key = format!("{}:{conversation}", instance.id);
        let group_key = format!("{}\u{1f}{}", event.platform, event.channel_id);
        let kind = ConversationKind::from_metadata(event.metadata.as_ref());
        let mentioned = bot_mentioned(event.metadata.as_ref());
        let wake = reply.should_reply(kind, mentioned, reply_sample(&event.event_id))
            || (mentioned && reply.mode != crate::conversation::ReplyMode::Never);
        let offered = self.simulation.offer(&key, event.clone(), wake);
        let (inbox, owner) = match offered {
            Ok(Some(value)) => value,
            Ok(None) => {
                if kind.is_policy_governed() {
                    self.group_log.record(
                        &group_key,
                        &super::super::identity::speaker_label(&event, context.include_sender_id),
                        &event.raw_text,
                    );
                }
                return PipelineResult::ReplySuppressed {
                    instance_id: instance.id.clone(),
                    reason: "simulation is observing; wake policy did not match".into(),
                };
            }
            Err(reason) => {
                self.dead_letter_event(&event, reason).await;
                return PipelineResult::ReplySuppressed {
                    instance_id: instance.id.clone(),
                    reason: reason.into(),
                };
            }
        };
        if !owner {
            return PipelineResult::SimulationHandled;
        }
        let result = self
            .participate(
                instance,
                &event,
                hosts,
                &inbox,
                context,
                &group_key,
                &conversation,
            )
            .await;
        for pending in self.simulation.finish(&key) {
            self.dead_letter_event(
                &pending,
                "simulation ended before this accepted message reached the model",
            )
            .await;
        }
        match result {
            Ok(()) | Err(kanon_llm::ToolRouterError::Stopped) => PipelineResult::SimulationHandled,
            Err(error) => {
                tracing::warn!(instance_id = %instance.id, %error, "Simulation participation failed");
                PipelineResult::LlmFailed {
                    error: error.to_string(),
                    // Operational failures remain visible in the console and dead-letter log;
                    // they must not bypass the mode's explicit speech and message limits.
                    replies: Vec::new(),
                }
            }
        }
    }

    async fn participate(
        &self,
        instance: &BotInstance,
        event: &PipelineEventRequest,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        inbox: &Inbox,
        context: ContextPolicy,
        group_key: &str,
        conversation: &str,
    ) -> Result<(), kanon_llm::ToolRouterError> {
        use kanon_llm::ToolRouterError as Error;
        let agent = match &self.agent_factory {
            Some(factory) => factory.agent_for_model(instance.model.as_deref()),
            None => self.agent.current(),
        }
        .ok_or_else(|| Error::InvalidRequest("Simulation requires a configured model".into()))?;
        if !agent.config().tool_calling {
            return Err(Error::InvalidRequest(
                "Simulation requires a model with tool calling enabled".into(),
            ));
        }
        let capabilities = self
            .agent_factory
            .as_ref()
            .map(|factory| {
                factory
                    .models()
                    .settings_for(&ModelRef::parse(&agent.config().model_ref()))
                    .capabilities
            })
            .unwrap_or_default();
        let session_id = instance.conversation_session_id(conversation);
        let ledger_key = format!("{}\u{1f}{conversation}", event.platform);
        let running = self.turns.begin(Some(instance.id.clone()));
        let signal = running.signal();
        let deadline =
            Instant::now() + Duration::from_secs(instance.simulation.max_participation_seconds);
        let writing = if let Some(sessions) = agent.session_manager() {
            Some(tokio::select! {
                biased;
                () = signal.stopped() => return Err(Error::Stopped),
                () = tokio::time::sleep_until(deadline) => return Err(Error::Stopped),
                writing = sessions.write(&session_id) => writing,
            })
        } else {
            None
        };
        let tools = Arc::new(ConversationTools {
            outbound: self.outbound_sender.clone(),
            observer: self.observer.clone(),
            instances: self
                .instances
                .as_ref()
                .expect("simulation requires an instance registry")
                .clone(),
            instance: instance.clone(),
            event: StdMutex::new(event.clone()),
            actions: StdMutex::new(Actions::default()),
            delivery_limits: StdMutex::new(ConversationDeliveryLimits::default()),
            hosts: hosts.to_vec(),
            signal: signal.clone(),
            deadline,
            session_id: session_id.clone(),
        });
        let mut tool_hosts = prepare_until(
            self.tool_hosts(hosts, Some(instance), &event.event_id),
            &signal,
            deadline,
        )
        .await?;
        if tool_hosts
            .iter()
            .flat_map(|host| host.plugin_metas())
            .flat_map(|plugin| plugin.tools)
            .any(|tool| {
                matches!(
                    tool.name.as_str(),
                    "conversation_say" | "conversation_wait" | "conversation_leave"
                )
            })
        {
            return Err(Error::InvalidRequest(
                "An extension uses a reserved conversation action name".into(),
            ));
        }
        tool_hosts.push(tools.clone());
        let route = self
            .supervisor
            .resolve_adapter(&event.platform)
            .await
            .ok_or_else(|| Error::InvalidRequest("Conversation platform is unavailable".into()))?;
        let all_group_messages = route
            .capabilities()
            .contains(&crate::adapter::Capability::GroupMessages);
        let mut first = true;
        loop {
            if let Err(error) = tools.current().await {
                tracing::info!(instance_id = %instance.id, %error, "Simulation participation ended");
                return Err(Error::Stopped);
            }
            if signal.is_stopped() || Instant::now() >= deadline {
                return Err(Error::Stopped);
            }
            // The same writer and stop registration span every listen and internal model turn.
            // No new user message is inserted in the middle of an outstanding tool-call round.
            let batch = self
                .simulation_batch(inbox, &instance.simulation, &signal, deadline)
                .await?;
            if batch.is_empty() {
                return Ok(());
            }
            *lock(&inbox.in_flight) = batch.clone();
            let latest = batch.last().expect("nonempty batch").clone();
            *lock(&tools.event) = latest.clone();
            let delivery_limits = match &route {
                crate::supervisor::AdapterRoute::Builtin(adapter) => adapter
                    .conversation_delivery_limits(&latest)
                    .map_err(|error| Error::InvalidRequest(error.to_string()))?,
                crate::supervisor::AdapterRoute::Plugin { .. } => {
                    ConversationDeliveryLimits::default()
                }
            };
            let turn_deadline = match delivery_limits.expires_at {
                Some(expiry) => deadline.min(
                    Instant::now()
                        + expiry
                            .duration_since(std::time::SystemTime::now())
                            .map_err(|_| {
                                Error::InvalidRequest(
                                    "Platform passive reply window has expired".into(),
                                )
                            })?,
                ),
                None => deadline,
            };
            *lock(&tools.delivery_limits) = delivery_limits.clone();
            let mut parts = vec![ContentPart::text(
                "[Conversation timeline update. Receiving these messages does not itself request a reply. Decide whether to speak or keep listening.]",
            )];
            if first {
                let unseen = self.group_log.unseen(group_key, &ledger_key);
                if !unseen.is_empty() {
                    parts.push(ContentPart::text(format!(
                        "[Earlier group messages]\n{}",
                        unseen
                            .iter()
                            .map(|(who, text)| format!("{who}: {text}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )));
                }
                first = false;
            }
            parts.extend(
                self.recalls
                    .take_notes(&ledger_key)
                    .into_iter()
                    .map(ContentPart::text),
            );
            for item in &batch {
                parts.extend(
                    prepare_until(
                        hooks::prepare_turn(hosts, item, &session_id),
                        &signal,
                        deadline,
                    )
                    .await?
                    .into_iter()
                    .map(ContentPart::text),
                );
                let message = build_user_message(item, &capabilities, &context)
                    .map_err(|e| Error::InvalidRequest(e.to_string()))?;
                parts.push(ContentPart::text(format!(
                    "[Message ID={}, addressed_to_bot={}]",
                    serde_json::Value::String(item.event_id.clone()),
                    bot_mentioned(item.metadata.as_ref())
                        || !ConversationKind::from_metadata(item.metadata.as_ref())
                            .is_policy_governed()
                )));
                if let Some(message_parts) = message.parts {
                    parts.extend(message_parts);
                } else if let Some(text) = message.content {
                    parts.push(ContentPart::text(text));
                }
                lock(&tools.actions).seen.insert(item.event_id.clone());
                self.recalls
                    .record_turn(&item.event_id, &ledger_key, &item.raw_text);
                let seq = self.group_log.record(
                    group_key,
                    &super::super::identity::speaker_label(item, context.include_sender_id),
                    &item.raw_text,
                );
                self.group_log.mark_seen(group_key, &ledger_key, seq);
            }
            let remaining = instance
                .simulation
                .max_messages
                .saturating_sub(lock(&tools.actions).sent);
            let platform_remaining = delivery_limits.max_messages.saturating_sub(
                lock(&tools.actions)
                    .per_event
                    .get(&latest.event_id)
                    .copied()
                    .unwrap_or_default(),
            );
            if ConversationKind::from_metadata(latest.metadata.as_ref()).is_policy_governed()
                && !all_group_messages
            {
                parts.push(ContentPart::text("[Platform visibility: complete group conversation is unavailable. You can only see messages delivered here; silence does not mean nobody is talking. Listening requires another platform-delivered message, which may require mentioning the bot. Do not claim to observe unseen exchanges.]"));
            }
            parts.push(ContentPart::text(format!("[Participation limits: messages remaining={}, maximum listen seconds={}, seconds remaining={}. Casual speech limit=120 characters; at most one spoken contribution for this batch. For unrelated group exchanges choose wait or leave. Tool-generated attachments are sent at the end of this internal turn.]", remaining.min(platform_remaining), instance.simulation.listen_seconds, turn_deadline.saturating_duration_since(Instant::now()).as_secs())));
            if delivery_limits.expires_at.is_some() {
                parts.push(ContentPart::text(format!("[Passive replies only: source message ID={}; native replies remaining={platform_remaining}. Text and each media attachment count separately. A new delivered message supplies its own reply budget and window; selecting reply_to does not. If exhausted, wait for a new message or leave; never retry a failed or uncertain send.]", serde_json::Value::String(latest.event_id.clone()))));
            }
            let mut message = ChatMessage::user(
                parts
                    .iter()
                    .filter_map(ContentPart::as_text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            if parts
                .iter()
                .any(|part| matches!(part, ContentPart::Image { .. }))
            {
                message.parts = Some(parts);
            }
            prepare_until(
                super::super::media::inline_images(&mut message),
                &signal,
                deadline,
            )
            .await?;
            {
                let mut actions = lock(&tools.actions);
                actions.next = NextAction::None;
                actions.spoke = false;
            }
            let turn = self.run_conversation_turn(
                ConversationTurn {
                    agent: kanon_llm::ConversationBackend::Builtin(agent.clone()),
                    running: &running,
                    session_id: &session_id,
                    event: &latest,
                    hosts,
                    tool_hosts: tool_hosts.clone(),
                    bash_caller: None,
                    options: TurnOptions {
                        instructions: Some(SIMULATION_PROTOCOL.into()),
                        ..Default::default()
                    },
                },
                message,
            );
            let turn = async {
                match &writing {
                    Some(writing) => writing.scope(turn).await,
                    None => turn.await,
                }
            };
            tokio::pin!(turn);
            let mut phase = self.shutdown.subscribe();
            let output = tokio::select! {
                result = &mut turn => result,
                () = tokio::time::sleep_until(turn_deadline) => { signal.stop(); turn.await },
                _ = wait_for_phase(&mut phase, |p| p != ShutdownPhase::Running) => { signal.stop(); turn.await },
            }?;
            lock(&inbox.in_flight).clear();
            if !output.attachments.is_empty() {
                let declared = self
                    .supervisor
                    .resolve_adapter(&latest.platform)
                    .await
                    .map(|route| route.capabilities());
                let media = super::super::attachment::attachment_segments(
                    &output.attachments,
                    declared.as_deref(),
                );
                let mut segments = media.segments;
                if !media.notes.is_empty() {
                    segments.push(text_reply(media.notes.join("\n")));
                }
                if !segments.is_empty() {
                    tools.send(segments).await.map_err(Error::ToolFailed)?;
                }
            }
            let (action, sent) = {
                let state = lock(&tools.actions);
                (state.next, state.sent)
            };
            if sent >= instance.simulation.max_messages {
                return Ok(());
            }
            let action = match action {
                NextAction::None if lock(&tools.actions).spoke => {
                    NextAction::Wait(instance.simulation.listen_seconds)
                }
                action => action,
            };
            match action {
                NextAction::None | NextAction::Leave => return Ok(()),
                NextAction::Wait(seconds) => {
                    let until = (Instant::now() + Duration::from_secs(seconds)).min(deadline);
                    loop {
                        // Notify stores one permit, not an event count. Recheck the queue after
                        // every wake so a stale permit cannot end an otherwise valid listen.
                        let changed = inbox.changed.notified();
                        if !lock(&inbox.pending).is_empty() {
                            break;
                        }
                        tokio::select! {
                            biased;
                            () = signal.stopped() => return Err(Error::Stopped),
                            _ = wait_for_phase(&mut phase, |p| p != ShutdownPhase::Running) => return Err(Error::Stopped),
                            () = tokio::time::sleep_until(until) => return Ok(()),
                            () = changed => {},
                        }
                    }
                }
            }
        }
    }

    async fn simulation_batch(
        &self,
        inbox: &Inbox,
        policy: &SimulationPolicy,
        signal: &StopSignal,
        deadline: Instant,
    ) -> Result<Vec<PipelineEventRequest>, kanon_llm::ToolRouterError> {
        let start = Instant::now();
        let until = (start + Duration::from_millis(policy.max_batch_ms)).min(deadline);
        let mut quiet = (start + Duration::from_millis(policy.quiet_ms)).min(until);
        let mut phase = self.shutdown.subscribe();
        loop {
            tokio::select! {
                biased;
                () = signal.stopped() => return Err(kanon_llm::ToolRouterError::Stopped),
                _ = wait_for_phase(&mut phase, |p| p != ShutdownPhase::Running) => return Err(kanon_llm::ToolRouterError::Stopped),
                () = tokio::time::sleep_until(quiet) => break,
                () = inbox.changed.notified() => { quiet = (Instant::now() + Duration::from_millis(policy.quiet_ms)).min(until); },
            }
        }
        Ok(lock(&inbox.pending).drain(..).collect())
    }
}
