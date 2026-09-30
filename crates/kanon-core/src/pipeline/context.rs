//! Builds the model-visible user message from an inbound event's segments.
//!
//! # Why this is not just `raw_text`
//! `raw_text` exists so an operator can read a message and so slash commands can be parsed; it
//! reduces media to placeholders (`[image]`, `[file:x]`). That is enough for a text-only model, but
//! a vision-capable model should receive the picture itself, and a forwarded-message card should
//! reach the model with the summary it carries rather than the bare word `forward`. This module is
//! the single place that decides how each segment becomes model input, so every adapter — present
//! and future — inherits the same treatment as soon as it reports segments.
//!
//! # Notices and merged forwards
//! A notice event (see [`crate::notice`]) becomes one bracketed line describing what happened, so
//! the model answers a newcomer or a poke in its own persona. A merged forward whose adapter
//! fetched its content (`messages: [{sender, text, images}]` in the custom payload) is expanded
//! into one line per message when the context policy allows it; its pictures are attached for a
//! vision model, capped so a forwarded photo album cannot flood the request.
//!
//! # Placeholders are still emitted
//! Every attachment also contributes a short textual marker. Two reasons: a model without vision
//! must not silently lose the fact that a picture was sent, and conversation history stays readable
//! after an image URL expires.

use kanon_llm::tool_router::prost_struct_to_json;
use kanon_llm::{ChatMessage, ContentPart, ModelCapabilities};
use kanon_proto::prost_types;
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::audio_segment;
use kanon_proto::v1::image_segment;
use kanon_proto::v1::message_segment::Segment;
use serde_json::Value;

use crate::conversation::{ContextPolicy, META_TIMESTAMP, META_TIMESTAMP_TEXT};
use crate::notice::{META_NOTICE_ACTOR, NoticeKind, metadata_str};

/// Forwarded messages shown at most; longer forwards say how many were left out.
const MAX_FORWARD_MESSAGES: usize = 50;

/// Characters kept of one forwarded message.
const MAX_FORWARD_TEXT: usize = 500;

/// Pictures attached from one merged forward.
const MAX_FORWARD_IMAGES: usize = 4;

/// Builds the user message for a conversation turn.
///
/// `capabilities` come from the target model's catalog entry: text is included only when the model
/// accepts it, and images are attached only when it accepts images, because sending a modality an
/// endpoint rejects fails the whole turn. `context_policy` decides whether the sender id and the
/// message time are prepended; both are off by default.
pub fn build_user_message(
    event: &PipelineEventRequest,
    capabilities: &ModelCapabilities,
    context_policy: &ContextPolicy,
) -> ChatMessage {
    let mut text = TextBuffer::default();
    let mut images: Vec<ContentPart> = Vec::new();

    if capabilities.text {
        if context_policy.include_channel_id && !event.channel_id.trim().is_empty() {
            text.push(&format!("[群号: {}]", event.channel_id.trim()));
        }
        if context_policy.include_sender_id && !event.sender_id.trim().is_empty() {
            text.push(&format!("[发送者: {}]", event.sender_id.trim()));
        }
        if context_policy.include_timestamp
            && let Some(timestamp) = event_timestamp(event.metadata.as_ref())
        {
            text.push(&format!("[时间: {timestamp}]"));
        }
        if let Some(line) = notice_line(event.metadata.as_ref()) {
            text.push(&line);
        }
    }

    for segment in &event.segments {
        match segment.segment.as_ref() {
            Some(Segment::Text(segment)) => text.push(&segment.content),
            Some(Segment::Mention(mention)) => {
                if mention.is_all {
                    text.push("@all");
                } else if mention.display_name.trim().is_empty() {
                    text.push(&format!("@{}", mention.target_user_id));
                } else {
                    text.push(&format!("@{}", mention.display_name));
                }
            }
            Some(Segment::Reply(reply)) => {
                if !reply.snippet.trim().is_empty() {
                    text.push(&format!("[引用] {}", reply.snippet.trim()));
                }
            }
            Some(Segment::Image(image)) => {
                text.push("[图片]");
                if capabilities.vision {
                    match image_part(image.source.as_ref(), image.mime_type.as_deref()) {
                        Some(part) => images.push(part),
                        None => tracing::debug!(
                            "Inbound image carries no URL or file path; kept as a text placeholder"
                        ),
                    }
                }
            }
            Some(Segment::Audio(audio)) => {
                let duration = audio
                    .duration_seconds
                    .map(|seconds| format!(" {seconds}s"))
                    .unwrap_or_default();
                text.push(&format!(
                    "[语音{duration}]{}",
                    media_suffix(&audio_url(audio))
                ));
            }
            Some(Segment::Custom(custom)) => {
                let json = custom
                    .payload
                    .as_ref()
                    .map(|payload| prost_struct_to_json(payload.clone()))
                    .unwrap_or(Value::Null);
                let expand = context_policy.expand_forward;
                render_custom(&custom.type_name, &json, expand, &mut text);
                // A sticker is an image, and so are pictures inside an expanded forward. Their URLs
                // are attached too, so the model can actually see them instead of only being told
                // that a picture was there.
                if capabilities.vision {
                    images.extend(custom_media_parts(&custom.type_name, &json, expand));
                }
            }
            None => {}
        }
    }

    let text = if !capabilities.text {
        // An image-only endpoint must not receive a textual projection it would reject.
        String::new()
    } else if text.is_empty() {
        // No segment produced anything readable. Falling back to the adapter's rendering keeps an
        // event shape this release does not understand from turning into an empty prompt.
        event.raw_text.clone()
    } else {
        text.finish()
    };

    if images.is_empty() {
        ChatMessage::user(text)
    } else {
        ChatMessage::user_multimodal(text, images)
    }
}

/// Describes a notice event in one line, or `None` for an ordinary message.
fn notice_line(metadata: Option<&prost_types::Struct>) -> Option<String> {
    let actor = metadata_str(metadata, META_NOTICE_ACTOR);
    Some(match NoticeKind::from_metadata(metadata)? {
        NoticeKind::MemberJoin => format!("[事件] {} 加入了群聊", actor.unwrap_or("新成员")),
        NoticeKind::BotJoin => match actor {
            Some(actor) => format!("[事件] 你刚被 {actor} 拉进了这个群"),
            None => "[事件] 你刚加入了这个群".to_string(),
        },
        NoticeKind::FriendAdd => format!("[事件] {} 添加你为好友", actor.unwrap_or("对方")),
        NoticeKind::Poke => format!("[事件] {} 戳了戳你", actor.unwrap_or("有人")),
        // Recalls never reach the model as a turn of their own.
        NoticeKind::Recall => return None,
    })
}

/// Returns the images a custom (platform-preserved) segment carries.
///
/// Only kinds whose payload is documented to hold image URLs are read: guessing from a generic
/// `url` field would attach a web page as if it were a picture.
fn custom_media_parts(type_name: &str, json: &Value, expand_forward: bool) -> Vec<ContentPart> {
    match custom_kind(type_name) {
        // A Milky market-face sticker carries the rendered image URL; a QQ built-in face does not.
        "market_face" => field_str(json, "url")
            .map(|url| vec![ContentPart::image_url(url, None)])
            .unwrap_or_default(),
        "forward" if expand_forward => forward_messages(json)
            .iter()
            .flat_map(|message| message["images"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|url| !url.trim().is_empty())
            .take(MAX_FORWARD_IMAGES)
            .map(|url| ContentPart::image_url(url, None))
            .collect(),
        _ => Vec::new(),
    }
}

/// The messages an adapter fetched into a merged-forward payload.
fn forward_messages(json: &Value) -> &[Value] {
    json["messages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Strips the adapter namespace from a custom segment type name.
fn custom_kind(type_name: &str) -> &str {
    type_name
        .split_once('.')
        .map(|(_, kind)| kind)
        .unwrap_or(type_name)
}

/// Renders one custom (platform-preserved) segment into the model text.
fn render_custom(type_name: &str, json: &Value, expand_forward: bool, text: &mut TextBuffer) {
    // Adapters namespace their preserved segments (`milky.forward`); the kind is what matters here.
    let kind = custom_kind(type_name);

    let rendered = match kind {
        "forward" if expand_forward && !forward_messages(json).is_empty() => {
            let title = field_str(json, "title").unwrap_or("合并转发");
            let messages = forward_messages(json);
            let mut detail = format!("[合并转发: {title}]");
            for message in messages.iter().take(MAX_FORWARD_MESSAGES) {
                let sender = field_str(message, "sender").unwrap_or("某人");
                let body: String = field_str(message, "text")
                    .unwrap_or_default()
                    .chars()
                    .take(MAX_FORWARD_TEXT)
                    .collect();
                detail.push_str(&format!("\n{sender}: {body}"));
            }
            if messages.len() > MAX_FORWARD_MESSAGES {
                detail.push_str(&format!(
                    "\n（共 {} 条，仅显示前 {MAX_FORWARD_MESSAGES} 条）",
                    messages.len()
                ));
            }
            detail
        }
        "forward" => {
            let title = field_str(json, "title").unwrap_or("合并转发");
            let mut detail = format!("[合并转发: {title}]");
            for key in ["summary", "preview"] {
                if let Some(value) = field_str(json, key) {
                    detail.push(' ');
                    detail.push_str(value);
                }
            }
            detail
        }
        "file" => {
            let name = field_str(json, "file_name").unwrap_or("file");
            match field_number(json, "file_size") {
                Some(size) => format!("[文件: {name} ({size} bytes)]"),
                None => format!("[文件: {name}]"),
            }
        }
        "video" => {
            let duration = field_number(json, "duration")
                .map(|seconds| format!(" {seconds}s"))
                .unwrap_or_default();
            let url = field_str(json, "temp_url").unwrap_or_default();
            format!("[视频{duration}]{url}")
        }
        "face" | "market_face" => "[表情]".to_string(),
        // Markdown is text as far as a model is concerned.
        "markdown" => field_str(json, "content").unwrap_or_default().to_string(),
        "light_app" => {
            let name = field_str(json, "app_name").unwrap_or("app");
            format!("[应用卡片: {name}]")
        }
        "xml" => "[xml 卡片]".to_string(),
        other => {
            // An unknown kind is still named, and any textual payload it carries is preserved so a
            // newer adapter's segment is not reduced to an opaque tag.
            let mut detail = format!("[{other}]");
            for key in ["text", "content", "summary", "url"] {
                if let Some(value) = field_str(json, key) {
                    detail.push(' ');
                    detail.push_str(value);
                    break;
                }
            }
            detail
        }
    };

    text.push(&rendered);
}

/// Builds an image part from a Kanon media source.
fn image_part(
    source: Option<&image_segment::Source>,
    mime_type: Option<&str>,
) -> Option<ContentPart> {
    match source? {
        image_segment::Source::Url(url) => Some(ContentPart::image_url(
            url.clone(),
            mime_type.map(str::to_string),
        )),
        image_segment::Source::FilePath(path) => Some(ContentPart::image_file(
            path.clone(),
            mime_type.map(str::to_string),
        )),
        // Inline bytes would have to be materialized on disk before a provider could read them;
        // the placeholder already recorded, so the bytes are not silently dropped from the text.
        image_segment::Source::RawBytes(_) => None,
    }
}

/// URL of an inbound audio segment, when it carries one.
fn audio_url(audio: &kanon_proto::v1::AudioSegment) -> Option<String> {
    match audio.source.as_ref()? {
        audio_segment::Source::Url(url) => Some(url.clone()),
        audio_segment::Source::FilePath(path) => Some(path.clone()),
        audio_segment::Source::RawBytes(_) => None,
    }
}

/// Renders an optional media reference as a trailing text fragment.
fn media_suffix(url: &Option<String>) -> String {
    match url {
        Some(url) if !url.trim().is_empty() => format!(" {url}"),
        _ => String::new(),
    }
}

/// Reads the event timestamp, preferring an adapter-formatted string over raw Unix seconds.
fn event_timestamp(metadata: Option<&prost_types::Struct>) -> Option<String> {
    let metadata = metadata?;
    let field = |key: &str| {
        metadata
            .fields
            .get(key)
            .and_then(|value| value.kind.as_ref())
    };

    if let Some(prost_types::value::Kind::StringValue(text)) = field(META_TIMESTAMP_TEXT)
        && !text.trim().is_empty()
    {
        return Some(text.trim().to_string());
    }

    match field(META_TIMESTAMP) {
        // Rendered in the host's own timezone, which is what a participant reading the
        // conversation expects; the platform's timezone is not reported by the IM protocols.
        Some(prost_types::value::Kind::NumberValue(seconds)) => {
            Some(crate::time::format_local(*seconds as i64))
        }
        _ => None,
    }
}

/// Reads a string field out of a decoded custom payload.
fn field_str<'a>(json: &'a Value, key: &str) -> Option<&'a str> {
    json.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

/// Reads a numeric field out of a decoded custom payload, rendered without a fractional part.
fn field_number(json: &Value, key: &str) -> Option<String> {
    let number = json.get(key)?.as_f64()?;
    if number.fract() == 0.0 {
        Some(format!("{}", number as i64))
    } else {
        Some(format!("{number}"))
    }
}

/// Accumulates text fragments with a single space between them and no leading or trailing space.
#[derive(Default)]
struct TextBuffer {
    /// Rendered fragments in arrival order.
    fragments: Vec<String>,
}

impl TextBuffer {
    /// Appends one fragment, ignoring empty ones.
    fn push(&mut self, fragment: &str) {
        let trimmed = fragment.trim();
        if !trimmed.is_empty() {
            self.fragments.push(trimmed.to_string());
        }
    }

    /// Whether nothing was appended.
    fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// Joins the fragments into the final prompt text.
    fn finish(self) -> String {
        self.fragments.join(" ")
    }
}
