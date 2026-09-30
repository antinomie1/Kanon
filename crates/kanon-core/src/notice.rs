//! Platform notices — a member joined, the bot was poked, a message was recalled — and the
//! node-wide policy that decides which of them the bot reacts to.
//!
//! # The adapter contract
//! A notice travels as an ordinary [`PipelineEventRequest`] whose metadata carries
//! [`META_NOTICE`] (one of the [`NoticeKind`] names), plus optionally [`META_NOTICE_ACTOR`] (a
//! display name of whoever caused it) and, for a recall, [`META_NOTICE_TARGET`] (the event ID the
//! recalled message had when it was ingested). The channel and sender are those of the
//! conversation the notice belongs to, so a welcome lands in the new member's own conversation
//! and a poke in the poker's. Adapters only report facts; wording and policy live here and in the
//! context builder.
//!
//! # Recalls never leak content
//! A recall only produces a note when the recalled message already reached the model, and the note
//! is attached to the next turn of that same conversation. Content the model never saw is not
//! revealed to it just because somebody deleted it.
//!
//! [`PipelineEventRequest`]: kanon_proto::v1::PipelineEventRequest

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use kanon_proto::prost_types;
use serde::{Deserialize, Serialize};

use crate::conversation::default_true;

/// Metadata key naming the notice an event reports.
pub const META_NOTICE: &str = "kanon.notice";

/// Metadata key carrying a display name (or ID) of whoever caused the notice.
pub const META_NOTICE_ACTOR: &str = "kanon.notice_actor";

/// Metadata key carrying the event ID of the message a recall removed.
pub const META_NOTICE_TARGET: &str = "kanon.notice_target";

/// How many answered messages are remembered as possible recall targets.
const LEDGER_CAPACITY: usize = 1024;

/// How many recall notes wait per conversation; older ones are dropped first.
const NOTES_PER_CONVERSATION: usize = 5;

/// Characters of a recalled message quoted in its note, enough to identify it.
const EXCERPT_CHARS: usize = 40;

/// A platform event that is not a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Someone joined a group the bot is in.
    MemberJoin,
    /// The bot itself was added to a group.
    BotJoin,
    /// Someone added the bot as a friend.
    FriendAdd,
    /// Someone poked (nudged) the bot.
    Poke,
    /// A message was recalled.
    Recall,
}

impl NoticeKind {
    /// Canonical metadata value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemberJoin => "member_join",
            Self::BotJoin => "bot_join",
            Self::FriendAdd => "friend_add",
            Self::Poke => "poke",
            Self::Recall => "recall",
        }
    }

    /// Reads the notice an adapter reported; `None` for an ordinary message.
    pub fn from_metadata(metadata: Option<&prost_types::Struct>) -> Option<Self> {
        match metadata_str(metadata, META_NOTICE)? {
            "member_join" => Some(Self::MemberJoin),
            "bot_join" => Some(Self::BotJoin),
            "friend_add" => Some(Self::FriendAdd),
            "poke" => Some(Self::Poke),
            "recall" => Some(Self::Recall),
            _ => None,
        }
    }
}

/// Reads a non-empty string field from event metadata.
pub fn metadata_str<'a>(metadata: Option<&'a prost_types::Struct>, key: &str) -> Option<&'a str> {
    match metadata?.fields.get(key)?.kind.as_ref()? {
        prost_types::value::Kind::StringValue(text) if !text.trim().is_empty() => Some(text.trim()),
        _ => None,
    }
}

/// Which notices the bot reacts to, node-wide.
///
/// Every reaction defaults to off: a bot that suddenly greets every newcomer in a large group is a
/// surprise an operator must opt into. Recall notes default to on because they only correct what
/// the model already knows and are never visible in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPolicy {
    /// Welcome a member who joined a group.
    #[serde(default)]
    pub welcome_members: bool,
    /// Say hello when the bot is added to a group or as a friend.
    #[serde(default)]
    pub greet_on_join: bool,
    /// Answer when somebody pokes the bot.
    #[serde(default)]
    pub reply_to_poke: bool,
    /// Tell the model, on its next turn, that a message it saw was recalled.
    #[serde(default = "default_true")]
    pub note_recalls: bool,
}

impl Default for EventPolicy {
    fn default() -> Self {
        Self {
            welcome_members: false,
            greet_on_join: false,
            reply_to_poke: false,
            note_recalls: true,
        }
    }
}

impl EventPolicy {
    /// Whether the bot answers this kind of notice.
    pub fn answers(&self, kind: NoticeKind) -> bool {
        match kind {
            NoticeKind::MemberJoin => self.welcome_members,
            NoticeKind::BotJoin | NoticeKind::FriendAdd => self.greet_on_join,
            NoticeKind::Poke => self.reply_to_poke,
            // A recall is never answered; it can only become a note.
            NoticeKind::Recall => false,
        }
    }
}

/// Hot-swappable node-wide event policy, mirroring the reply and context policy stores.
#[derive(Debug, Default)]
pub struct EventPolicyStore {
    current: std::sync::RwLock<EventPolicy>,
}

impl EventPolicyStore {
    /// Creates a store holding an initial policy.
    pub fn new(policy: EventPolicy) -> Self {
        Self {
            current: std::sync::RwLock::new(policy),
        }
    }

    /// Returns the current policy.
    pub fn get(&self) -> EventPolicy {
        *self
            .current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replaces the policy.
    pub fn set(&self, policy: EventPolicy) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }
}

/// Messages the model answered, and recall notes waiting for their conversation's next turn.
///
/// In memory and bounded: after a restart an old recall simply produces no note, which is the
/// same outcome as a recall of something the model never saw.
#[derive(Debug, Default)]
pub struct RecallLedger {
    seen: Mutex<Seen>,
    notes: Mutex<HashMap<String, VecDeque<String>>>,
}

#[derive(Debug, Default)]
struct Seen {
    /// Event ID to (conversation key, excerpt).
    entries: HashMap<String, (String, String)>,
    /// Insertion order for eviction.
    order: VecDeque<String>,
}

impl RecallLedger {
    /// Records that the message `event_id` of `conversation` reached the model.
    pub fn record_turn(&self, event_id: &str, conversation: &str, text: &str) {
        if event_id.is_empty() {
            return;
        }
        let excerpt: String = text.trim().chars().take(EXCERPT_CHARS).collect();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen
            .entries
            .insert(event_id.to_owned(), (conversation.to_owned(), excerpt))
            .is_none()
        {
            seen.order.push_back(event_id.to_owned());
        }
        while seen.order.len() > LEDGER_CAPACITY {
            if let Some(oldest) = seen.order.pop_front() {
                seen.entries.remove(&oldest);
            }
        }
    }

    /// Queues a note for the recalled message when the model saw it; returns whether it did.
    pub fn note_recall(&self, target_event_id: &str, actor: Option<&str>) -> bool {
        let Some((conversation, excerpt)) = self
            .seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .remove(target_event_id)
        else {
            return false;
        };
        let who = actor.unwrap_or("对方");
        let note = if excerpt.is_empty() {
            format!("[通知] {who}撤回了之前的一条消息")
        } else {
            format!("[通知] {who}撤回了之前的消息「{excerpt}」")
        };
        let mut notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        let queue = notes.entry(conversation).or_default();
        queue.push_back(note);
        while queue.len() > NOTES_PER_CONVERSATION {
            queue.pop_front();
        }
        true
    }

    /// Takes the notes waiting for a conversation.
    pub fn take_notes(&self, conversation: &str) -> Vec<String> {
        self.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(conversation)
            .map(Vec::from)
            .unwrap_or_default()
    }
}
