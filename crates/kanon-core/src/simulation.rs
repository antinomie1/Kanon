//! Instance-level conversational participation policy and its stable model instructions.
//!
//! The policy is independent of persona selection: changing a character never disables the
//! conversation protocol. Runtime messages and timing belong in the current turn, not here.

use serde::{Deserialize, Serialize};

/// How an instance participates in conversations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationMode {
    /// Complete one answer for each message admitted by the reply policy.
    #[default]
    Assistant,
    /// Observe, speak, listen and leave through explicit conversational actions.
    Simulation,
}

/// Bounded timing and participation settings for a simulation instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulationPolicy {
    /// Silence after the most recent incoming message before composing a response.
    pub quiet_ms: u64,
    /// Maximum batching delay, even when a conversation stays continuously busy.
    pub max_batch_ms: u64,
    /// Maximum time one listen action may wait for a follow-up.
    pub listen_seconds: u64,
    /// Maximum duration of one participation, including model requests and waiting.
    pub max_participation_seconds: u64,
    /// Maximum independently chosen messages during one participation.
    pub max_messages: usize,
}

impl Default for SimulationPolicy {
    fn default() -> Self {
        Self {
            quiet_ms: 2500,
            max_batch_ms: 10000,
            listen_seconds: 30,
            max_participation_seconds: 180,
            max_messages: 3,
        }
    }
}

impl SimulationPolicy {
    /// Rejects impossible or unbounded timings before publishing instance configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.quiet_ms > 30000
            || self.max_batch_ms < self.quiet_ms
            || !(100..=60000).contains(&self.max_batch_ms)
        {
            return Err("simulation requires quiet_ms <= 30000 and quiet_ms <= max_batch_ms <= 60000 (minimum 100)".into());
        }
        if !(1..=120).contains(&self.listen_seconds)
            || !(10..=600).contains(&self.max_participation_seconds)
            || self.listen_seconds > self.max_participation_seconds
        {
            return Err("simulation requires listen_seconds in 1..=120, max_participation_seconds in 10..=600, and listen_seconds <= max_participation_seconds".into());
        }
        if self.max_batch_ms > self.max_participation_seconds * 1000 {
            return Err("simulation max_batch_ms must fit within max_participation_seconds".into());
        }
        if !(1..=20).contains(&self.max_messages) {
            return Err("simulation max_messages must be in 1..=20".into());
        }
        Ok(())
    }
}

/// Default cold-start attention: always consider direct mentions; sample other group messages.
/// Active listening accepts follow-ups independently of this wake policy.
pub fn default_reply_policy() -> crate::conversation::ReplyPolicy {
    crate::conversation::ReplyPolicy {
        mode: crate::conversation::ReplyMode::Probability,
        probability: 0.15,
        ..Default::default()
    }
}

/// Maximum length of an ordinary conversational contribution, measured in Unicode characters.
pub const CASUAL_MESSAGE_CHARS: usize = 120;

/// Stable rules for explicit speech; ordinary assistant output stays internal in this mode.
pub const SIMULATION_PROTOCOL: &str = "Conversation mode: simulation. You are taking part as one \
member of this conversation. The incoming user-role content is a conversation timeline, not a \
service request that requires an answer. Preserve your selected persona, but do not use the \
habit of completing an assistant task on every turn. First decide whether to participate. \
Use conversation_wait to stay quiet and listen, conversation_leave to step away, and \
conversation_say only when you actually have something to say to someone here. Every decision \
must use an action tool, including a decision to remain silent. Even a one-line reply must go \
through conversation_say; never write the chat reply as ordinary final text. Ordinary assistant \
output is internal and never delivered. A directly addressed question or a reply to \
your own remark is a good reason to speak; an unrelated exchange, a bare acknowledgment, a \
question already answered by someone else, or an ignored previous comment is a reason to listen. \
For an unaddressed group timeline, listening is the default. You may join a relevant shared topic \
with one brief contribution, but you are not responsible for answering the room. Each batch \
allows at most one spoken contribution; do not answer each line or split a long answer across \
repeated calls. Casual messages must fit the current length limit. expanded=true is reserved \
for a member explicitly asking you for a detailed explanation, code or a long artifact; do not \
use it simply because you know more. After speaking, usually choose wait and let others take \
the floor. After wait or leave, end this internal turn. Never announce these actions or promise \
to monitor messages the platform cannot deliver. Runtime limits and identities are in the \
current turn; send only to this conversation and reference only supplied message IDs.";

/// Optional social guidance shared by assistant and simulation conversations.
pub const CONVERSATION_RULES: &str = "Join the conversation as a considerate participant. Match \
the language, pace and level of detail of the people talking. In casual Chinese chat, a few \
words or one or two short sentences usually suffice. React to one interesting point and leave \
space for a reply; do not exhaust a topic, provide a mini-essay, summarize everyone, give \
unasked advice, or end every remark with a question. A follow-up question is optional, not a \
technique to prolong every exchange. For example, someone saying '今天累死了' might receive \
'今天这么忙啊'; it does not call for a numbered recovery plan. If two other members are arranging \
dinner, let them arrange it. If someone says '谢了' or '哈哈', the exchange may already be complete. \
When explicitly asked for an explanation, answer it naturally and expand only as much as asked. \
Keep the persona's voice and interests without inventing personal experiences, facts or human \
identity. Be honest if asked about being an AI. Avoid canned offers of help, routine headings, \
bullets, lecture-like transitions, fake typing, intentional errors, and mechanically splitting \
text into fragments. Mention or quote someone only when it clarifies whom you are addressing.";
