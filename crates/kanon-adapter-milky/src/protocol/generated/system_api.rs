//! Generated Milky system api.

use super::*;

// ---- System APIs ----

/// Request parameters for the `get_login_info` API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetLoginInfoInput {}

/// Response data for the `get_login_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetLoginInfoOutput {
    /// Logged-in QQ number.
    #[serde(rename = "uin")]
    pub uin: i64,
    /// Login nickname.
    #[serde(rename = "nickname")]
    pub nickname: String,
}

/// Request parameters for the `get_impl_info` API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetImplInfoInput {}

/// Response data for the `get_impl_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetImplInfoOutput {
    /// Protocol implementation name.
    #[serde(rename = "impl_name")]
    pub impl_name: String,
    /// Protocol implementation version.
    #[serde(rename = "impl_version")]
    pub impl_version: String,
    /// QQ protocol version used by the protocol implementation.
    #[serde(rename = "qq_protocol_version")]
    pub qq_protocol_version: String,
    /// QQ protocol platform used by the protocol implementation.
    #[serde(rename = "qq_protocol_type")]
    pub qq_protocol_type: String,
    /// Milky protocol version implemented by the protocol implementation, currently "1.3".
    #[serde(rename = "milky_version")]
    pub milky_version: String,
}

/// Request parameters for the `get_user_profile` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetUserProfileInput {
    /// User QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
}

/// Response data for the `get_user_profile` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetUserProfileOutput {
    /// Nickname.
    #[serde(rename = "nickname")]
    pub nickname: String,
    /// QID
    #[serde(rename = "qid")]
    pub qid: String,
    /// Age.
    #[serde(rename = "age")]
    pub age: i32,
    /// Gender.
    #[serde(rename = "sex")]
    pub sex: String,
    /// Remark.
    #[serde(rename = "remark")]
    pub remark: String,
    /// Bio.
    #[serde(rename = "bio")]
    pub bio: String,
    /// QQ level.
    #[serde(rename = "level")]
    pub level: i32,
    /// Country or region.
    #[serde(rename = "country")]
    pub country: String,
    /// City.
    #[serde(rename = "city")]
    pub city: String,
    /// School.
    #[serde(rename = "school")]
    pub school: String,
}

/// Request parameters for the `get_friend_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendListInput {
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_friend_list_input_no_cache",
        deserialize_with = "deserialize_get_friend_list_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_friend_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendListOutput {
    /// Friend list.
    #[serde(rename = "friends")]
    pub friends: Vec<FriendEntity>,
}

/// Request parameters for the `get_friend_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendInfoInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_friend_info_input_no_cache",
        deserialize_with = "deserialize_get_friend_info_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_friend_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetFriendInfoOutput {
    /// Friend information.
    #[serde(rename = "friend")]
    pub friend: FriendEntity,
}

/// Request parameters for the `get_group_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupListInput {
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_group_list_input_no_cache",
        deserialize_with = "deserialize_get_group_list_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_group_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupListOutput {
    /// Group list.
    #[serde(rename = "groups")]
    pub groups: Vec<GroupEntity>,
}

/// Request parameters for the `get_group_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupInfoInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_group_info_input_no_cache",
        deserialize_with = "deserialize_get_group_info_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_group_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupInfoOutput {
    /// Group information.
    #[serde(rename = "group")]
    pub group: GroupEntity,
}

/// Request parameters for the `get_group_member_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupMemberListInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_group_member_list_input_no_cache",
        deserialize_with = "deserialize_get_group_member_list_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_group_member_list` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupMemberListOutput {
    /// Group member list.
    #[serde(rename = "members")]
    pub members: Vec<GroupMemberEntity>,
}

/// Request parameters for the `get_group_member_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupMemberInfoInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Group member QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Whether to force bypassing the cache.
    #[serde(
        rename = "no_cache",
        default = "default_get_group_member_info_input_no_cache",
        deserialize_with = "deserialize_get_group_member_info_input_no_cache"
    )]
    pub no_cache: bool,
}

/// Response data for the `get_group_member_info` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupMemberInfoOutput {
    /// Group member information.
    #[serde(rename = "member")]
    pub member: GroupMemberEntity,
}

/// Request parameters for the `get_peer_pins` API.
/// @since 1.2
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetPeerPinsInput {}

/// Response data for the `get_peer_pins` API.
/// @since 1.2
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetPeerPinsOutput {
    /// Pinned friend list.
    #[serde(rename = "friends")]
    pub friends: Vec<FriendEntity>,
    /// Pinned group list.
    #[serde(rename = "groups")]
    pub groups: Vec<GroupEntity>,
}

/// Request parameters for the `set_peer_pin` API.
/// @since 1.2
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetPeerPinInput {
    /// Message scene of the session to set.
    #[serde(rename = "message_scene")]
    pub message_scene: String,
    /// Friend QQ number or group number to set.
    #[serde(rename = "peer_id")]
    pub peer_id: i64,
    /// Whether to pin; `false` means unpin.
    #[serde(
        rename = "is_pinned",
        default = "default_set_peer_pin_input_is_pinned",
        deserialize_with = "deserialize_set_peer_pin_input_is_pinned"
    )]
    pub is_pinned: bool,
}

/// Response data for the `set_peer_pin` API.
/// @since 1.2
pub type SetPeerPinOutput = ApiEmptyStruct;

/// Request parameters for the `set_avatar` API.
/// @since 1.1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetAvatarInput {
    /// Avatar file URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "uri")]
    pub uri: String,
}

/// Response data for the `set_avatar` API.
/// @since 1.1
pub type SetAvatarOutput = ApiEmptyStruct;

/// Request parameters for the `set_nickname` API.
/// @since 1.1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetNicknameInput {
    /// New nickname.
    #[serde(rename = "new_nickname")]
    pub new_nickname: String,
}

/// Response data for the `set_nickname` API.
/// @since 1.1
pub type SetNicknameOutput = ApiEmptyStruct;

/// Request parameters for the `set_bio` API.
/// @since 1.1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetBioInput {
    /// New bio.
    #[serde(rename = "new_bio")]
    pub new_bio: String,
}

/// Response data for the `set_bio` API.
/// @since 1.1
pub type SetBioOutput = ApiEmptyStruct;

/// Request parameters for the `get_custom_face_url_list` API.
/// @since 1.1
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCustomFaceUrlListInput {}

/// Response data for the `get_custom_face_url_list` API.
/// @since 1.1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCustomFaceUrlListOutput {
    /// Custom emoji URL list.
    #[serde(rename = "urls")]
    pub urls: Vec<String>,
}

/// Request parameters for the `get_cookies` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCookiesInput {
    /// Domain for which to retrieve Cookies.
    #[serde(rename = "domain")]
    pub domain: String,
}

/// Response data for the `get_cookies` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCookiesOutput {
    /// Cookies string corresponding to the domain.
    #[serde(rename = "cookies")]
    pub cookies: String,
}

/// Request parameters for the `get_csrf_token` API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCsrfTokenInput {}

/// Response data for the `get_csrf_token` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCsrfTokenOutput {
    /// CSRF Token
    #[serde(rename = "csrf_token")]
    pub csrf_token: String,
}
