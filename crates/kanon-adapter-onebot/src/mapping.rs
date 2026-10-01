//! OneBot v11 message translation. Channel IDs are `private:<user>` or `group:<group>`.
//! Unknown segments keep their data under `onebot.<type>`; media without a download URL
//! stays custom because a OneBot cache filename is not a file on the Kanon host.

use base64::Engine;
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, DeliverMessageRequest, ImageSegment, MentionSegment, MessageSegment,
    PipelineEventRequest, RawCustomSegment, ReplySegment, TextSegment, audio_segment, file_segment,
    image_segment, video_segment,
};
use serde_json::{Map, Value, json};

/// Translates conversational events; notices, requests and meta events do not enter the pipeline.
pub fn map_event(platform: &str, value: Value) -> Result<Option<PipelineEventRequest>, String> {
    if string(&value, "post_type")? != "message" {
        return Ok(None);
    }
    let scene = string(&value, "message_type")?;
    let self_id = id(&value["self_id"])?;
    let sender_id = id(&value["user_id"])?;
    let message_id = message_id(&value["message_id"])?;
    let peer_id = match scene {
        "private" => sender_id.clone(),
        "group" => id(&value["group_id"])?,
        other => return Err(format!("unsupported OneBot message_type '{other}'")),
    };
    let channel_id = format!("{scene}:{peer_id}");
    let segments = parse_message(&value["message"])?;
    let raw_text = render(&segments);
    let mentioned = segments.iter().any(|segment| {
        matches!(
            &segment.segment, Some(Segment::Mention(mention))
                if mention.is_all || mention.target_user_id == self_id
        )
    });
    let timestamp = value["time"]
        .as_i64()
        .filter(|time| *time >= 0)
        .ok_or("OneBot time must be a nonnegative integer")?;
    // Protobuf Struct numbers are doubles. Identity fields remain strings throughout the
    // pipeline so large IDs cannot silently change when an event is delivered or deduplicated.
    let mut metadata = json!({
        "onebot.self_id": self_id,
        "onebot.user_id": sender_id,
        "onebot.message_id": message_id,
        "onebot.message_type": scene,
        "onebot.time": timestamp,
        kanon_core::META_CONVERSATION_KIND: scene,
        kanon_core::META_BOT_MENTIONED: mentioned,
        kanon_core::META_TIMESTAMP: timestamp,
    });
    if scene == "group" {
        metadata["onebot.group_id"] = json!(peer_id);
    }
    if let Some(sub_type) = value.get("sub_type").and_then(Value::as_str) {
        metadata["onebot.sub_type"] = json!(sub_type);
    }
    for key in ["nickname", "card", "role"] {
        if let Some(text) = value["sender"].get(key).and_then(Value::as_str) {
            metadata[format!("onebot.sender_{key}")] = json!(text);
        }
    }
    // Platform-neutral name and role, read by the core for speaker labels and command access.
    let name = ["card", "nickname"]
        .into_iter()
        .filter_map(|key| value["sender"][key].as_str())
        .map(str::trim)
        .find(|name| !name.is_empty());
    if let Some(name) = name {
        metadata[kanon_core::META_SENDER_NAME] = json!(name);
    }
    if let Some(role) = value["sender"]["role"]
        .as_str()
        .filter(|role| !role.is_empty())
    {
        metadata[kanon_core::META_SENDER_ROLE] = json!(role);
    }
    Ok(Some(PipelineEventRequest {
        event_id: format!("{platform}:{self_id}:{channel_id}:{sender_id}:{message_id}"),
        platform: platform.into(),
        channel_id,
        sender_id,
        raw_text,
        segments,
        metadata: Some(to_struct(&metadata)?),
    }))
}

/// The OneBot message ID an inbound event quotes, if any.
///
/// OneBot v11 reply segments carry only the target ID, so the quoted content must be fetched
/// with `get_msg` before the event is ingested.
pub fn reply_target(event: &PipelineEventRequest) -> Option<i64> {
    event
        .segments
        .iter()
        .find_map(|segment| match &segment.segment {
            Some(Segment::Reply(reply)) => parse_id(&reply.target_message_id).ok(),
            _ => None,
        })
}

/// Fills the reply segment with the quoted message, as Milky delivers it natively.
///
/// The quote's text becomes the reply snippet; quoted images, stickers and voice follow the reply
/// segment as their own segments so the model can see them, not just a placeholder.
pub fn attach_quote(event: &mut PipelineEventRequest, message: &Value) -> Result<(), String> {
    let quoted = parse_message(message)?;
    let index = event
        .segments
        .iter()
        .position(|segment| matches!(segment.segment, Some(Segment::Reply(_))))
        .ok_or("event has no reply segment")?;
    if let Some(Segment::Reply(reply)) = &mut event.segments[index].segment {
        reply.snippet = render(&quoted);
    }
    let media = quoted
        .into_iter()
        .filter_map(|segment| match segment.segment {
            Some(Segment::Image(_) | Segment::Audio(_)) => Some(segment),
            Some(Segment::Custom(custom)) => sticker(&custom),
            _ => None,
        });
    event
        .segments
        .splice(index + 1..index + 1, media.collect::<Vec<_>>());
    Ok(())
}

/// A market sticker (`mface`) with a rendered image URL, surfaced as an image.
fn sticker(custom: &RawCustomSegment) -> Option<MessageSegment> {
    if custom.type_name != "onebot.mface" {
        return None;
    }
    let url = payload_str(custom, "url").filter(|url| !url.is_empty())?;
    Some(MessageSegment {
        segment: Some(Segment::Image(ImageSegment {
            source: Some(image_segment::Source::Url(url.into())),
            mime_type: None,
            filename: None,
        })),
    })
}

fn payload_str<'a>(custom: &'a RawCustomSegment, key: &str) -> Option<&'a str> {
    match custom.payload.as_ref()?.fields.get(key)?.kind.as_ref()? {
        Kind::StringValue(text) => Some(text),
        _ => None,
    }
}

/// Parses a OneBot message in array or CQ string form into Kanon segments.
fn parse_message(message: &Value) -> Result<Vec<MessageSegment>, String> {
    let wire = match message {
        Value::Array(segments) => segments.clone(),
        Value::String(cq) => parse_cq(cq)?,
        _ => return Err("OneBot message must be an array or CQ string".into()),
    };
    wire.iter().map(incoming).collect()
}

/// Renders the human-readable text of a message; media becomes a short placeholder.
fn render(segments: &[MessageSegment]) -> String {
    let mut text = String::new();
    for segment in segments {
        match segment.segment.as_ref() {
            Some(Segment::Text(segment)) => text.push_str(&segment.content),
            Some(Segment::Mention(mention)) => {
                text.push('@');
                text.push_str(if mention.is_all {
                    "all"
                } else {
                    &mention.target_user_id
                });
            }
            Some(Segment::Image(_)) => text.push_str("[image]"),
            Some(Segment::Audio(_)) => text.push_str("[voice]"),
            Some(Segment::Video(_)) => text.push_str("[video]"),
            Some(Segment::Face(_)) => text.push_str("[face]"),
            Some(Segment::File(file)) => {
                text.push_str("[file:");
                text.push_str(&file.name);
                text.push(']');
            }
            Some(Segment::Custom(custom)) => {
                let kind = custom
                    .type_name
                    .strip_prefix("onebot.")
                    .unwrap_or(&custom.type_name);
                text.push('[');
                text.push_str(kind);
                // A file is only useful to the model with its name.
                if kind == "file"
                    && let Some(name) = payload_str(custom, "name").or(payload_str(custom, "file"))
                {
                    text.push(':');
                    text.push_str(name);
                }
                text.push(']');
            }
            // The reply target belongs to the structured context, not the current utterance.
            Some(Segment::Reply(_)) | None => {}
        }
    }
    text
}

/// Builds the OneBot action and params for a complete outbound message.
pub fn delivery(request: &DeliverMessageRequest) -> Result<(String, Value), String> {
    let (scene, target) = request
        .channel_id
        .split_once(':')
        .ok_or("OneBot channel must be private:<user_id> or group:<group_id>")?;
    let target = parse_id(target)?;
    let (action, key) = match scene {
        "private" => ("send_private_msg", "user_id"),
        "group" => ("send_group_msg", "group_id"),
        _ => return Err("OneBot channel must be private:<user_id> or group:<group_id>".into()),
    };
    if request.segments.is_empty() {
        return Err("cannot send an empty OneBot message".into());
    }
    let message = request
        .segments
        .iter()
        .map(outgoing)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((action.into(), json!({key: target, "message": message})))
}

/// Reads an exact scalar OneBot identifier without conversion through floating point.
///
/// The same numeric contract applies to account IDs and message IDs in events and API data.
pub fn message_id(value: &Value) -> Result<String, String> {
    id(value)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("OneBot '{key}' must be a string"))
}

fn parse_id(value: &str) -> Result<i64, String> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid OneBot integer ID '{value}'"));
    }
    value
        .parse()
        .map_err(|_| format!("OneBot integer ID '{value}' is out of range"))
}

fn id(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(parse_id(text)?.to_string()),
        Value::Number(number) => number
            .as_i64()
            .map(|id| id.to_string())
            .ok_or_else(|| "OneBot ID must be an exact signed 64-bit integer".into()),
        _ => Err("OneBot ID must be an integer or decimal string".into()),
    }
}

fn incoming(value: &Value) -> Result<MessageSegment, String> {
    let kind = string(value, "type")?;
    let data = value
        .get("data")
        .filter(|data| data.is_object())
        .ok_or("OneBot segment data must be an object")?;
    let segment = match kind {
        "text" => Segment::Text(TextSegment {
            content: string(data, "text")?.into(),
        }),
        "at" => {
            let is_all = data["qq"].as_str() == Some("all");
            Segment::Mention(MentionSegment {
                target_user_id: if is_all {
                    String::new()
                } else {
                    id(&data["qq"])?
                },
                display_name: String::new(),
                is_all,
            })
        }
        "reply" => Segment::Reply(ReplySegment {
            target_message_id: id(&data["id"])?,
            snippet: String::new(),
        }),
        "image" if media_url(data).is_some() => Segment::Image(ImageSegment {
            source: Some(image_segment::Source::Url(media_url(data).unwrap().into())),
            mime_type: None,
            filename: data["file"].as_str().map(str::to_owned),
        }),
        "record" if media_url(data).is_some() => Segment::Audio(AudioSegment {
            source: Some(audio_segment::Source::Url(media_url(data).unwrap().into())),
            duration_seconds: None,
        }),
        "" => return Err("OneBot segment type cannot be empty".into()),
        _ => Segment::Custom(RawCustomSegment {
            type_name: format!("onebot.{kind}"),
            payload: Some(to_struct(data)?),
        }),
    };
    Ok(MessageSegment {
        segment: Some(segment),
    })
}

fn media_url(data: &Value) -> Option<&str> {
    data["url"].as_str().filter(|url| !url.is_empty())
}

fn outgoing(segment: &MessageSegment) -> Result<Value, String> {
    let (kind, data) = match segment.segment.as_ref() {
        Some(Segment::Text(text)) => ("text", json!({"text": text.content})),
        Some(Segment::Mention(mention)) => (
            "at",
            json!({"qq": if mention.is_all {
            "all".into()
        } else { parse_id(&mention.target_user_id)?.to_string() }}),
        ),
        // A quote of an ingested event names its full Kanon event ID; the OneBot message ID is
        // its last component.
        Some(Segment::Reply(reply)) => (
            "reply",
            json!({"id": parse_id(reply.target_message_id.rsplit(':').next().unwrap_or_default())?.to_string()}),
        ),
        Some(Segment::Image(image)) => {
            let file = match image.source.as_ref().ok_or("image has no source")? {
                image_segment::Source::Url(url) => url.clone(),
                image_segment::Source::FilePath(path) => file_uri(path),
                image_segment::Source::RawBytes(bytes) => base64_uri(bytes),
            };
            ("image", json!({"file": file}))
        }
        Some(Segment::Audio(audio)) => {
            let file = match audio.source.as_ref().ok_or("audio has no source")? {
                audio_segment::Source::Url(url) => url.clone(),
                audio_segment::Source::FilePath(path) => file_uri(path),
                audio_segment::Source::RawBytes(bytes) => base64_uri(bytes),
            };
            ("record", json!({"file": file}))
        }
        Some(Segment::Video(video)) => {
            let file = match video.source.as_ref().ok_or("video has no source")? {
                video_segment::Source::Url(url) => url.clone(),
                video_segment::Source::FilePath(path) => file_uri(path),
                video_segment::Source::RawBytes(bytes) => base64_uri(bytes),
            };
            ("video", json!({"file": file}))
        }
        // `file` is an extension segment (NapCat, LLOneBot, Lagrange); an implementation without
        // it rejects the message, which surfaces as a delivery error.
        Some(Segment::File(document)) => {
            if document.name.is_empty() {
                return Err("file segment has no name".into());
            }
            let file = match document.source.as_ref().ok_or("file has no source")? {
                file_segment::Source::Url(url) => url.clone(),
                file_segment::Source::FilePath(path) => file_uri(path),
                file_segment::Source::RawBytes(bytes) => base64_uri(bytes),
            };
            ("file", json!({"file": file, "name": document.name}))
        }
        Some(Segment::Face(face)) => ("face", json!({"id": face.id})),
        Some(Segment::Custom(custom)) => {
            let kind = custom
                .type_name
                .strip_prefix("onebot.")
                .filter(|kind| !kind.is_empty())
                .ok_or("custom OneBot segments must be named onebot.<type>")?;
            let data = custom
                .payload
                .as_ref()
                .ok_or("custom OneBot segment has no data")?;
            (kind, from_struct(data))
        }
        None => return Err("message contains an empty segment".into()),
    };
    Ok(json!({"type": kind, "data": data}))
}

fn file_uri(path: &str) -> String {
    if path.starts_with("file://") {
        path.into()
    } else {
        format!("file://{path}")
    }
}

fn base64_uri(bytes: &[u8]) -> String {
    format!(
        "base64://{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Parses CQ syntax before unescaping so escaped brackets never introduce a new segment.
fn parse_cq(mut input: &str) -> Result<Vec<Value>, String> {
    let mut segments = Vec::new();
    while !input.is_empty() {
        let Some(start) = input.find("[CQ:") else {
            segments.push(json!({"type": "text", "data": {"text": unescape(input, false)}}));
            break;
        };
        if start > 0 {
            segments
                .push(json!({"type": "text", "data": {"text": unescape(&input[..start], false)}}));
        }
        let code = &input[start + 4..];
        let end = code.find(']').ok_or("unterminated OneBot CQ code")?;
        let mut parts = code[..end].split(',');
        let kind = parts
            .next()
            .filter(|kind| !kind.is_empty())
            .ok_or("empty CQ type")?;
        let mut data = Map::new();
        for part in parts {
            let (key, value) = part
                .split_once('=')
                .ok_or("CQ parameter must contain '='")?;
            if key.is_empty()
                || data
                    .insert(key.into(), json!(unescape(value, true)))
                    .is_some()
            {
                return Err("CQ parameters must have unique nonempty names".into());
            }
        }
        segments.push(json!({"type": kind, "data": data}));
        input = &code[end + 1..];
    }
    Ok(segments)
}

fn unescape(text: &str, parameter: bool) -> String {
    let text = text.replace("&#91;", "[").replace("&#93;", "]");
    let text = if parameter {
        text.replace("&#44;", ",")
    } else {
        text
    };
    text.replace("&amp;", "&")
}

fn to_struct(value: &Value) -> Result<prost_types::Struct, String> {
    let fields = value
        .as_object()
        .ok_or("protobuf payload must be an object")?;
    Ok(prost_types::Struct {
        fields: fields
            .iter()
            .map(|(key, value)| Ok((key.clone(), to_value(value)?)))
            .collect::<Result<_, String>>()?,
    })
}

fn to_value(value: &Value) -> Result<prost_types::Value, String> {
    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(flag) => Kind::BoolValue(*flag),
        Value::String(text) => Kind::StringValue(text.clone()),
        Value::Number(number) => {
            // Reject unrepresentable custom numbers instead of corrupting an opaque ID.
            if number
                .as_i64()
                .is_some_and(|n| n.unsigned_abs() > (1_u64 << 53))
                || number.as_u64().is_some_and(|n| n > (1_u64 << 53))
            {
                return Err(
                    "OneBot custom numeric value exceeds exact protobuf precision; use a string"
                        .into(),
                );
            }
            Kind::NumberValue(number.as_f64().ok_or("invalid JSON number")?)
        }
        Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.iter().map(to_value).collect::<Result<_, _>>()?,
        }),
        Value::Object(_) => Kind::StructValue(to_struct(value)?),
    };
    Ok(prost_types::Value { kind: Some(kind) })
}

fn from_struct(value: &prost_types::Struct) -> Value {
    Value::Object(
        value
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), from_value(value)))
            .collect(),
    )
}

fn from_value(value: &prost_types::Value) -> Value {
    match value.kind.as_ref() {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::BoolValue(flag)) => json!(flag),
        Some(Kind::StringValue(text)) => json!(text),
        Some(Kind::NumberValue(number))
            if number.fract() == 0.0 && number.abs() <= (1_u64 << 53) as f64 =>
        {
            json!(*number as i64)
        }
        Some(Kind::NumberValue(number)) => json!(number),
        Some(Kind::ListValue(list)) => Value::Array(list.values.iter().map(from_value).collect()),
        Some(Kind::StructValue(value)) => from_struct(value),
    }
}

/// A notice translated for the pipeline, with the account whose name should become its actor.
pub struct Notice {
    /// The notice event; its actor metadata is filled once the name is known.
    pub event: PipelineEventRequest,
    /// Group the actor belongs to, for a group-card lookup.
    pub group_id: Option<i64>,
    /// Account whose display name describes the notice.
    pub actor_id: Option<i64>,
}

/// Translates the notices Kanon reacts to: joins, pokes of the bot, recalls and new friends.
///
/// A recall names the recalled message by the event ID [`map_event`] gave it, so the core can tell
/// whether the model ever saw it.
pub fn map_notice(platform: &str, value: &Value) -> Result<Option<Notice>, String> {
    if value["post_type"].as_str() != Some("notice") {
        return Ok(None);
    }
    let self_id = id(&value["self_id"])?;
    let user_id = id(&value["user_id"])?;
    let group_id = match &value["group_id"] {
        Value::Null => None,
        group => Some(id(group)?),
    };
    let optional = |key: &str| match &value[key] {
        Value::Null => Ok(None),
        other => id(other).map(Some),
    };
    // (kind, conversation sender, actor, recalled message)
    let (kind, sender, actor, target) = match value["notice_type"].as_str().unwrap_or_default() {
        "group_increase" if user_id == self_id => {
            let operator = optional("operator_id")?.filter(|op| op != "0");
            (
                "bot_join",
                operator.clone().unwrap_or_default(),
                operator,
                None,
            )
        }
        "group_increase" => ("member_join", user_id.clone(), Some(user_id.clone()), None),
        "notify"
            if value["sub_type"].as_str() == Some("poke")
                && optional("target_id")?.as_deref() == Some(self_id.as_str()) =>
        {
            ("poke", user_id.clone(), Some(user_id.clone()), None)
        }
        "group_recall" | "friend_recall" => {
            let channel = match &group_id {
                Some(group) => format!("group:{group}"),
                None => format!("private:{user_id}"),
            };
            let target = format!(
                "{platform}:{self_id}:{channel}:{user_id}:{}",
                message_id(&value["message_id"])?
            );
            let operator = optional("operator_id")?.unwrap_or_else(|| user_id.clone());
            ("recall", user_id.clone(), Some(operator), Some(target))
        }
        "friend_add" => ("friend_add", user_id.clone(), Some(user_id.clone()), None),
        _ => return Ok(None),
    };
    let (channel_id, conversation_kind) = match &group_id {
        Some(group) => (format!("group:{group}"), "group"),
        None => (format!("private:{user_id}"), "private"),
    };
    let mut metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_CONVERSATION_KIND: conversation_kind,
        "onebot.self_id": self_id,
        "onebot.notice_type": value["notice_type"],
    });
    if let Some(target) = target {
        metadata[kanon_core::META_NOTICE_TARGET] = json!(target);
    }
    let time = value["time"].as_i64().unwrap_or_default();
    Ok(Some(Notice {
        event: PipelineEventRequest {
            event_id: format!("{platform}:{self_id}:notice:{kind}:{time}:{channel_id}:{sender}"),
            platform: platform.into(),
            channel_id,
            sender_id: sender,
            raw_text: format!("[{kind}]"),
            segments: Vec::new(),
            metadata: Some(to_struct(&metadata)?),
        },
        group_id: group_id.as_deref().map(parse_id).transpose()?,
        actor_id: actor.as_deref().map(parse_id).transpose()?,
    }))
}

/// Records the actor's display name on a notice.
pub fn set_notice_actor(notice: &mut PipelineEventRequest, name: &str) {
    if let Some(metadata) = notice.metadata.as_mut() {
        metadata.fields.insert(
            kanon_core::META_NOTICE_ACTOR.into(),
            prost_types::Value {
                kind: Some(Kind::StringValue(name.into())),
            },
        );
    }
}

/// Translates a friend request or group invitation into a notice the core may accept.
///
/// The OneBot `flag` travels as the request token; join requests from other people are the group
/// admins' business and are not reported.
pub fn map_request(platform: &str, value: &Value) -> Result<Option<PipelineEventRequest>, String> {
    if value["post_type"].as_str() != Some("request") {
        return Ok(None);
    }
    let kind = match (value["request_type"].as_str(), value["sub_type"].as_str()) {
        (Some("friend"), _) => "friend_request",
        (Some("group"), Some("invite")) => "group_invite",
        _ => return Ok(None),
    };
    let flag = string(value, "flag")?;
    let self_id = id(&value["self_id"])?;
    let user_id = id(&value["user_id"])?;
    let channel_id = match kind {
        "group_invite" => format!("group:{}", id(&value["group_id"])?),
        _ => format!("private:{user_id}"),
    };
    let metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_REQUEST_TOKEN: flag,
        kanon_core::META_NOTICE_ACTOR: user_id,
        "onebot.self_id": self_id,
    });
    Ok(Some(PipelineEventRequest {
        event_id: format!("{platform}:{self_id}:request:{kind}:{flag}"),
        platform: platform.into(),
        channel_id,
        sender_id: user_id,
        raw_text: format!("[{kind}]"),
        segments: Vec::new(),
        metadata: Some(to_struct(&metadata)?),
    }))
}

/// The OneBot action and parameters that accept a request notice produced by [`map_request`].
pub fn accept_request_call(event: &PipelineEventRequest) -> Result<(&'static str, Value), String> {
    let field = |key: &str| match event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.fields.get(key))
        .and_then(|value| value.kind.as_ref())
    {
        Some(Kind::StringValue(text)) => Ok(text.clone()),
        _ => Err(format!("request event lacks {key}")),
    };
    let flag = field(kanon_core::META_REQUEST_TOKEN)?;
    match field(kanon_core::META_NOTICE)?.as_str() {
        "friend_request" => Ok((
            "set_friend_add_request",
            json!({"flag": flag, "approve": true}),
        )),
        "group_invite" => Ok((
            "set_group_add_request",
            json!({"flag": flag, "sub_type": "invite", "approve": true}),
        )),
        other => Err(format!("'{other}' is not a request OneBot can accept")),
    }
}

/// IDs of merged forwards in an event whose content has not been fetched yet.
pub fn forward_ids(event: &PipelineEventRequest) -> Vec<String> {
    event
        .segments
        .iter()
        .filter_map(|segment| match &segment.segment {
            Some(Segment::Custom(custom)) if custom.type_name == "onebot.forward" => {
                payload_str(custom, "id").map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

/// Stores a fetched merged forward in its segment as `messages: [{sender, text, images}]`.
///
/// Implementations disagree on the response shape: the v11 standard returns `message` as `node`
/// segments, NapCat and LLOneBot return `messages` as message objects. Both are read.
pub fn attach_forward(
    event: &mut PipelineEventRequest,
    forward_id: &str,
    data: &Value,
) -> Result<(), String> {
    let entries: Vec<(String, &Value)> = if let Some(messages) = data["messages"].as_array() {
        messages
            .iter()
            .map(|entry| {
                let sender = &entry["sender"];
                let name = sender["card"]
                    .as_str()
                    .filter(|card| !card.is_empty())
                    .or_else(|| sender["nickname"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| sender["user_id"].to_string());
                let content = if entry["message"].is_null() {
                    &entry["content"]
                } else {
                    &entry["message"]
                };
                (name, content)
            })
            .collect()
    } else if let Some(nodes) = data["message"].as_array() {
        nodes
            .iter()
            .map(|node| {
                let data = &node["data"];
                let name = data["nickname"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| data["user_id"].to_string());
                (name, &data["content"])
            })
            .collect()
    } else {
        return Err("get_forward_msg returned neither messages nor message".into());
    };
    let mut messages = Vec::with_capacity(entries.len());
    for (sender, content) in entries {
        let segments = parse_message(content)?;
        let images: Vec<String> = segments
            .iter()
            .filter_map(|segment| match &segment.segment {
                Some(Segment::Image(ImageSegment {
                    source: Some(image_segment::Source::Url(url)),
                    ..
                })) => Some(url.clone()),
                _ => None,
            })
            .collect();
        messages.push(json!({"sender": sender, "text": render(&segments), "images": images}));
    }
    let segment = event
        .segments
        .iter_mut()
        .find_map(|segment| match &mut segment.segment {
            Some(Segment::Custom(custom))
                if custom.type_name == "onebot.forward"
                    && payload_str(custom, "id") == Some(forward_id) =>
            {
                Some(custom)
            }
            _ => None,
        })
        .ok_or("event has no such forward segment")?;
    segment
        .payload
        .get_or_insert_with(Default::default)
        .fields
        .insert("messages".into(), to_value(&Value::Array(messages))?);
    Ok(())
}

/// Users @-mentioned in a group message whose display name is still unknown.
pub fn unnamed_mentions(event: &PipelineEventRequest) -> Vec<i64> {
    let mut ids: Vec<i64> = event
        .segments
        .iter()
        .filter_map(|segment| match &segment.segment {
            Some(Segment::Mention(mention))
                if !mention.is_all && mention.display_name.is_empty() =>
            {
                parse_id(&mention.target_user_id).ok()
            }
            _ => None,
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Fills a mentioned user's display name, so the model reads `@Alice` instead of a QQ number.
pub fn name_mention(event: &mut PipelineEventRequest, user_id: i64, name: &str) {
    let target = user_id.to_string();
    for segment in &mut event.segments {
        if let Some(Segment::Mention(mention)) = &mut segment.segment
            && mention.target_user_id == target
        {
            mention.display_name = name.to_owned();
        }
    }
}

/// Whether a message references something that must be fetched before the model can read it.
pub fn needs_lookup(event: &PipelineEventRequest) -> bool {
    reply_target(event).is_some()
        || !forward_ids(event).is_empty()
        || (group_of(event).is_some() && !unnamed_mentions(event).is_empty())
}

/// The group a `group:<id>` channel names.
pub fn group_of(event: &PipelineEventRequest) -> Option<i64> {
    event
        .channel_id
        .strip_prefix("group:")
        .and_then(|id| parse_id(id).ok())
}
