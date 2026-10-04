//! Generated Milky serde helpers.

use super::*;

pub(super) fn serialize_segment_with_data<S, T>(
    serializer: S,
    segment_type: &str,
    data: &T,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    serde_json::json!({
        "type": segment_type,
        "data": data,
    })
    .serialize(serializer)
}

pub(super) fn serialize_segment_with_field<S, T>(
    serializer: S,
    segment_type: &str,
    field_name: &str,
    value: &T,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    let mut object = serde_json::Map::with_capacity(2);
    object.insert(
        "type".to_string(),
        serde_json::Value::String(segment_type.to_string()),
    );
    object.insert(
        field_name.to_string(),
        serde_json::to_value(value).map_err(serde::ser::Error::custom)?,
    );
    serde_json::Value::Object(object).serialize(serializer)
}

pub(super) fn deserialize_segment_type<E>(value: &serde_json::Value) -> Result<&str, E>
where
    E: serde::de::Error,
{
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| E::missing_field("type"))
}

pub(super) fn deserialize_segment_data<E, T>(value: &serde_json::Value) -> Result<T, E>
where
    E: serde::de::Error,
    T: DeserializeOwned,
{
    let data = value
        .get("data")
        .cloned()
        .ok_or_else(|| E::missing_field("data"))?;
    serde_json::from_value(data).map_err(E::custom)
}

pub(super) fn deserialize_segment_field<E, T>(
    value: &serde_json::Value,
    field_name: &'static str,
) -> Result<T, E>
where
    E: serde::de::Error,
    T: DeserializeOwned,
{
    let field_value = value
        .get(field_name)
        .cloned()
        .ok_or_else(|| E::missing_field(field_name))?;
    serde_json::from_value(field_value).map_err(E::custom)
}

pub(super) fn deserialize_drop_bad_element_list<'de, D, T>(
    deserializer: D,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?.unwrap_or_default();
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if let Ok(item) = serde_json::from_value::<T>(value) {
            out.push(item);
        }
    }
    Ok(out)
}

pub(super) fn deserialize_optional_drop_bad_element_list<'de, D, T>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    let Some(values) = values else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if let Ok(item) = serde_json::from_value::<T>(value) {
            out.push(item);
        }
    }
    Ok(Some(out))
}

/// LOCAL PATCH: preserves a segment that could not be decoded instead of degrading it to text.
pub(super) fn unknown_incoming_segment(value: serde_json::Value) -> IncomingSegment {
    IncomingSegment::Unknown(value)
}

pub(super) fn deserialize_incoming_segment_list<'de, D>(
    deserializer: D,
) -> Result<Vec<IncomingSegment>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?.unwrap_or_default();
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match serde_json::from_value::<IncomingSegment>(value.clone()) {
            Ok(item) => out.push(item),
            Err(_) => out.push(unknown_incoming_segment(value)),
        }
    }
    Ok(out)
}

pub(super) fn deserialize_optional_incoming_segment_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<IncomingSegment>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    let Some(values) = values else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match serde_json::from_value::<IncomingSegment>(value.clone()) {
            Ok(item) => out.push(item),
            Err(_) => out.push(unknown_incoming_segment(value)),
        }
    }
    Ok(Some(out))
}

pub(super) fn deserialize_drop_bad_outgoing_segment_list<'de, D>(
    deserializer: D,
) -> Result<Vec<OutgoingSegment>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_drop_bad_element_list::<D, OutgoingSegment>(deserializer)
}

pub(super) fn deserialize_optional_drop_bad_outgoing_segment_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<OutgoingSegment>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_drop_bad_element_list::<D, OutgoingSegment>(deserializer)
}

pub(super) fn deserialize_drop_bad_incoming_message_list<'de, D>(
    deserializer: D,
) -> Result<Vec<IncomingMessage>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_drop_bad_element_list::<D, IncomingMessage>(deserializer)
}

pub(super) fn deserialize_optional_drop_bad_incoming_message_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<IncomingMessage>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_drop_bad_element_list::<D, IncomingMessage>(deserializer)
}

pub(super) fn deserialize_drop_bad_group_notification_list<'de, D>(
    deserializer: D,
) -> Result<Vec<GroupNotification>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_drop_bad_element_list::<D, GroupNotification>(deserializer)
}

pub(super) fn deserialize_optional_drop_bad_group_notification_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<GroupNotification>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_drop_bad_element_list::<D, GroupNotification>(deserializer)
}

pub(super) fn deserialize_default_on_null<'de, D, T, F>(
    deserializer: D,
    default: F,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
    F: FnOnce() -> T,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_else(default))
}

pub(super) fn default_outgoing_segment_face_data_is_large() -> bool {
    false
}

pub(super) fn deserialize_outgoing_segment_face_data_is_large<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_outgoing_segment_face_data_is_large)
}
pub(super) fn default_outgoing_segment_image_data_sub_type() -> String {
    "normal".to_string()
}

pub(super) fn deserialize_outgoing_segment_image_data_sub_type<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_outgoing_segment_image_data_sub_type)
}
pub(super) fn default_get_friend_list_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_friend_list_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_friend_list_input_no_cache)
}
pub(super) fn default_get_friend_info_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_friend_info_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_friend_info_input_no_cache)
}
pub(super) fn default_get_group_list_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_group_list_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_list_input_no_cache)
}
pub(super) fn default_get_group_info_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_group_info_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_info_input_no_cache)
}
pub(super) fn default_get_group_member_list_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_group_member_list_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_member_list_input_no_cache)
}
pub(super) fn default_get_group_member_info_input_no_cache() -> bool {
    false
}

pub(super) fn deserialize_get_group_member_info_input_no_cache<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_member_info_input_no_cache)
}
pub(super) fn default_set_peer_pin_input_is_pinned() -> bool {
    true
}

pub(super) fn deserialize_set_peer_pin_input_is_pinned<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_set_peer_pin_input_is_pinned)
}
pub(super) fn default_get_history_messages_input_limit() -> i32 {
    20
}

pub(super) fn deserialize_get_history_messages_input_limit<'de, D>(
    deserializer: D,
) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_history_messages_input_limit)
}
pub(super) fn default_send_friend_nudge_input_is_self() -> bool {
    false
}

pub(super) fn deserialize_send_friend_nudge_input_is_self<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_send_friend_nudge_input_is_self)
}
pub(super) fn default_send_profile_like_input_count() -> i32 {
    1
}

pub(super) fn deserialize_send_profile_like_input_count<'de, D>(
    deserializer: D,
) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_send_profile_like_input_count)
}
pub(super) fn default_get_friend_requests_input_limit() -> i32 {
    20
}

pub(super) fn deserialize_get_friend_requests_input_limit<'de, D>(
    deserializer: D,
) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_friend_requests_input_limit)
}
pub(super) fn default_get_friend_requests_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_get_friend_requests_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_friend_requests_input_is_filtered)
}
pub(super) fn default_accept_friend_request_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_accept_friend_request_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_accept_friend_request_input_is_filtered,
    )
}
pub(super) fn default_reject_friend_request_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_reject_friend_request_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_reject_friend_request_input_is_filtered,
    )
}
pub(super) fn default_set_group_member_admin_input_is_set() -> bool {
    true
}

pub(super) fn deserialize_set_group_member_admin_input_is_set<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_set_group_member_admin_input_is_set)
}
pub(super) fn default_set_group_member_mute_input_duration() -> i32 {
    0
}

pub(super) fn deserialize_set_group_member_mute_input_duration<'de, D>(
    deserializer: D,
) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_set_group_member_mute_input_duration)
}
pub(super) fn default_set_group_whole_mute_input_is_mute() -> bool {
    true
}

pub(super) fn deserialize_set_group_whole_mute_input_is_mute<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_set_group_whole_mute_input_is_mute)
}
pub(super) fn default_kick_group_member_input_reject_add_request() -> bool {
    false
}

pub(super) fn deserialize_kick_group_member_input_reject_add_request<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_kick_group_member_input_reject_add_request,
    )
}
pub(super) fn default_set_group_essence_message_input_is_set() -> bool {
    true
}

pub(super) fn deserialize_set_group_essence_message_input_is_set<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_set_group_essence_message_input_is_set)
}
pub(super) fn default_send_group_message_reaction_input_reaction_type() -> String {
    "face".to_string()
}

pub(super) fn deserialize_send_group_message_reaction_input_reaction_type<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_send_group_message_reaction_input_reaction_type,
    )
}
pub(super) fn default_send_group_message_reaction_input_is_add() -> bool {
    true
}

pub(super) fn deserialize_send_group_message_reaction_input_is_add<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_send_group_message_reaction_input_is_add,
    )
}
pub(super) fn default_get_group_notifications_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_get_group_notifications_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_get_group_notifications_input_is_filtered,
    )
}
pub(super) fn default_get_group_notifications_input_limit() -> i32 {
    20
}

pub(super) fn deserialize_get_group_notifications_input_limit<'de, D>(
    deserializer: D,
) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_notifications_input_limit)
}
pub(super) fn default_accept_group_request_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_accept_group_request_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_accept_group_request_input_is_filtered)
}
pub(super) fn default_reject_group_request_input_is_filtered() -> bool {
    false
}

pub(super) fn deserialize_reject_group_request_input_is_filtered<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_reject_group_request_input_is_filtered)
}
pub(super) fn default_upload_group_file_input_parent_folder_id() -> String {
    "/".to_string()
}

pub(super) fn deserialize_upload_group_file_input_parent_folder_id<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_upload_group_file_input_parent_folder_id,
    )
}
pub(super) fn default_get_private_file_download_url_input_is_self_send() -> bool {
    false
}

pub(super) fn deserialize_get_private_file_download_url_input_is_self_send<'de, D>(
    deserializer: D,
) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_get_private_file_download_url_input_is_self_send,
    )
}
pub(super) fn default_get_group_files_input_parent_folder_id() -> String {
    "/".to_string()
}

pub(super) fn deserialize_get_group_files_input_parent_folder_id<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_get_group_files_input_parent_folder_id)
}
pub(super) fn default_move_group_file_input_parent_folder_id() -> String {
    "/".to_string()
}

pub(super) fn deserialize_move_group_file_input_parent_folder_id<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_move_group_file_input_parent_folder_id)
}
pub(super) fn default_move_group_file_input_target_folder_id() -> String {
    "/".to_string()
}

pub(super) fn deserialize_move_group_file_input_target_folder_id<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(deserializer, default_move_group_file_input_target_folder_id)
}
pub(super) fn default_rename_group_file_input_parent_folder_id() -> String {
    "/".to_string()
}

pub(super) fn deserialize_rename_group_file_input_parent_folder_id<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_default_on_null(
        deserializer,
        default_rename_group_file_input_parent_folder_id,
    )
}

// ####################################
// API Endpoint Constants
// ####################################
