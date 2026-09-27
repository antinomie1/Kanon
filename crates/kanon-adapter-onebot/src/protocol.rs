//! Wire types for OneBot v11 public API requests and responses.
//! Message segment data and implementation-specific status fields remain open JSON objects.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A OneBot message in either protocol-supported representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Message {
    /// A CQ string, or plain text when the sending API enables `auto_escape`.
    Text(String),
    /// Structured message segments.
    Segments(Vec<MessageSegment>),
}

/// A typed segment envelope with protocol-specific data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageSegment {
    /// Segment discriminator, serialized as `type`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Segment parameters, including nested forward nodes and implementation extensions.
    pub data: Map<String, Value>,
}

/// Result of sending a private or group message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SendMessageOutput {
    /// Identifier assigned to the sent message.
    pub message_id: i64,
}

/// Identity of the account logged into the OneBot implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoginInfo {
    /// Account QQ number.
    pub user_id: i64,
    /// Account nickname.
    pub nickname: String,
}

/// Public profile returned for an individual account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrangerInfo {
    /// Account QQ number.
    pub user_id: i64,
    /// Account nickname.
    pub nickname: String,
    /// Reported gender: `male`, `female`, or `unknown`.
    pub sex: String,
    /// Reported age in years.
    pub age: i32,
}

/// An entry in the logged-in account's friend list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FriendInfo {
    /// Friend's QQ number.
    pub user_id: i64,
    /// Friend's nickname.
    pub nickname: String,
    /// Locally assigned friend remark.
    pub remark: String,
}

/// Group details returned individually or as a list entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupInfo {
    /// Group number.
    pub group_id: i64,
    /// Group name.
    pub group_name: String,
    /// Current member count.
    pub member_count: i32,
    /// Maximum member count.
    pub max_member_count: i32,
}

/// Group membership details; list responses can omit profile and title details.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupMemberInfo {
    /// Group number.
    pub group_id: i64,
    /// Member's QQ number.
    pub user_id: i64,
    /// Account nickname.
    pub nickname: String,
    /// Group-specific display name or remark.
    pub card: String,
    /// Reported gender: `male`, `female`, or `unknown`.
    pub sex: String,
    /// Reported age in years.
    pub age: i32,
    /// Reported region, which may be unavailable in member lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// Unix timestamp when the member joined.
    pub join_time: i64,
    /// Unix timestamp of the member's last message.
    pub last_sent_time: i64,
    /// Group activity level.
    pub level: String,
    /// Group role: `owner`, `admin`, or `member`.
    pub role: String,
    /// Whether the member has an unfavorable record.
    pub unfriendly: bool,
    /// Special title, which may be unavailable in member lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Title expiration timestamp, when title details are available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_expire_time: Option<i64>,
    /// Whether the member's group card can be changed.
    pub card_changeable: bool,
}

/// Sender profile shared by private and group message responses.
///
/// OneBot explicitly makes every sender field best-effort, including the account ID and
/// nickname. Missing information remains `None`, particularly for anonymous group messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageSender {
    /// Sender's QQ number, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    /// Sender's account nickname, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Group-specific display name, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// Reported gender, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sex: Option<String>,
    /// Reported age in years, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<i32>,
    /// Reported region, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// Group activity level, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Group role, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Group-specific special title, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A message fetched by its OneBot identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageInfo {
    /// Unix timestamp when the message was sent.
    pub time: i64,
    /// Conversation type, such as `private` or `group`.
    pub message_type: String,
    /// OneBot message identifier.
    pub message_id: i64,
    /// Underlying platform message identifier.
    pub real_id: i64,
    /// Sender profile supplied by the implementation.
    pub sender: MessageSender,
    /// Message content in string or segment form.
    pub message: Message,
}

/// A fetched merged-forward message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForwardMessage {
    /// Forwarded nodes represented as `node` message segments.
    pub message: Vec<MessageSegment>,
}

/// A file retrieved or converted by the OneBot implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaFile {
    /// Path on the OneBot host; this is not a local path on the Kanon host.
    pub file: String,
}

/// Availability of an implementation capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capability {
    /// Whether the requested capability is available.
    pub yes: bool,
}

/// Runtime health reported by the OneBot implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeStatus {
    /// Account presence; `None` means the implementation cannot determine it.
    pub online: Option<bool>,
    /// Whether the implementation and its logged-in account are operating normally.
    pub good: bool,
    /// Additional implementation-specific health fields.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

/// Implementation and protocol version details.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    /// Implementation identifier.
    pub app_name: String,
    /// Implementation version.
    pub app_version: String,
    /// OneBot protocol version.
    pub protocol_version: String,
    /// Additional implementation-specific version fields.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

/// Request category supplied to `set_group_add_request`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GroupRequestType {
    /// An application to join a group.
    Add,
    /// An invitation to join a group.
    Invite,
}
