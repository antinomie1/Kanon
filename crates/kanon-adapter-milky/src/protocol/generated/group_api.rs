//! Generated Milky group api.

use super::*;

// ---- Group APIs ----

/// Request parameters for the `set_group_name` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupNameInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// New group name.
    #[serde(rename = "new_group_name")]
    pub new_group_name: String,
}

/// Response data for the `set_group_name` API.
pub type SetGroupNameOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_avatar` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupAvatarInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Avatar file URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "image_uri")]
    pub image_uri: String,
}

/// Response data for the `set_group_avatar` API.
pub type SetGroupAvatarOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_member_card` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupMemberCardInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the group member being operated on.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// New group card.
    #[serde(rename = "card")]
    pub card: String,
}

/// Response data for the `set_group_member_card` API.
pub type SetGroupMemberCardOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_member_special_title` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupMemberSpecialTitleInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the group member being operated on.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// New special title.
    #[serde(rename = "special_title")]
    pub special_title: String,
}

/// Response data for the `set_group_member_special_title` API.
pub type SetGroupMemberSpecialTitleOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_member_admin` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupMemberAdminInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number being operated on.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether to set the member as an admin; `false` means revoking admin privileges.
    #[serde(
        rename = "is_set",
        default = "default_set_group_member_admin_input_is_set",
        deserialize_with = "deserialize_set_group_member_admin_input_is_set"
    )]
    pub is_set: bool,
}

/// Response data for the `set_group_member_admin` API.
pub type SetGroupMemberAdminOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_member_mute` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupMemberMuteInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number being operated on.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Mute duration in seconds; set to `0` to unmute.
    #[serde(
        rename = "duration",
        default = "default_set_group_member_mute_input_duration",
        deserialize_with = "deserialize_set_group_member_mute_input_duration"
    )]
    pub duration: i32,
}

/// Response data for the `set_group_member_mute` API.
pub type SetGroupMemberMuteOutput = ApiEmptyStruct;

/// Request parameters for the `set_group_whole_mute` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupWholeMuteInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Whether to enable whole-group mute; `false` means disabling whole-group mute.
    #[serde(
        rename = "is_mute",
        default = "default_set_group_whole_mute_input_is_mute",
        deserialize_with = "deserialize_set_group_whole_mute_input_is_mute"
    )]
    pub is_mute: bool,
}

/// Response data for the `set_group_whole_mute` API.
pub type SetGroupWholeMuteOutput = ApiEmptyStruct;

/// Request parameters for the `kick_group_member` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KickGroupMemberInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the member to kick.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether to reject the group join request; `false` means not to reject it.
    #[serde(
        rename = "reject_add_request",
        default = "default_kick_group_member_input_reject_add_request",
        deserialize_with = "deserialize_kick_group_member_input_reject_add_request"
    )]
    pub reject_add_request: bool,
}

/// Response data for the `kick_group_member` API.
pub type KickGroupMemberOutput = ApiEmptyStruct;

/// Request parameters for the `get_group_announcements` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupAnnouncementsInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
}

/// Response data for the `get_group_announcements` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupAnnouncementsOutput {
    /// Group announcement list.
    #[serde(rename = "announcements")]
    pub announcements: Vec<GroupAnnouncementEntity>,
}

/// Request parameters for the `send_group_announcement` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendGroupAnnouncementInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Announcement content.
    #[serde(rename = "content")]
    pub content: String,
    /// URI of the image file attached to the announcement, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "image_uri", default, skip_serializing_if = "Option::is_none")]
    pub image_uri: Option<String>,
}

/// Response data for the `send_group_announcement` API.
pub type SendGroupAnnouncementOutput = ApiEmptyStruct;

/// Request parameters for the `delete_group_announcement` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteGroupAnnouncementInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Announcement ID.
    #[serde(rename = "announcement_id")]
    pub announcement_id: String,
}

/// Response data for the `delete_group_announcement` API.
pub type DeleteGroupAnnouncementOutput = ApiEmptyStruct;

/// Request parameters for the `get_group_essence_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupEssenceMessagesInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Page index, starting from 0.
    #[serde(rename = "page_index")]
    pub page_index: i32,
    /// Number of essence messages per page.
    #[serde(rename = "page_size")]
    pub page_size: i32,
}

/// Response data for the `get_group_essence_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupEssenceMessagesOutput {
    /// Essence message list.
    #[serde(rename = "messages")]
    pub messages: Vec<GroupEssenceMessage>,
    /// Whether the last page has been reached.
    #[serde(rename = "is_end")]
    pub is_end: bool,
}

/// Request parameters for the `set_group_essence_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGroupEssenceMessageInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Whether to set the message as an essence message; `false` means removing the essence mark.
    #[serde(
        rename = "is_set",
        default = "default_set_group_essence_message_input_is_set",
        deserialize_with = "deserialize_set_group_essence_message_input_is_set"
    )]
    pub is_set: bool,
}

/// Response data for the `set_group_essence_message` API.
pub type SetGroupEssenceMessageOutput = ApiEmptyStruct;

/// Request parameters for the `quit_group` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuitGroupInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
}

/// Response data for the `quit_group` API.
pub type QuitGroupOutput = ApiEmptyStruct;

/// Request parameters for the `send_group_message_reaction` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendGroupMessageReactionInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Sequence number of the message to react to.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Emoji ID of the reaction to send.
    #[serde(rename = "reaction")]
    pub reaction: String,
    /// Type of the reaction to send.
    /// @since 1.2
    #[serde(
        rename = "reaction_type",
        default = "default_send_group_message_reaction_input_reaction_type",
        deserialize_with = "deserialize_send_group_message_reaction_input_reaction_type"
    )]
    pub reaction_type: String,
    /// Whether to add the emoji; `false` means removing it.
    #[serde(
        rename = "is_add",
        default = "default_send_group_message_reaction_input_is_add",
        deserialize_with = "deserialize_send_group_message_reaction_input_is_add"
    )]
    pub is_add: bool,
}

/// Response data for the `send_group_message_reaction` API.
pub type SendGroupMessageReactionOutput = ApiEmptyStruct;

/// Request parameters for the `send_group_nudge` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendGroupNudgeInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// QQ number of the group member to nudge.
    #[serde(rename = "user_id")]
    pub user_id: i64,
}

/// Response data for the `send_group_nudge` API.
pub type SendGroupNudgeOutput = ApiEmptyStruct;

/// Request parameters for the `get_group_notifications` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupNotificationsInput {
    /// Starting notification sequence number.
    #[serde(
        rename = "start_notification_seq",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub start_notification_seq: Option<i64>,
    /// `true` to retrieve only filtered notifications (initiated by risky accounts); `false` to retrieve only unfiltered notifications.
    #[serde(
        rename = "is_filtered",
        default = "default_get_group_notifications_input_is_filtered",
        deserialize_with = "deserialize_get_group_notifications_input_is_filtered"
    )]
    pub is_filtered: bool,
    /// Maximum number of notifications to retrieve.
    #[serde(
        rename = "limit",
        default = "default_get_group_notifications_input_limit",
        deserialize_with = "deserialize_get_group_notifications_input_limit"
    )]
    pub limit: i32,
}

/// Response data for the `get_group_notifications` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupNotificationsOutput {
    /// Retrieved group notifications (sorted by `notification_seq` in descending order); sequence numbers are not necessarily contiguous.
    #[serde(
        rename = "notifications",
        deserialize_with = "deserialize_drop_bad_group_notification_list"
    )]
    pub notifications: Vec<GroupNotification>,
    /// Starting notification sequence number of the next page.
    #[serde(
        rename = "next_notification_seq",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_notification_seq: Option<i64>,
}

/// Request parameters for the `accept_group_request` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptGroupRequestInput {
    /// Notification sequence number corresponding to the request.
    #[serde(rename = "notification_seq")]
    pub notification_seq: i64,
    /// Notification type corresponding to the request.
    #[serde(rename = "notification_type")]
    pub notification_type: String,
    /// Group number the request belongs to.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Whether the request was filtered.
    #[serde(
        rename = "is_filtered",
        default = "default_accept_group_request_input_is_filtered",
        deserialize_with = "deserialize_accept_group_request_input_is_filtered"
    )]
    pub is_filtered: bool,
}

/// Response data for the `accept_group_request` API.
pub type AcceptGroupRequestOutput = ApiEmptyStruct;

/// Request parameters for the `reject_group_request` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectGroupRequestInput {
    /// Notification sequence number corresponding to the request.
    #[serde(rename = "notification_seq")]
    pub notification_seq: i64,
    /// Notification type corresponding to the request.
    #[serde(rename = "notification_type")]
    pub notification_type: String,
    /// Group number the request belongs to.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Whether the request was filtered.
    #[serde(
        rename = "is_filtered",
        default = "default_reject_group_request_input_is_filtered",
        deserialize_with = "deserialize_reject_group_request_input_is_filtered"
    )]
    pub is_filtered: bool,
    /// Rejection reason.
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Response data for the `reject_group_request` API.
pub type RejectGroupRequestOutput = ApiEmptyStruct;

/// Request parameters for the `accept_group_invitation` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptGroupInvitationInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Invitation sequence number.
    #[serde(rename = "invitation_seq")]
    pub invitation_seq: i64,
}

/// Response data for the `accept_group_invitation` API.
pub type AcceptGroupInvitationOutput = ApiEmptyStruct;

/// Request parameters for the `reject_group_invitation` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectGroupInvitationInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Invitation sequence number.
    #[serde(rename = "invitation_seq")]
    pub invitation_seq: i64,
}

/// Response data for the `reject_group_invitation` API.
pub type RejectGroupInvitationOutput = ApiEmptyStruct;
