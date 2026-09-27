//! Conversation classification and the reply policy that decides whether a bot answers.
//!
//! # Why the core, not the adapter, decides
//! "Only answer when mentioned" is an operator preference about *a bot*, not a property of a
//! platform: the same Milky group should be answered by a chatty instance and ignored by a quiet
//! one. Adapters therefore only report two facts — what kind of conversation the event belongs to,
//! and whether the bot itself was addressed — through well-known metadata keys, and the core
//! applies the instance's policy to them.
//!
//! # Defaults are deliberately permissive
//! An adapter that reports nothing keeps the pre-policy behaviour: a conversation of unknown kind
//! is treated as private and always answered. That makes the feature additive — enabling a policy
//! is an explicit operator action, never a silent change that makes a working bot stop replying.

use serde::{Deserialize, Serialize};

use kanon_proto::prost_types;

/// Metadata key carrying the kind of conversation an event belongs to.
///
/// Adapters set one of `private`, `group` or `channel`. It is a platform-neutral key on purpose:
/// the reply policy must work for every adapter, including ones that do not exist yet.
pub const META_CONVERSATION_KIND: &str = "kanon.conversation_kind";

/// Metadata key carrying whether the bot itself was addressed by the event.
///
/// A boolean. Present and `true` when the bot was mentioned (or the platform event is inherently
/// addressed to it, such as a dedicated @-callback); absent or `false` otherwise.
pub const META_BOT_MENTIONED: &str = "kanon.bot_mentioned";

/// Kind of conversation an inbound event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    /// One-to-one conversation. Always answered, regardless of policy.
    Private,
    /// Multi-participant group chat, where an unaddressed bot should usually stay quiet.
    Group,
    /// Broadcast channel or guild channel; policy applies exactly as for a group.
    Channel,
}

impl ConversationKind {
    /// Canonical metadata value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
            Self::Channel => "channel",
        }
    }

    /// Reads the kind an adapter reported, defaulting to [`ConversationKind::Private`].
    ///
    /// The permissive default is what keeps an adapter that predates this feature working: it has
    /// no way to report a group, so its events must not be silenced by a group policy.
    pub fn from_metadata(metadata: Option<&prost_types::Struct>) -> Self {
        let reported = metadata
            .and_then(|metadata| metadata.fields.get(META_CONVERSATION_KIND))
            .and_then(|value| value.kind.as_ref())
            .and_then(|kind| match kind {
                prost_types::value::Kind::StringValue(text) => Some(text.as_str()),
                _ => None,
            });

        match reported.map(str::trim) {
            Some("group") => Self::Group,
            Some("channel") | Some("guild") => Self::Channel,
            _ => Self::Private,
        }
    }

    /// Whether the reply policy governs this kind of conversation.
    ///
    /// Private conversations are always answered: a user who wrote to the bot directly has
    /// unambiguously addressed it, so a mention requirement or a probability would only make the
    /// bot look broken.
    pub fn is_policy_governed(self) -> bool {
        matches!(self, Self::Group | Self::Channel)
    }
}

/// Reads whether an adapter reported the bot as addressed.
pub fn bot_mentioned(metadata: Option<&prost_types::Struct>) -> bool {
    metadata
        .and_then(|metadata| metadata.fields.get(META_BOT_MENTIONED))
        .and_then(|value| value.kind.as_ref())
        .and_then(|kind| match kind {
            prost_types::value::Kind::BoolValue(flag) => Some(*flag),
            _ => None,
        })
        .unwrap_or(false)
}

/// How an instance decides whether to answer a group conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode {
    /// Answer every message.
    Always,
    /// Answer only messages that mention the bot.
    Mention,
    /// Answer each message with a fixed probability.
    Probability,
    /// Never answer group messages (private conversations are still answered).
    Never,
}

impl Default for ReplyMode {
    fn default() -> Self {
        Self::Always
    }
}

/// Reply policy of an instance or of the node as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReplyPolicy {
    /// Decision mode for group and channel conversations.
    #[serde(default)]
    pub mode: ReplyMode,
    /// Probability used by [`ReplyMode::Probability`], in `0.0..=1.0`.
    #[serde(default = "default_probability")]
    pub probability: f32,
}

/// Default reply probability for [`ReplyMode::Probability`].
///
/// One half: a coin flip is the least surprising starting point an operator can then tune.
fn default_probability() -> f32 {
    0.5
}

impl Default for ReplyPolicy {
    fn default() -> Self {
        Self {
            mode: ReplyMode::Always,
            probability: default_probability(),
        }
    }
}

impl ReplyPolicy {
    /// Creates a policy with a mode and the default probability.
    pub fn new(mode: ReplyMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Validates the probability range.
    ///
    /// Rejected rather than clamped so a console typo (`5` meaning 50%) is reported instead of
    /// silently becoming "always reply".
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.probability) {
            return Err(format!(
                "reply probability {} is outside 0.0..=1.0",
                self.probability
            ));
        }
        Ok(())
    }

    /// Decides whether one event should be answered.
    ///
    /// `sample` is a caller-supplied value in `[0, 1)` used only by the probability mode; passing
    /// it in keeps this decision a pure function, which is what makes it testable without
    /// controlling a random number generator.
    pub fn should_reply(&self, kind: ConversationKind, mentioned: bool, sample: f32) -> bool {
        if !kind.is_policy_governed() {
            return true;
        }

        match self.mode {
            ReplyMode::Always => true,
            ReplyMode::Never => false,
            ReplyMode::Mention => mentioned,
            ReplyMode::Probability => sample < self.probability.clamp(0.0, 1.0),
        }
    }

    /// Human-readable description used in logs and in the console.
    pub fn describe(&self) -> String {
        match self.mode {
            ReplyMode::Always => "always".to_string(),
            ReplyMode::Mention => "mention only".to_string(),
            ReplyMode::Never => "never".to_string(),
            ReplyMode::Probability => format!("probability {:.0}%", self.probability * 100.0),
        }
    }
}

/// Hot-swappable node-wide reply policy shared by the pipeline and the management console.
///
/// The console can change the policy while events are in flight, so the pipeline reads it per
/// event instead of capturing a value at construction. A synchronous lock is enough: the guarded
/// value is `Copy`, so a read is a load and never held across an `await`.
#[derive(Debug)]
pub struct ReplyPolicyStore {
    /// Current node-wide policy.
    current: std::sync::RwLock<ReplyPolicy>,
}

impl Default for ReplyPolicyStore {
    fn default() -> Self {
        Self::new(ReplyPolicy::default())
    }
}

impl ReplyPolicyStore {
    /// Creates a store holding an initial policy.
    pub fn new(policy: ReplyPolicy) -> Self {
        Self {
            current: std::sync::RwLock::new(policy),
        }
    }

    /// Returns the current node-wide policy.
    pub fn get(&self) -> ReplyPolicy {
        *self
            .current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replaces the node-wide policy.
    pub fn set(&self, policy: ReplyPolicy) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }
}

/// Metadata key carrying the event timestamp in Unix seconds.
pub const META_TIMESTAMP: &str = "kanon.timestamp";

/// Metadata key carrying an adapter-formatted timestamp string, preferred over [`META_TIMESTAMP`].
pub const META_TIMESTAMP_TEXT: &str = "kanon.timestamp_text";

/// Whether identifying or contextual extras are prepended to the model prompt.
///
/// Both default to `false`: a sender id is personal data and a wall-clock time is not part of what
/// the user said, so including them is an explicit operator decision — which is why the switch
/// exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextPolicy {
    /// Whether the platform sender id (QQ number, openid, …) is included in the prompt.
    #[serde(default)]
    pub include_sender_id: bool,
    /// Whether the message timestamp is included in the prompt.
    #[serde(default)]
    pub include_timestamp: bool,
}

/// Hot-swappable node-wide context policy, mirroring [`ReplyPolicyStore`].
#[derive(Debug)]
pub struct ContextPolicyStore {
    /// Current node-wide policy.
    current: std::sync::RwLock<ContextPolicy>,
}

impl Default for ContextPolicyStore {
    fn default() -> Self {
        Self::new(ContextPolicy::default())
    }
}

impl ContextPolicyStore {
    /// Creates a store holding an initial policy.
    pub fn new(policy: ContextPolicy) -> Self {
        Self {
            current: std::sync::RwLock::new(policy),
        }
    }

    /// Returns the current node-wide policy.
    pub fn get(&self) -> ContextPolicy {
        *self
            .current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replaces the node-wide policy.
    pub fn set(&self, policy: ContextPolicy) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }
}
