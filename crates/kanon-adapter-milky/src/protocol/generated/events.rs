//! Generated Milky events.

use super::*;

/// Event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum Event {
    /// Bot offline event.
    #[serde(rename = "bot_offline")]
    BotOffline {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Bot offline event data.
        #[serde(rename = "data")]
        data: EventBotOfflineData,
    },

    /// Message receive event.
    #[serde(rename = "message_receive")]
    MessageReceive {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Message receive event data.
        #[serde(rename = "data")]
        data: IncomingMessage,
    },

    /// Message recall event.
    #[serde(rename = "message_recall")]
    MessageRecall {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Message recall event data.
        #[serde(rename = "data")]
        data: EventMessageRecallData,
    },

    /// Session pin change event.
    /// @since 1.2
    #[serde(rename = "peer_pin_change")]
    PeerPinChange {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Session pin change event data.
        #[serde(rename = "data")]
        data: EventPeerPinChangeData,
    },

    /// Friend request event.
    #[serde(rename = "friend_request")]
    FriendRequest {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Friend request event data.
        #[serde(rename = "data")]
        data: EventFriendRequestData,
    },

    /// Group join request event.
    #[serde(rename = "group_join_request")]
    GroupJoinRequest {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group join request event data.
        #[serde(rename = "data")]
        data: EventGroupJoinRequestData,
    },

    /// Group member invitation request event.
    #[serde(rename = "group_invited_join_request")]
    GroupInvitedJoinRequest {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group member invitation request event data.
        #[serde(rename = "data")]
        data: EventGroupInvitedJoinRequestData,
    },

    /// Invitation-to-join-group event.
    #[serde(rename = "group_invitation")]
    GroupInvitation {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Invitation-to-join-group event data.
        #[serde(rename = "data")]
        data: EventGroupInvitationData,
    },

    /// Friend nudge event.
    #[serde(rename = "friend_nudge")]
    FriendNudge {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Friend nudge event data.
        #[serde(rename = "data")]
        data: EventFriendNudgeData,
    },

    /// Friend file upload event.
    #[serde(rename = "friend_file_upload")]
    FriendFileUpload {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Friend file upload event data.
        #[serde(rename = "data")]
        data: EventFriendFileUploadData,
    },

    /// Group admin change event.
    #[serde(rename = "group_admin_change")]
    GroupAdminChange {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group admin change event data.
        #[serde(rename = "data")]
        data: EventGroupAdminChangeData,
    },

    /// Group essence message change event.
    #[serde(rename = "group_essence_message_change")]
    GroupEssenceMessageChange {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group essence message change event data.
        #[serde(rename = "data")]
        data: EventGroupEssenceMessageChangeData,
    },

    /// Group member increase event.
    #[serde(rename = "group_member_increase")]
    GroupMemberIncrease {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group member increase event data.
        #[serde(rename = "data")]
        data: EventGroupMemberIncreaseData,
    },

    /// Group member decrease event.
    #[serde(rename = "group_member_decrease")]
    GroupMemberDecrease {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group member decrease event data.
        #[serde(rename = "data")]
        data: EventGroupMemberDecreaseData,
    },

    /// Group disband event.
    /// @since 1.3
    #[serde(rename = "group_disband")]
    GroupDisband {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group disband event data.
        #[serde(rename = "data")]
        data: EventGroupDisbandData,
    },

    /// Group name change event.
    #[serde(rename = "group_name_change")]
    GroupNameChange {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group name change event data.
        #[serde(rename = "data")]
        data: EventGroupNameChangeData,
    },

    /// Group message reaction event.
    #[serde(rename = "group_message_reaction")]
    GroupMessageReaction {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group message reaction event data.
        #[serde(rename = "data")]
        data: EventGroupMessageReactionData,
    },

    /// Group mute event.
    #[serde(rename = "group_mute")]
    GroupMute {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group mute event data.
        #[serde(rename = "data")]
        data: EventGroupMuteData,
    },

    /// Group whole-group mute event.
    #[serde(rename = "group_whole_mute")]
    GroupWholeMute {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group whole-group mute event data.
        #[serde(rename = "data")]
        data: EventGroupWholeMuteData,
    },

    /// Group nudge event.
    #[serde(rename = "group_nudge")]
    GroupNudge {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group nudge event data.
        #[serde(rename = "data")]
        data: EventGroupNudgeData,
    },

    /// Group file upload event.
    #[serde(rename = "group_file_upload")]
    GroupFileUpload {
        /// Event Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// Bot QQ number.
        #[serde(rename = "self_id")]
        self_id: i64,
        /// Group file upload event data.
        #[serde(rename = "data")]
        data: EventGroupFileUploadData,
    },
}

/// Bot offline event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventBotOfflineData {
    /// Offline reason.
    #[serde(rename = "reason")]
    pub reason: String,
}

/// Message recall event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMessageRecallData {
    /// Message scene.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// QQ number of the sender of the recalled message.
    #[serde(rename = "sender_id")]
    pub sender_id: i64,
    /// Operator QQ number.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Suffix text of the recall prompt.
    #[serde(rename = "display_suffix")]
    pub display_suffix: String,
}

/// Session pin change event data.
/// @since 1.2
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPeerPinChangeData {
    /// Message scene of the changed session.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number that changed.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Whether it is pinned; `false` means unpinned.
    #[serde(rename = "is_pinned")]
    pub is_pinned: bool,
}

/// Friend request event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFriendRequestData {
    /// QQ number of the user requesting friendship.
    #[serde(rename = "initiator_id")]
    pub initiator_id: i64,
    /// User UID.
    #[serde(rename = "initiator_uid")]
    pub initiator_uid: String,
    /// Additional request information.
    #[serde(rename = "comment")]
    pub comment: String,
    /// Request source.
    #[serde(rename = "via")]
    pub via: String,
}

/// Group join request event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupJoinRequestData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Notification sequence number corresponding to the request.
    #[serde(rename = "notification_seq")]
    pub notification_seq: i64,
    /// Whether the request was filtered (initiated from a risky account).
    #[serde(rename = "is_filtered")]
    pub is_filtered: bool,
    /// QQ number of the user requesting to join the group.
    #[serde(rename = "initiator_id")]
    pub initiator_id: i64,
    /// Additional request information.
    #[serde(rename = "comment")]
    pub comment: String,
}

/// Group member invitation request event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupInvitedJoinRequestData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Notification sequence number corresponding to the request.
    #[serde(rename = "notification_seq")]
    pub notification_seq: i64,
    /// Inviter QQ number.
    #[serde(rename = "initiator_id")]
    pub initiator_id: i64,
    /// Invitee QQ number.
    #[serde(rename = "target_user_id")]
    pub target_user_id: i64,
}

/// Invitation-to-join-group event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupInvitationData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Invitation sequence number.
    #[serde(rename = "invitation_seq")]
    pub invitation_seq: i64,
    /// Inviter QQ number.
    #[serde(rename = "initiator_id")]
    pub initiator_id: i64,
    /// Source group number, if invited via a QQ group.
    /// @since 1.2
    #[serde(
        rename = "source_group_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_group_id: Option<i64>,
}

/// Friend nudge event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFriendNudgeData {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether the nudge was sent by yourself.
    #[serde(rename = "is_self_send")]
    pub is_self_send: bool,
    /// Whether the nudge was received by yourself.
    #[serde(rename = "is_self_receive")]
    pub is_self_receive: bool,
    /// Action text of the nudge prompt.
    #[serde(rename = "display_action")]
    pub display_action: String,
    /// Suffix text of the nudge prompt.
    #[serde(rename = "display_suffix")]
    pub display_suffix: String,
    /// Action image URL of the nudge prompt, used in place of the action prompt text.
    #[serde(rename = "display_action_img_url")]
    pub display_action_img_url: String,
}

/// Friend file upload event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFriendFileUploadData {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
    /// File size in bytes.
    #[serde(rename = "file_size")]
    pub file_size: i64,
    /// TriSHA1 hash of the file.
    #[serde(rename = "file_hash")]
    pub file_hash: String,
    /// Whether the file was sent by yourself.
    #[serde(rename = "is_self")]
    pub is_self: bool,
}

/// Group admin change event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupAdminChangeData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the user that changed.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Operator QQ number.
    /// @since 1.1
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Whether the user was set as an admin; `false` means admin privileges were revoked.
    #[serde(rename = "is_set")]
    pub is_set: bool,
}

/// Group essence message change event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupEssenceMessageChangeData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Sequence number of the changed message.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Operator QQ number.
    /// @since 1.1
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Whether it was set as an essence message; `false` means the essence mark was removed.
    #[serde(rename = "is_set")]
    pub is_set: bool,
}

/// Group member increase event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupMemberIncreaseData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the user that changed.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Admin QQ number, if the request was approved by an admin.
    #[serde(
        rename = "operator_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub operator_id: Option<i64>,
    /// Inviter QQ number, if invited to join the group.
    #[serde(
        rename = "invitor_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub invitor_id: Option<i64>,
}

/// Group member decrease event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupMemberDecreaseData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the user that changed.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Admin QQ number, if kicked by an admin.
    #[serde(
        rename = "operator_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub operator_id: Option<i64>,
}

/// Group disband event data.
/// @since 1.3
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupDisbandData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Operator QQ number.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
}

/// Group name change event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupNameChangeData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// New group name.
    #[serde(rename = "new_group_name")]
    pub new_group_name: String,
    /// Operator QQ number.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
}

/// Group message reaction event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupMessageReactionData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the user who sent the reaction.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Emoji ID.
    #[serde(rename = "face_id")]
    pub face_id: String,
    /// Type of the reaction received.
    /// @since 1.2
    #[serde(rename = "reaction_type")]
    pub reaction_type: String,
    /// Whether it is an addition; `false` means the reaction was removed.
    #[serde(rename = "is_add")]
    pub is_add: bool,
}

/// Group mute event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupMuteData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the user that changed.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Operator QQ number.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Mute duration in seconds; 0 means unmute.
    #[serde(rename = "duration")]
    pub duration: i32,
}

/// Group whole-group mute event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupWholeMuteData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Operator QQ number.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Whether whole-group mute is enabled; `false` means whole-group mute is disabled.
    #[serde(rename = "is_mute")]
    pub is_mute: bool,
}

/// Group nudge event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupNudgeData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Sender QQ number.
    #[serde(rename = "sender_id")]
    pub sender_id: i64,
    /// Receiver QQ number.
    #[serde(rename = "receiver_id")]
    pub receiver_id: i64,
    /// Action text of the nudge prompt.
    #[serde(rename = "display_action")]
    pub display_action: String,
    /// Suffix text of the nudge prompt.
    #[serde(rename = "display_suffix")]
    pub display_suffix: String,
    /// Action image URL of the nudge prompt, used in place of the action prompt text.
    #[serde(rename = "display_action_img_url")]
    pub display_action_img_url: String,
}

/// Group file upload event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventGroupFileUploadData {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Sender QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
    /// File size in bytes.
    #[serde(rename = "file_size")]
    pub file_size: i64,
}
