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
//! # Placeholders are still emitted
//! Every attachment also contributes a short textual marker. Two reasons: a model without vision
//! must not silently lose the fact that a picture was sent, and conversation history stays readable
//! after an image URL expires.

use kanon_llm::ChatMessage;
use kanon_llm::ContentPart;
use kanon_llm::tool_router::prost_struct_to_json;
use kanon_proto::prost_types;
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::audio_segment;
use kanon_proto::v1::image_segment;
use kanon_proto::v1::message_segment::Segment;
use serde_json::Value;

/// Builds the user message for a conversation turn.
///
/// `vision` is taken from the target model's capabilities: when it is `false`, images stay textual
/// placeholders, because attaching an image part an endpoint rejects would fail the whole turn.
pub fn build_user_message(event: &PipelineEventRequest, vision: bool) -> ChatMessage {
    let mut text = TextBuffer::default();
    let mut images: Vec<ContentPart> = Vec::new();

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
                if vision {
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
                render_custom(&custom.type_name, custom.payload.as_ref(), &mut text);
            }
            None => {}
        }
    }

    let text = if text.is_empty() {
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

/// Renders one custom (platform-preserved) segment into the model text.
fn render_custom(type_name: &str, payload: Option<&prost_types::Struct>, text: &mut TextBuffer) {
    let json = payload
        .map(|payload| prost_struct_to_json(payload.clone()))
        .unwrap_or(Value::Null);
    // Adapters namespace their preserved segments (`milky.forward`); the kind is what matters here.
    let kind = type_name
        .split_once('.')
        .map(|(_, kind)| kind)
        .unwrap_or(type_name);

    let rendered = match kind {
        "forward" => {
            let title = field_str(&json, "title").unwrap_or("合并转发");
            let mut detail = format!("[合并转发: {title}]");
            for key in ["summary", "preview"] {
                if let Some(value) = field_str(&json, key) {
                    detail.push(' ');
                    detail.push_str(value);
                }
            }
            detail
        }
        "file" => {
            let name = field_str(&json, "file_name").unwrap_or("file");
            match field_number(&json, "file_size") {
                Some(size) => format!("[文件: {name} ({size} bytes)]"),
                None => format!("[文件: {name}]"),
            }
        }
        "video" => {
            let duration = field_number(&json, "duration")
                .map(|seconds| format!(" {seconds}s"))
                .unwrap_or_default();
            let url = field_str(&json, "temp_url").unwrap_or_default();
            format!("[视频{duration}]{url}")
        }
        "face" | "market_face" => "[表情]".to_string(),
        // Markdown is text as far as a model is concerned.
        "markdown" => field_str(&json, "content").unwrap_or_default().to_string(),
        "light_app" => {
            let name = field_str(&json, "app_name").unwrap_or("app");
            format!("[应用卡片: {name}]")
        }
        "xml" => "[xml 卡片]".to_string(),
        other => {
            // An unknown kind is still named, and any textual payload it carries is preserved so a
            // newer adapter's segment is not reduced to an opaque tag.
            let mut detail = format!("[{other}]");
            for key in ["text", "content", "summary", "url"] {
                if let Some(value) = field_str(&json, key) {
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
