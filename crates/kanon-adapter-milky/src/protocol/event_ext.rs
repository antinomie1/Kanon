//! Hand-written accessors over the vendored [`Event`] union.
//!
//! The generated enum is an internally tagged union: every variant repeats the envelope
//! fields `time` and `self_id` alongside its own payload. Callers that only need the envelope
//! would otherwise have to match all variants themselves, so the envelope is exposed here once.
//!
//! Both accessors match exhaustively on purpose. When Milky adds an event type, the generator
//! grows the enum and this module stops compiling — which is the intended signal that the new
//! event must be classified here rather than silently ignored.

use super::Event;

impl Event {
    /// Milky `event_type` discriminator of this event.
    pub fn event_type(&self) -> &'static str {
        self.classify().0
    }

    /// QQ number of the bot account this event belongs to.
    pub fn self_id(&self) -> i64 {
        self.classify().1
    }

    /// Unix timestamp in seconds carried by the event envelope.
    pub fn time(&self) -> i64 {
        match self {
            Event::BotOffline { time, .. } => *time,
            Event::MessageReceive { time, .. } => *time,
            Event::MessageRecall { time, .. } => *time,
            Event::PeerPinChange { time, .. } => *time,
            Event::FriendRequest { time, .. } => *time,
            Event::GroupJoinRequest { time, .. } => *time,
            Event::GroupInvitedJoinRequest { time, .. } => *time,
            Event::GroupInvitation { time, .. } => *time,
            Event::FriendNudge { time, .. } => *time,
            Event::FriendFileUpload { time, .. } => *time,
            Event::GroupAdminChange { time, .. } => *time,
            Event::GroupEssenceMessageChange { time, .. } => *time,
            Event::GroupMemberIncrease { time, .. } => *time,
            Event::GroupMemberDecrease { time, .. } => *time,
            Event::GroupDisband { time, .. } => *time,
            Event::GroupNameChange { time, .. } => *time,
            Event::GroupMessageReaction { time, .. } => *time,
            Event::GroupMute { time, .. } => *time,
            Event::GroupWholeMute { time, .. } => *time,
            Event::GroupNudge { time, .. } => *time,
            Event::GroupFileUpload { time, .. } => *time,
        }
    }

    /// Returns the event type name and the bot QQ number in one pass.
    fn classify(&self) -> (&'static str, i64) {
        match self {
            Event::BotOffline { self_id, .. } => ("bot_offline", *self_id),
            Event::MessageReceive { self_id, .. } => ("message_receive", *self_id),
            Event::MessageRecall { self_id, .. } => ("message_recall", *self_id),
            Event::PeerPinChange { self_id, .. } => ("peer_pin_change", *self_id),
            Event::FriendRequest { self_id, .. } => ("friend_request", *self_id),
            Event::GroupJoinRequest { self_id, .. } => ("group_join_request", *self_id),
            Event::GroupInvitedJoinRequest { self_id, .. } => {
                ("group_invited_join_request", *self_id)
            }
            Event::GroupInvitation { self_id, .. } => ("group_invitation", *self_id),
            Event::FriendNudge { self_id, .. } => ("friend_nudge", *self_id),
            Event::FriendFileUpload { self_id, .. } => ("friend_file_upload", *self_id),
            Event::GroupAdminChange { self_id, .. } => ("group_admin_change", *self_id),
            Event::GroupEssenceMessageChange { self_id, .. } => {
                ("group_essence_message_change", *self_id)
            }
            Event::GroupMemberIncrease { self_id, .. } => ("group_member_increase", *self_id),
            Event::GroupMemberDecrease { self_id, .. } => ("group_member_decrease", *self_id),
            Event::GroupDisband { self_id, .. } => ("group_disband", *self_id),
            Event::GroupNameChange { self_id, .. } => ("group_name_change", *self_id),
            Event::GroupMessageReaction { self_id, .. } => ("group_message_reaction", *self_id),
            Event::GroupMute { self_id, .. } => ("group_mute", *self_id),
            Event::GroupWholeMute { self_id, .. } => ("group_whole_mute", *self_id),
            Event::GroupNudge { self_id, .. } => ("group_nudge", *self_id),
            Event::GroupFileUpload { self_id, .. } => ("group_file_upload", *self_id),
        }
    }
}
