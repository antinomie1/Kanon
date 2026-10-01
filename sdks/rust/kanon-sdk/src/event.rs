//! Event objects handed to [`Router`](crate::router::Router) handlers, and multi-turn
//! conversations.
//!
//! Handlers receive a [`MessageEvent`] (or a [`CommandEvent`], which dereferences to one)
//! instead of raw requests. The event knows the conversation it came from, so a handler can
//! answer with `event.reply(..)` and ask a follow-up question with `event.wait_next(..)`.
//!
//! # How `wait_next` works
//!
//! Core processes messages one at a time and never blocks waiting for a plugin to "hear back"
//! from a user, so a handler cannot sleep inside one `OnExecuteCommand` call until the next
//! message arrives. Instead:
//!
//! 1. The handler runs as its own task. The RPC that started it waits for the handler's next
//!    *yield point*: either it finishes, or it calls `wait_next`.
//! 2. `wait_next(timeout)` ends the current RPC, returning the replies gathered so far together
//!    with `capture_seconds = timeout`. Core then routes the same sender's next message in that
//!    channel back to this plugin as a *continuation*.
//! 3. The continuation RPC resumes the suspended handler with the new event and again waits for
//!    its next yield point.
//!
//! Replies made while an RPC is waiting are returned in that RPC's response; replies made when
//! none is waiting (after a `wait_next` timed out, or from a spawned task) are delivered through
//! `ReplyMessage` instead.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DeliverMessageResponse, ImageSegment,
    MessageSegment, PipelineEventRequest,
};
use tokio::sync::oneshot;

use crate::context::CoreHandle;
use crate::plugin::PluginResult;
use crate::segment::IntoReply;

/// Longest capture Core honours, in seconds.
pub const MAX_WAIT_SECONDS: u64 = 600;

/// Extra time a suspended handler stays alive after its capture window, so a message Core
/// routed at the last moment still finds the handler waiting.
const WAIT_GRACE: Duration = Duration::from_secs(5);

/// Conversation key Core captures on: platform, channel and sender.
pub type ConversationKey = (String, String, String);

/// An inbound platform message (or notice) as a plugin sees it.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    raw: PipelineEventRequest,
    core: Option<CoreHandle>,
}

impl MessageEvent {
    /// Wraps a pipeline event; `core` is `None` in standalone mode.
    pub fn new(raw: PipelineEventRequest, core: Option<CoreHandle>) -> Self {
        Self { raw, core }
    }

    /// The underlying request.
    pub fn raw(&self) -> &PipelineEventRequest {
        &self.raw
    }

    /// The host's Core handle, `None` in standalone mode.
    pub fn core(&self) -> Option<&CoreHandle> {
        self.core.as_ref()
    }

    /// Platform-qualified id of this message; quote it with [`segment::quote`](crate::segment::quote).
    pub fn event_id(&self) -> &str {
        &self.raw.event_id
    }

    /// Platform the message arrived on.
    pub fn platform(&self) -> &str {
        &self.raw.platform
    }

    /// Conversation the message belongs to, e.g. `group:123` or `private:456`.
    pub fn channel_id(&self) -> &str {
        &self.raw.channel_id
    }

    /// Platform id of the author.
    pub fn sender_id(&self) -> &str {
        &self.raw.sender_id
    }

    /// The message's plain text.
    pub fn text(&self) -> &str {
        &self.raw.raw_text
    }

    /// The message's typed segments (text, images, mentions, quotes, ...).
    pub fn segments(&self) -> &[MessageSegment] {
        &self.raw.segments
    }

    /// One adapter metadata entry (keys such as `kanon.sender_name`).
    pub fn metadata(&self, key: &str) -> Option<&prost_types::Value> {
        self.raw.metadata.as_ref()?.fields.get(key)
    }

    fn metadata_str(&self, key: &str) -> &str {
        match self.metadata(key).and_then(|value| value.kind.as_ref()) {
            Some(prost_types::value::Kind::StringValue(text)) => text,
            _ => "",
        }
    }

    /// Display name of the author, when the platform reports one.
    pub fn sender_name(&self) -> &str {
        self.metadata_str("kanon.sender_name")
    }

    /// `owner`, `admin` or `member` in groups whose platform reports roles.
    pub fn sender_role(&self) -> &str {
        self.metadata_str("kanon.sender_role")
    }

    /// Whether the message was posted in a group conversation.
    pub fn is_group(&self) -> bool {
        self.metadata_str("kanon.conversation_kind") == "group"
    }

    /// Whether the message @-mentions the bot.
    pub fn bot_mentioned(&self) -> bool {
        matches!(
            self.metadata("kanon.bot_mentioned")
                .and_then(|value| value.kind.as_ref()),
            Some(prost_types::value::Kind::BoolValue(true))
        )
    }

    /// Notice kind (`poke`, `member_join`, ...) or `""` for an ordinary message.
    pub fn notice(&self) -> &str {
        self.metadata_str("kanon.notice")
    }

    /// Image segments of the message, in order.
    pub fn images(&self) -> Vec<&ImageSegment> {
        self.raw
            .segments
            .iter()
            .filter_map(|segment| match &segment.segment {
                Some(Segment::Image(image)) => Some(image),
                _ => None,
            })
            .collect()
    }

    /// The key Core uses for captures.
    pub fn conversation_key(&self) -> ConversationKey {
        (
            self.raw.platform.clone(),
            self.raw.channel_id.clone(),
            self.raw.sender_id.clone(),
        )
    }

    /// Sends a message to this conversation right away and waits for delivery.
    ///
    /// Use this for progress notes during long work. Fails in standalone mode, where there is
    /// no Core to deliver through.
    pub async fn send(&self, content: impl IntoReply) -> PluginResult<DeliverMessageResponse> {
        let core = self
            .core
            .as_ref()
            .ok_or("no Core connection: cannot send messages in standalone mode")?;
        Ok(core.reply_to(&self.raw, content).await?)
    }
}

/// Returned by [`CommandEvent::wait_next`] when no answer arrived: the sender did not answer in
/// time, or a newer `wait_next` in the same conversation replaced this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitTimeout;

impl std::fmt::Display for WaitTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sender did not answer in time")
    }
}

impl std::error::Error for WaitTimeout {}

/// One RPC waiting for the handler's next yield point, and the replies it will carry.
struct Turn {
    replies: Vec<MessageSegment>,
    /// Set by [`CommandEvent::pass_to_model`]: `Some(None)` hands the message on unchanged,
    /// `Some(Some(text))` with its text replaced.
    pass: Option<Option<String>>,
    done: oneshot::Sender<CommandExecuteResponse>,
}

/// A running command handler and the RPC currently waiting on it, if any.
pub(crate) struct Session {
    turn: Mutex<Option<Turn>>,
    conversations: Conversations,
}

impl Session {
    pub(crate) fn new(conversations: Conversations) -> Arc<Self> {
        Arc::new(Self {
            turn: Mutex::new(None),
            conversations,
        })
    }

    /// Opens a turn; the receiver resolves with the command response once the handler yields.
    pub(crate) fn open_turn(&self) -> oneshot::Receiver<CommandExecuteResponse> {
        let (done, receiver) = oneshot::channel();
        *self.lock() = Some(Turn {
            replies: Vec::new(),
            pass: None,
            done,
        });
        receiver
    }

    /// Discards the open turn without answering it.
    pub(crate) fn abandon_turn(&self) {
        self.lock().take();
    }

    /// Ends the open turn, if any; the waiting RPC answers with the replies gathered so far.
    ///
    /// A turn that captures the conversation never hands its message on: the handler is waiting
    /// for the next message, so this one is not the model's.
    pub(crate) fn finish(&self, capture_seconds: u32, success: bool, error: String) {
        if let Some(turn) = self.lock().take() {
            let pass = turn.pass.filter(|_| capture_seconds == 0);
            // The RPC may have been cancelled meanwhile; then nobody needs the answer.
            let _ = turn.done.send(CommandExecuteResponse {
                success,
                replies: turn.replies,
                error_message: error,
                capture_seconds,
                pass_to_model: pass.is_some(),
                model_text: pass.flatten(),
            });
        }
    }

    /// Marks the open turn as handing its message on to the model.
    fn pass(&self, text: Option<String>) -> Result<(), ()> {
        match self.lock().as_mut() {
            Some(turn) => {
                turn.pass = Some(text);
                Ok(())
            }
            None => Err(()),
        }
    }

    /// Whether a turn is open.
    pub(crate) fn has_turn(&self) -> bool {
        self.lock().is_some()
    }

    /// Adds replies to the open turn, or hands them back when none is open.
    fn push(&self, segments: Vec<MessageSegment>) -> Result<(), Vec<MessageSegment>> {
        match self.lock().as_mut() {
            Some(turn) => {
                turn.replies.extend(segments);
                Ok(())
            }
            None => Err(segments),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Turn>> {
        // A poisoned lock only means another handler panicked mid-push; the turn data is still
        // consistent (a Vec and a sender), so keep going rather than wedging the conversation.
        self.turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A suspended handler waiting for its conversation's next message.
pub(crate) struct Waiter {
    id: u64,
    pub(crate) session: Arc<Session>,
    pub(crate) resume: oneshot::Sender<CommandEvent>,
}

/// Suspended command handlers, keyed by the conversation Core will route back.
#[derive(Clone, Default)]
pub(crate) struct Conversations {
    waiting: Arc<Mutex<HashMap<ConversationKey, Waiter>>>,
}

/// Distinguishes waits on the same conversation, so a finished wait never removes a newer one.
static NEXT_WAIT_ID: AtomicU64 = AtomicU64::new(1);

impl Conversations {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConversationKey, Waiter>> {
        self.waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers a waiting handler. Core keeps one capture per conversation, so a newer wait
    /// replaces an older one; dropping the older sender wakes that handler with [`WaitTimeout`]
    /// instead of leaving it hanging until its timeout.
    fn suspend(&self, key: ConversationKey, waiter: Waiter) {
        self.lock().insert(key, waiter);
    }

    fn forget(&self, key: &ConversationKey, id: u64) {
        let mut waiting = self.lock();
        if waiting.get(key).is_some_and(|waiter| waiter.id == id) {
            waiting.remove(key);
        }
    }

    /// Removes and returns the handler waiting for `key`, if it still is.
    pub(crate) fn take(&self, key: &ConversationKey) -> Option<Waiter> {
        self.lock()
            .remove(key)
            .filter(|waiter| !waiter.resume.is_closed())
    }
}

/// A message that invoked a command or trigger, or continued a conversation.
///
/// Dereferences to the [`MessageEvent`] it carries.
#[derive(Clone)]
pub struct CommandEvent {
    message: MessageEvent,
    request: CommandExecuteRequest,
    session: Option<Arc<Session>>,
}

impl Deref for CommandEvent {
    type Target = MessageEvent;

    fn deref(&self) -> &MessageEvent {
        &self.message
    }
}

impl CommandEvent {
    pub(crate) fn new(
        request: CommandExecuteRequest,
        core: Option<CoreHandle>,
        session: Option<Arc<Session>>,
    ) -> Self {
        let message = MessageEvent::new(request.context.clone().unwrap_or_default(), core);
        Self {
            message,
            request,
            session,
        }
    }

    /// The underlying request.
    pub fn request(&self) -> &CommandExecuteRequest {
        &self.request
    }

    /// The message as a plain [`MessageEvent`].
    pub fn message(&self) -> &MessageEvent {
        &self.message
    }

    /// Canonical command (or trigger) name.
    pub fn command(&self) -> &str {
        &self.request.command
    }

    /// Arguments split on whitespace, quotes respected. For a trigger, its regex groups.
    pub fn args(&self) -> &[String] {
        &self.request.args
    }

    /// Everything after the command name, unsplit.
    pub fn raw_args(&self) -> &str {
        &self.request.raw_args
    }

    /// Whether this message answers an earlier `wait_next`/capture.
    pub fn continuation(&self) -> bool {
        self.request.continuation
    }

    /// Answers this message.
    ///
    /// While Core is waiting on this handler, replies are collected and sent together as the
    /// command's answer; otherwise they are delivered immediately.
    pub async fn reply(&self, content: impl IntoReply) -> PluginResult<()> {
        let segments = content.into_segments();
        let unsent = match &self.session {
            Some(session) => session.push(segments),
            None => Err(segments),
        };
        if let Err(segments) = unsent {
            self.message.send(segments).await?;
        }
        Ok(())
    }

    /// Hands this message on to the model once the handler returns, as if no command or
    /// trigger had matched it. Replies made in this turn are delivered first; the reply policy
    /// and the model then decide whether the bot answers.
    ///
    /// Fails when Core is no longer waiting on this handler (after a `wait_next` timed out), as
    /// the message has then already been handled. A later `wait_next` in the same turn cancels
    /// the hand-off.
    pub fn pass_to_model(&self) -> PluginResult<()> {
        self.mark_pass(None)
    }

    /// Like [`pass_to_model`](Self::pass_to_model), but the model reads `text` instead of the
    /// message's own text. Images and other segments are kept.
    pub fn pass_to_model_as(&self, text: impl Into<String>) -> PluginResult<()> {
        self.mark_pass(Some(text.into()))
    }

    fn mark_pass(&self, text: Option<String>) -> PluginResult<()> {
        self.session
            .as_ref()
            .ok_or(())
            .and_then(|session| session.pass(text))
            .map_err(|()| {
                "Core is no longer waiting on this message; it cannot be passed on".into()
            })
    }

    /// Ends this turn and waits for the same sender's next message in this conversation.
    ///
    /// Replies made so far are sent first. The next message skips commands and the model and
    /// comes back here as a new `CommandEvent` with [`continuation`](Self::continuation) set.
    /// `timeout` is capped at [`MAX_WAIT_SECONDS`]. After a [`WaitTimeout`] the handler may
    /// still `reply`; those replies are delivered on their own.
    pub async fn wait_next(&self, timeout: Duration) -> Result<CommandEvent, WaitTimeout> {
        let Some(session) = &self.session else {
            // Not dispatched as a command: there is no conversation to suspend.
            return Err(WaitTimeout);
        };
        let seconds = timeout.as_secs().clamp(1, MAX_WAIT_SECONDS);
        let key = self.conversation_key();
        let id = NEXT_WAIT_ID.fetch_add(1, Ordering::Relaxed);
        let (resume, next) = oneshot::channel();
        session.conversations.suspend(
            key.clone(),
            Waiter {
                id,
                session: session.clone(),
                resume,
            },
        );

        // Hand the turn back to Core: its RPC returns now, asking for the capture.
        session.finish(seconds as u32, true, String::new());

        let outcome = tokio::time::timeout(Duration::from_secs(seconds) + WAIT_GRACE, next).await;
        session.conversations.forget(&key, id);
        match outcome {
            Ok(Ok(event)) => Ok(event),
            // Timed out, or the sender was dropped because a newer wait replaced this one.
            _ => Err(WaitTimeout),
        }
    }
}
