//! Milky notices, recalled messages, merged forwards and contact requests.

use super::*;

/// A notice translated for the pipeline, with the account whose name should become its actor.
pub struct Notice {
    /// The notice event; its actor metadata is filled once the name is known.
    pub event: PipelineEventRequest,
    /// Group the actor belongs to, for a group-card lookup.
    pub group_id: Option<i64>,
    /// Account whose display name describes the notice.
    pub actor_id: Option<i64>,
}

/// Translates the notices Kanon reacts to: joins, nudges of the bot and recalls.
///
/// A recall names the recalled message by the event ID [`inbound_message`] gave it, so the core
/// can tell whether the model ever saw it.
pub fn map_notice(platform: &str, event: &Event) -> Option<Notice> {
    let self_id = event.self_id();
    // (kind, scene, peer, conversation sender, actor, recalled message)
    let (kind, scene, peer, sender, actor, target) = match event {
        Event::GroupMemberIncrease { data, .. } if data.user_id == self_id => {
            let inviter = data.invitor_id.or(data.operator_id);
            (
                "bot_join",
                ChannelScene::Group,
                data.group_id,
                inviter.unwrap_or_default(),
                inviter,
                None,
            )
        }
        Event::GroupMemberIncrease { data, .. } => (
            "member_join",
            ChannelScene::Group,
            data.group_id,
            data.user_id,
            Some(data.user_id),
            None,
        ),
        Event::GroupNudge { data, .. } if data.receiver_id == self_id => (
            "poke",
            ChannelScene::Group,
            data.group_id,
            data.sender_id,
            Some(data.sender_id),
            None,
        ),
        Event::FriendNudge { data, .. } if data.is_self_receive && !data.is_self_send => (
            "poke",
            ChannelScene::Friend,
            data.user_id,
            data.user_id,
            Some(data.user_id),
            None,
        ),
        Event::MessageRecall { data, .. } => {
            let scene = match data.message_scene.as_str() {
                "group" => ChannelScene::Group,
                "temp" => ChannelScene::Temp,
                _ => ChannelScene::Friend,
            };
            let target = format!(
                "{platform}:{self_id}:{}:{}:{}",
                channel_id(scene, data.peer_id),
                data.sender_id,
                data.message_seq
            );
            (
                "recall",
                scene,
                data.peer_id,
                data.sender_id,
                Some(data.operator_id),
                Some(target),
            )
        }
        _ => return None,
    };
    let channel = channel_id(scene, peer);
    let group_id = (scene == ChannelScene::Group).then_some(peer);
    let mut metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_CONVERSATION_KIND: if group_id.is_some() { "group" } else { "private" },
        META_EVENT_TYPE: event.event_type(),
    });
    if let Some(target) = target {
        metadata[kanon_core::META_NOTICE_TARGET] = json!(target);
    }
    let time = match event {
        Event::GroupMemberIncrease { time, .. }
        | Event::GroupNudge { time, .. }
        | Event::FriendNudge { time, .. }
        | Event::MessageRecall { time, .. } => *time,
        _ => 0,
    };
    Some(Notice {
        event: PipelineEventRequest {
            event_id: format!("{platform}:{self_id}:notice:{kind}:{time}:{channel}:{sender}"),
            platform: platform.to_string(),
            channel_id: channel,
            sender_id: if sender == 0 {
                String::new()
            } else {
                sender.to_string()
            },
            raw_text: format!("[{kind}]"),
            segments: Vec::new(),
            metadata: Some(json_to_struct(&metadata)),
        },
        group_id,
        actor_id: actor.filter(|id| *id != 0),
    })
}

/// Records the actor's display name on a notice.
pub fn set_notice_actor(notice: &mut PipelineEventRequest, name: &str) {
    if let Some(metadata) = notice.metadata.as_mut() {
        metadata.fields.insert(
            kanon_core::META_NOTICE_ACTOR.into(),
            kanon_proto::json::json_to_prost_value(&json!(name)),
        );
    }
}

/// IDs of merged forwards in an event whose content has not been fetched yet.
pub fn forward_ids(event: &PipelineEventRequest) -> Vec<String> {
    event
        .segments
        .iter()
        .filter_map(|segment| match &segment.segment {
            Some(Segment::Custom(custom))
                if custom.type_name == format!("{CUSTOM_SEGMENT_PREFIX}forward") =>
            {
                match custom
                    .payload
                    .as_ref()?
                    .fields
                    .get("forward_id")?
                    .kind
                    .as_ref()?
                {
                    prost_types::value::Kind::StringValue(id) => Some(id.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// Stores a fetched merged forward in its segment as `messages: [{sender, text, images}]`.
pub fn attach_forward(
    event: &mut PipelineEventRequest,
    forward_id: &str,
    messages: &[IncomingForwardedMessage],
) -> Result<(), String> {
    let rendered: Vec<Value> = messages
        .iter()
        .map(|message| {
            let images: Vec<&str> = message
                .segments
                .iter()
                .filter_map(|segment| match segment {
                    IncomingSegment::Image(image) => Some(image.temp_url.as_str()),
                    _ => None,
                })
                .collect();
            json!({
                "sender": message.sender_name,
                "text": render_text(&message.segments),
                "images": images,
            })
        })
        .collect();
    let target = forward_id.to_string();
    let custom = event
        .segments
        .iter_mut()
        .find_map(|segment| match &mut segment.segment {
            Some(Segment::Custom(custom))
                if custom.type_name == format!("{CUSTOM_SEGMENT_PREFIX}forward")
                    && custom
                        .payload
                        .as_ref()
                        .and_then(|payload| payload.fields.get("forward_id"))
                        .and_then(|value| value.kind.as_ref())
                        == Some(&prost_types::value::Kind::StringValue(target.clone())) =>
            {
                Some(custom)
            }
            _ => None,
        })
        .ok_or_else(|| format!("event has no forward segment {forward_id}"))?;
    custom
        .payload
        .get_or_insert_with(Default::default)
        .fields
        .insert(
            "messages".into(),
            kanon_proto::json::json_to_prost_value(&Value::Array(rendered)),
        );
    Ok(())
}

/// Translates a friend request or group invitation into a notice the core may accept.
///
/// The request token names what [`accept_request_input`] needs: the initiator's UID for a friend
/// request, the group and invitation sequence for an invitation.
pub fn map_request(platform: &str, event: &Event) -> Option<PipelineEventRequest> {
    let self_id = event.self_id();
    let (kind, channel, sender, token) = match event {
        Event::FriendRequest { data, .. } => (
            "friend_request",
            channel_id(ChannelScene::Friend, data.initiator_id),
            data.initiator_id,
            format!("friend:{}", data.initiator_uid),
        ),
        Event::GroupInvitation { data, .. } => (
            "group_invite",
            channel_id(ChannelScene::Group, data.group_id),
            data.initiator_id,
            format!("group:{}:{}", data.group_id, data.invitation_seq),
        ),
        _ => return None,
    };
    let metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_REQUEST_TOKEN: token,
        kanon_core::META_NOTICE_ACTOR: sender.to_string(),
        META_EVENT_TYPE: event.event_type(),
    });
    Some(PipelineEventRequest {
        event_id: format!("{platform}:{self_id}:request:{token}"),
        platform: platform.to_string(),
        channel_id: channel,
        sender_id: sender.to_string(),
        raw_text: format!("[{kind}]"),
        segments: Vec::new(),
        metadata: Some(json_to_struct(&metadata)),
    })
}

/// What a request notice's token asks the Milky API to accept.
pub enum AcceptRequest {
    /// `accept_friend_request` for this initiator UID.
    Friend(String),
    /// `accept_group_invitation` for this group and invitation sequence.
    Group(i64, i64),
}

/// Reads the request token [`map_request`] wrote.
pub fn accept_request_input(event: &PipelineEventRequest) -> Result<AcceptRequest, String> {
    let token = match event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.fields.get(kanon_core::META_REQUEST_TOKEN))
        .and_then(|value| value.kind.as_ref())
    {
        Some(prost_types::value::Kind::StringValue(token)) => token.as_str(),
        _ => return Err("request event lacks its token".into()),
    };
    if let Some(uid) = token.strip_prefix("friend:").filter(|uid| !uid.is_empty()) {
        return Ok(AcceptRequest::Friend(uid.to_string()));
    }
    let parsed = token.strip_prefix("group:").and_then(|rest| {
        let (group, seq) = rest.split_once(':')?;
        Some((group.parse().ok()?, seq.parse().ok()?))
    });
    match parsed {
        Some((group, seq)) => Ok(AcceptRequest::Group(group, seq)),
        None => Err(format!("'{token}' is not a Milky request token")),
    }
}
