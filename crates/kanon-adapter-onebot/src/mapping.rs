//! OneBot v11 message translation. Channel IDs are `private:<user>` or `group:<group>`.
//! Unknown segments keep their data under `onebot.<type>`; media without a download URL
//! stays custom because a OneBot cache filename is not a file on the Kanon host.

use base64::Engine;
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, DeliverMessageRequest, ImageSegment, MentionSegment, MessageSegment,
    PipelineEventRequest, RawCustomSegment, ReplySegment, TextSegment, audio_segment,
    image_segment,
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
    let wire = match &value["message"] {
        Value::Array(segments) => segments.clone(),
        Value::String(cq) => parse_cq(cq)?,
        _ => return Err("OneBot message must be an array or CQ string".into()),
    };
    let mut segments = Vec::with_capacity(wire.len());
    let mut raw_text = String::new();
    for segment in &wire {
        let mapped = incoming(segment)?;
        match mapped.segment.as_ref() {
            Some(Segment::Text(text)) => raw_text.push_str(&text.content),
            Some(Segment::Mention(mention)) => {
                raw_text.push('@');
                raw_text.push_str(if mention.is_all {
                    "all"
                } else {
                    &mention.target_user_id
                });
            }
            Some(Segment::Image(_)) => raw_text.push_str("[image]"),
            Some(Segment::Audio(_)) => raw_text.push_str("[voice]"),
            Some(Segment::Custom(custom)) => {
                raw_text.push('[');
                raw_text.push_str(
                    custom
                        .type_name
                        .strip_prefix("onebot.")
                        .unwrap_or(&custom.type_name),
                );
                raw_text.push(']');
            }
            // The reply target belongs to the structured context, not the current utterance.
            Some(Segment::Reply(_)) | None => {}
        }
        segments.push(mapped);
    }
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
        Some(Segment::Reply(reply)) => (
            "reply",
            json!({"id": parse_id(&reply.target_message_id)?.to_string()}),
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
