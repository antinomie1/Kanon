//! Generated Milky entities.

use super::*;

/// Friend entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendEntity {
    /// User QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// User nickname.
    #[serde(rename = "nickname")]
    pub nickname: String,
    /// User gender.
    #[serde(rename = "sex")]
    pub sex: String,
    /// User QID.
    #[serde(rename = "qid")]
    pub qid: String,
    /// Friend remark.
    #[serde(rename = "remark")]
    pub remark: String,
    /// Friend category.
    #[serde(rename = "category")]
    pub category: FriendCategoryEntity,
}

/// Friend category entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendCategoryEntity {
    /// Friend category ID.
    #[serde(rename = "category_id")]
    pub category_id: i32,
    /// Friend category name.
    #[serde(rename = "category_name")]
    pub category_name: String,
}

/// Group entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupEntity {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Group name.
    #[serde(rename = "group_name")]
    pub group_name: String,
    /// Number of group members.
    #[serde(rename = "member_count")]
    pub member_count: i32,
    /// Group capacity.
    #[serde(rename = "max_member_count")]
    pub max_member_count: i32,
    /// Group remark.
    /// @since 1.2
    #[serde(rename = "remark")]
    pub remark: String,
    /// Group creation time, Unix timestamp in seconds.
    /// @since 1.2
    #[serde(rename = "created_time")]
    pub created_time: i64,
    /// Group description.
    /// @since 1.2
    #[serde(rename = "description")]
    pub description: String,
    /// Group join verification question.
    /// @since 1.2
    #[serde(rename = "question")]
    pub question: String,
    /// Group announcement preview.
    /// @since 1.2
    #[serde(rename = "announcement")]
    pub announcement: String,
}

/// Group member entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMemberEntity {
    /// User QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// User nickname.
    #[serde(rename = "nickname")]
    pub nickname: String,
    /// User gender.
    #[serde(rename = "sex")]
    pub sex: String,
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Member remark.
    #[serde(rename = "card")]
    pub card: String,
    /// Special title.
    #[serde(rename = "title")]
    pub title: String,
    /// Group level; note that this is distinct from the QQ level.
    #[serde(rename = "level")]
    pub level: i32,
    /// Permission role.
    #[serde(rename = "role")]
    pub role: String,
    /// Group join time, Unix timestamp in seconds.
    #[serde(rename = "join_time")]
    pub join_time: i64,
    /// Last message time, Unix timestamp in seconds.
    #[serde(rename = "last_sent_time")]
    pub last_sent_time: i64,
    /// Mute end time, Unix timestamp in seconds.
    #[serde(
        rename = "shut_up_end_time",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub shut_up_end_time: Option<i64>,
}

/// Group announcement entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupAnnouncementEntity {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Announcement ID.
    #[serde(rename = "announcement_id")]
    pub announcement_id: String,
    /// Sender QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Unix timestamp in seconds.
    #[serde(rename = "time")]
    pub time: i64,
    /// Announcement content.
    #[serde(rename = "content")]
    pub content: String,
    /// Announcement image URL.
    #[serde(rename = "image_url", default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// Group file entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupFileEntity {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
    /// Parent folder ID.
    #[serde(rename = "parent_folder_id")]
    pub parent_folder_id: String,
    /// File size in bytes.
    #[serde(rename = "file_size")]
    pub file_size: i64,
    /// Upload Unix timestamp in seconds.
    #[serde(rename = "uploaded_time")]
    pub uploaded_time: i64,
    /// Expiration Unix timestamp in seconds.
    #[serde(
        rename = "expire_time",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expire_time: Option<i64>,
    /// Uploader QQ number.
    #[serde(rename = "uploader_id")]
    pub uploader_id: i64,
    /// Download count.
    #[serde(rename = "downloaded_times")]
    pub downloaded_times: i32,
}

/// Group folder entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupFolderEntity {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Folder ID.
    #[serde(rename = "folder_id")]
    pub folder_id: String,
    /// Parent folder ID.
    #[serde(rename = "parent_folder_id")]
    pub parent_folder_id: String,
    /// Folder name.
    #[serde(rename = "folder_name")]
    pub folder_name: String,
    /// Creation Unix timestamp in seconds.
    #[serde(rename = "created_time")]
    pub created_time: i64,
    /// Last modification Unix timestamp in seconds.
    #[serde(rename = "last_modified_time")]
    pub last_modified_time: i64,
    /// Creator QQ number.
    #[serde(rename = "creator_id")]
    pub creator_id: i64,
    /// File count.
    #[serde(rename = "file_count")]
    pub file_count: i32,
}

/// Friend request entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendRequest {
    /// Request initiation Unix timestamp in seconds.
    #[serde(rename = "time")]
    pub time: i64,
    /// QQ number of the request initiator.
    #[serde(rename = "initiator_id")]
    pub initiator_id: i64,
    /// UID of the request initiator.
    #[serde(rename = "initiator_uid")]
    pub initiator_uid: String,
    /// Target user QQ number.
    #[serde(rename = "target_user_id")]
    pub target_user_id: i64,
    /// Target user UID.
    #[serde(rename = "target_user_uid")]
    pub target_user_uid: String,
    /// Request status.
    #[serde(rename = "state")]
    pub state: String,
    /// Additional request information.
    #[serde(rename = "comment")]
    pub comment: String,
    /// Request source.
    #[serde(rename = "via")]
    pub via: String,
    /// Whether the request was filtered (initiated from a risky account).
    #[serde(rename = "is_filtered")]
    pub is_filtered: bool,
}

/// Group notification entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GroupNotification {
    /// User group join request.
    #[serde(rename = "join_request")]
    JoinRequest {
        /// Group number.
        #[serde(rename = "group_id")]
        group_id: i64,
        /// Notification sequence number.
        #[serde(rename = "notification_seq")]
        notification_seq: i64,
        /// Whether the request was filtered (initiated from a risky account).
        #[serde(rename = "is_filtered")]
        is_filtered: bool,
        /// Initiator QQ number.
        #[serde(rename = "initiator_id")]
        initiator_id: i64,
        /// Request status.
        #[serde(rename = "state")]
        state: String,
        /// QQ number of the admin handling the request.
        #[serde(
            rename = "operator_id",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        operator_id: Option<i64>,
        /// Additional group join request information.
        #[serde(rename = "comment")]
        comment: String,
    },

    /// Group admin change notification.
    #[serde(rename = "admin_change")]
    AdminChange {
        /// Group number.
        #[serde(rename = "group_id")]
        group_id: i64,
        /// Notification sequence number.
        #[serde(rename = "notification_seq")]
        notification_seq: i64,
        /// QQ number of the user set or unset.
        #[serde(rename = "target_user_id")]
        target_user_id: i64,
        /// Whether the user was set as an admin; `false` means admin privileges were revoked.
        #[serde(rename = "is_set")]
        is_set: bool,
        /// Operator (group owner) QQ number.
        #[serde(rename = "operator_id")]
        operator_id: i64,
    },

    /// Group member removed notification.
    #[serde(rename = "kick")]
    Kick {
        /// Group number.
        #[serde(rename = "group_id")]
        group_id: i64,
        /// Notification sequence number.
        #[serde(rename = "notification_seq")]
        notification_seq: i64,
        /// QQ number of the removed user.
        #[serde(rename = "target_user_id")]
        target_user_id: i64,
        /// QQ number of the admin who removed the user.
        #[serde(rename = "operator_id")]
        operator_id: i64,
    },

    /// Group member leave notification.
    #[serde(rename = "quit")]
    Quit {
        /// Group number.
        #[serde(rename = "group_id")]
        group_id: i64,
        /// Notification sequence number.
        #[serde(rename = "notification_seq")]
        notification_seq: i64,
        /// QQ number of the user who left the group.
        #[serde(rename = "target_user_id")]
        target_user_id: i64,
    },

    /// Group member invitation request.
    #[serde(rename = "invited_join_request")]
    InvitedJoinRequest {
        /// Group number.
        #[serde(rename = "group_id")]
        group_id: i64,
        /// Notification sequence number.
        #[serde(rename = "notification_seq")]
        notification_seq: i64,
        /// Inviter QQ number.
        #[serde(rename = "initiator_id")]
        initiator_id: i64,
        /// QQ number of the invited user.
        #[serde(rename = "target_user_id")]
        target_user_id: i64,
        /// Request status.
        #[serde(rename = "state")]
        state: String,
        /// QQ number of the admin handling the request.
        #[serde(
            rename = "operator_id",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        operator_id: Option<i64>,
    },
}

/// Received message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message_scene")]
pub enum IncomingMessage {
    /// Friend message.
    #[serde(rename = "friend")]
    Friend {
        /// Friend QQ number or group number.
        #[serde(rename = "peer_id")]
        peer_id: i64,
        /// Message sequence number.
        #[serde(rename = "message_seq")]
        message_seq: i64,
        /// Sender QQ number.
        #[serde(rename = "sender_id")]
        sender_id: i64,
        /// Message Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// List of message segments.
        #[serde(
            rename = "segments",
            deserialize_with = "deserialize_incoming_segment_list"
        )]
        segments: Vec<IncomingSegment>,
        /// Friend information.
        #[serde(rename = "friend")]
        friend: FriendEntity,
    },

    /// Group message.
    #[serde(rename = "group")]
    Group {
        /// Friend QQ number or group number.
        #[serde(rename = "peer_id")]
        peer_id: i64,
        /// Message sequence number.
        #[serde(rename = "message_seq")]
        message_seq: i64,
        /// Sender QQ number.
        #[serde(rename = "sender_id")]
        sender_id: i64,
        /// Message Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// List of message segments.
        #[serde(
            rename = "segments",
            deserialize_with = "deserialize_incoming_segment_list"
        )]
        segments: Vec<IncomingSegment>,
        /// Group information.
        #[serde(rename = "group")]
        group: GroupEntity,
        /// Group member information.
        #[serde(rename = "group_member")]
        group_member: GroupMemberEntity,
    },

    /// Temporary (temp) session message.
    #[serde(rename = "temp")]
    Temp {
        /// Friend QQ number or group number.
        #[serde(rename = "peer_id")]
        peer_id: i64,
        /// Message sequence number.
        #[serde(rename = "message_seq")]
        message_seq: i64,
        /// Sender QQ number.
        #[serde(rename = "sender_id")]
        sender_id: i64,
        /// Message Unix timestamp in seconds.
        #[serde(rename = "time")]
        time: i64,
        /// List of message segments.
        #[serde(
            rename = "segments",
            deserialize_with = "deserialize_incoming_segment_list"
        )]
        segments: Vec<IncomingSegment>,
        /// Group information of the sender of the temporary (temp) session.
        #[serde(rename = "group", default, skip_serializing_if = "Option::is_none")]
        group: Option<GroupEntity>,
    },
}

/// Received forwarded message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingForwardedMessage {
    /// Message sequence number.
    /// @since 1.2
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Sender name.
    #[serde(rename = "sender_name")]
    pub sender_name: String,
    /// Sender avatar URL.
    #[serde(rename = "avatar_url")]
    pub avatar_url: String,
    /// Message Unix timestamp in seconds.
    #[serde(rename = "time")]
    pub time: i64,
    /// List of message segments.
    #[serde(
        rename = "segments",
        deserialize_with = "deserialize_incoming_segment_list"
    )]
    pub segments: Vec<IncomingSegment>,
}

/// Group essence message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupEssenceMessage {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Message sequence number.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// Message send Unix timestamp in seconds.
    #[serde(rename = "message_time")]
    pub message_time: i64,
    /// Sender QQ number.
    #[serde(rename = "sender_id")]
    pub sender_id: i64,
    /// Sender name.
    #[serde(rename = "sender_name")]
    pub sender_name: String,
    /// QQ number of the operator who set the essence message.
    #[serde(rename = "operator_id")]
    pub operator_id: i64,
    /// Name of the operator who set the essence message.
    #[serde(rename = "operator_name")]
    pub operator_name: String,
    /// Unix timestamp when the message was set as an essence message.
    #[serde(rename = "operation_time")]
    pub operation_time: i64,
    /// List of message segments.
    #[serde(
        rename = "segments",
        deserialize_with = "deserialize_incoming_segment_list"
    )]
    pub segments: Vec<IncomingSegment>,
}
