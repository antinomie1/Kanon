//! Generated Milky message api.

use super::*;

// ---- Message APIs ----

/// Request parameters for the `send_private_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendPrivateMessageInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Message content.
    #[serde(
        rename = "message",
        deserialize_with = "deserialize_drop_bad_outgoing_segment_list"
    )]
    pub message: Vec<OutgoingSegment>,
}

/// Response data for the `send_private_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendPrivateMessageOutput {
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Message send time.
    #[serde(rename = "time")]
    pub time: i64,
}

/// Request parameters for the `send_group_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendGroupMessageInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Message content.
    #[serde(
        rename = "message",
        deserialize_with = "deserialize_drop_bad_outgoing_segment_list"
    )]
    pub message: Vec<OutgoingSegment>,
}

/// Response data for the `send_group_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendGroupMessageOutput {
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Message send time.
    #[serde(rename = "time")]
    pub time: i64,
}

/// Request parameters for the `recall_private_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallPrivateMessageInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
}

/// Response data for the `recall_private_message` API.
pub type RecallPrivateMessageOutput = ApiEmptyStruct;

/// Request parameters for the `recall_group_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallGroupMessageInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
}

/// Response data for the `recall_group_message` API.
pub type RecallGroupMessageOutput = ApiEmptyStruct;

/// Request parameters for the `get_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetMessageInput {
    /// Message scene.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
}

/// Response data for the `get_message` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetMessageOutput {
    /// Message content.
    #[serde(rename = "message")]
    pub message: IncomingMessage,
}

/// Request parameters for the `get_history_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetHistoryMessagesInput {
    /// Message scene.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Starting message sequence number; queries proceed from newest to oldest starting here, or from the latest message when omitted.
    #[serde(
        rename = "start_message_seq",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub start_message_seq: Option<i64>,
    /// Desired number of messages to retrieve, up to 30.
    #[serde(
        rename = "limit",
        default = "default_get_history_messages_input_limit",
        deserialize_with = "deserialize_get_history_messages_input_limit"
    )]
    pub limit: i32,
}

/// Response data for the `get_history_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetHistoryMessagesOutput {
    /// Retrieved messages (sorted by `message_seq` in ascending order); some messages may be missing, such as recalled messages.
    #[serde(
        rename = "messages",
        deserialize_with = "deserialize_drop_bad_incoming_message_list"
    )]
    pub messages: Vec<IncomingMessage>,
    /// Starting message sequence number of the next page.
    #[serde(
        rename = "next_message_seq",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_message_seq: Option<i64>,
}

/// Request parameters for the `get_resource_temp_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetResourceTempUrlInput {
    /// Resource ID.
    #[serde(rename = "resource_id")]
    pub resource_id: String,
}

/// Response data for the `get_resource_temp_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetResourceTempUrlOutput {
    /// Temporary resource URL.
    #[serde(rename = "url")]
    pub url: String,
}

/// Request parameters for the `get_forwarded_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetForwardedMessagesInput {
    /// Forwarded message ID.
    #[serde(rename = "forward_id")]
    pub forward_id: String,
}

/// Response data for the `get_forwarded_messages` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetForwardedMessagesOutput {
    /// Forwarded message content.
    #[serde(rename = "messages")]
    pub messages: Vec<IncomingForwardedMessage>,
}

/// Request parameters for the `mark_message_as_read` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkMessageAsReadInput {
    /// Message scene.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Sequence number of the message to mark as read; this message and all earlier messages will be marked as read.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
}

/// Response data for the `mark_message_as_read` API.
pub type MarkMessageAsReadOutput = ApiEmptyStruct;

// ---- Friend APIs ----

/// Request parameters for the `send_friend_nudge` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendFriendNudgeInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether to nudge yourself.
    #[serde(
        rename = "is_self",
        default = "default_send_friend_nudge_input_is_self",
        deserialize_with = "deserialize_send_friend_nudge_input_is_self"
    )]
    pub is_self: bool,
}

/// Response data for the `send_friend_nudge` API.
pub type SendFriendNudgeOutput = ApiEmptyStruct;

/// Request parameters for the `send_profile_like` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendProfileLikeInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Number of likes.
    #[serde(
        rename = "count",
        default = "default_send_profile_like_input_count",
        deserialize_with = "deserialize_send_profile_like_input_count"
    )]
    pub count: i32,
}

/// Response data for the `send_profile_like` API.
pub type SendProfileLikeOutput = ApiEmptyStruct;

/// Request parameters for the `delete_friend` API.
/// @since 1.1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteFriendInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
}

/// Response data for the `delete_friend` API.
/// @since 1.1
pub type DeleteFriendOutput = ApiEmptyStruct;

/// Request parameters for the `get_friend_requests` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendRequestsInput {
    /// Maximum number of requests to retrieve.
    #[serde(
        rename = "limit",
        default = "default_get_friend_requests_input_limit",
        deserialize_with = "deserialize_get_friend_requests_input_limit"
    )]
    pub limit: i32,
    /// `true` to retrieve only filtered notifications (initiated by risky accounts); `false` to retrieve only unfiltered notifications.
    #[serde(
        rename = "is_filtered",
        default = "default_get_friend_requests_input_is_filtered",
        deserialize_with = "deserialize_get_friend_requests_input_is_filtered"
    )]
    pub is_filtered: bool,
}

/// Response data for the `get_friend_requests` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendRequestsOutput {
    /// Friend request list.
    #[serde(rename = "requests")]
    pub requests: Vec<FriendRequest>,
}

/// Request parameters for the `accept_friend_request` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptFriendRequestInput {
    /// UID of the request initiator.
    #[serde(rename = "initiator_uid")]
    pub initiator_uid: String,
    /// Whether the request was filtered.
    #[serde(
        rename = "is_filtered",
        default = "default_accept_friend_request_input_is_filtered",
        deserialize_with = "deserialize_accept_friend_request_input_is_filtered"
    )]
    pub is_filtered: bool,
}

/// Response data for the `accept_friend_request` API.
pub type AcceptFriendRequestOutput = ApiEmptyStruct;

/// Request parameters for the `reject_friend_request` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectFriendRequestInput {
    /// UID of the request initiator.
    #[serde(rename = "initiator_uid")]
    pub initiator_uid: String,
    /// Whether the request was filtered.
    #[serde(
        rename = "is_filtered",
        default = "default_reject_friend_request_input_is_filtered",
        deserialize_with = "deserialize_reject_friend_request_input_is_filtered"
    )]
    pub is_filtered: bool,
    /// Rejection reason.
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Response data for the `reject_friend_request` API.
pub type RejectFriendRequestOutput = ApiEmptyStruct;
