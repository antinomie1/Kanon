//! Tests for turning inbound segments into the model-visible user message.
//!
//! An adapter reports what arrived; this translation decides what the model actually sees. The
//! cases below cover the media kinds the request called out — images, stickers, quoted media,
//! forwarded messages, audio and files — plus the conservative fallback when a segment produces no
//! readable text.

use kanon_core::pipeline::build_user_message;
use kanon_llm::ModelCapabilities;
use kanon_proto::prost_types;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, ImageSegment, MentionSegment, MessageSegment, PipelineEventRequest,
    RawCustomSegment, ReplySegment, TextSegment, audio_segment, image_segment,
};

/// Capabilities with text enabled and vision set as requested.
fn caps(vision: bool) -> ModelCapabilities {
    ModelCapabilities {
        vision,
        ..ModelCapabilities::default()
    }
}

/// Builds a pipeline event wrapping the given segments.
fn event(segments: Vec<Segment>, raw_text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: "evt".to_string(),
        platform: "test".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "2".to_string(),
        raw_text: raw_text.to_string(),
        segments: segments
            .into_iter()
            .map(|segment| MessageSegment {
                segment: Some(segment),
            })
            .collect(),
        metadata: None,
    }
}

/// Builds a protobuf struct from string fields.
fn string_struct(pairs: &[(&str, &str)]) -> prost_types::Struct {
    prost_types::Struct {
        fields: pairs
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_string(),
                    prost_types::Value {
                        kind: Some(prost_types::value::Kind::StringValue((*value).to_string())),
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn text_and_mentions_render_into_the_prompt() {
    let message = build_user_message(
        &event(
            vec![
                Segment::Mention(MentionSegment {
                    target_user_id: "9".to_string(),
                    display_name: "黑猪AI".to_string(),
                    is_all: false,
                }),
                Segment::Text(TextSegment {
                    content: "你好".to_string(),
                }),
            ],
            "",
        ),
        &caps(false),
    );

    assert_eq!(message.content.as_deref(), Some("@黑猪AI 你好"));
    assert!(!message.has_parts());
}

#[test]
fn a_quoted_message_contributes_its_snippet() {
    let message = build_user_message(
        &event(
            vec![Segment::Reply(ReplySegment {
                target_message_id: "7".to_string(),
                snippet: "原消息".to_string(),
            })],
            "[reply]",
        ),
        &caps(false),
    );

    assert_eq!(message.content.as_deref(), Some("[引用] 原消息"));
}

#[test]
fn images_become_native_parts_only_when_the_model_accepts_them() {
    let image = || {
        Segment::Image(ImageSegment {
            source: Some(image_segment::Source::Url(
                "https://example.invalid/a.png".to_string(),
            )),
            mime_type: Some("image/png".to_string()),
            filename: None,
        })
    };

    let without_vision = build_user_message(&event(vec![image()], "[image]"), &caps(false));
    assert!(
        !without_vision.has_parts(),
        "a text-only model must not receive an image part"
    );
    assert_eq!(without_vision.content.as_deref(), Some("[图片]"));

    let with_vision = build_user_message(&event(vec![image()], "[image]"), &caps(true));
    assert!(with_vision.has_parts());
    assert_eq!(with_vision.content.as_deref(), Some("[图片]"));
    let parts = with_vision.parts.expect("image parts");
    assert_eq!(parts.len(), 1);
    assert_eq!(
        parts[0].resolved_image_url().as_deref(),
        Some("https://example.invalid/a.png")
    );
}

#[test]
fn a_sticker_carries_its_image_when_vision_is_enabled() {
    let sticker = || {
        Segment::Custom(RawCustomSegment {
            type_name: "milky.market_face".to_string(),
            payload: Some(string_struct(&[
                ("emoji_id", "9"),
                ("url", "https://example.invalid/sticker.png"),
            ])),
        })
    };

    let with_vision = build_user_message(&event(vec![sticker()], "[emoji]"), &caps(true));
    assert!(
        with_vision.has_parts(),
        "the sticker image must be attached"
    );
    assert_eq!(with_vision.content.as_deref(), Some("[表情]"));
    assert_eq!(
        with_vision.parts.expect("parts")[0]
            .resolved_image_url()
            .as_deref(),
        Some("https://example.invalid/sticker.png")
    );

    let without_vision = build_user_message(&event(vec![sticker()], "[emoji]"), &caps(false));
    assert!(!without_vision.has_parts());
    assert_eq!(without_vision.content.as_deref(), Some("[表情]"));
}

#[test]
fn forwarded_messages_expand_their_title_and_summary() {
    let message = build_user_message(
        &event(
            vec![Segment::Custom(RawCustomSegment {
                type_name: "milky.forward".to_string(),
                payload: Some(string_struct(&[
                    ("title", "群聊的聊天记录"),
                    ("summary", "共 3 条消息"),
                ])),
            })],
            "[forward:群聊的聊天记录]",
        ),
        &caps(false),
    );

    let content = message.content.unwrap_or_default();
    assert!(
        content.contains("合并转发"),
        "unexpected content: {content}"
    );
    assert!(content.contains("群聊的聊天记录"));
    assert!(content.contains("共 3 条消息"));
}

#[test]
fn audio_and_files_are_described_in_text() {
    let message = build_user_message(
        &event(
            vec![
                Segment::Audio(AudioSegment {
                    source: Some(audio_segment::Source::Url(
                        "https://example.invalid/v.amr".to_string(),
                    )),
                    duration_seconds: Some(4),
                }),
                Segment::Custom(RawCustomSegment {
                    type_name: "milky.file".to_string(),
                    payload: Some(string_struct(&[("file_name", "report.pdf")])),
                }),
            ],
            "",
        ),
        &caps(false),
    );

    let content = message.content.unwrap_or_default();
    assert!(
        content.contains("[语音 4s]"),
        "unexpected content: {content}"
    );
    assert!(content.contains("v.amr"));
    assert!(content.contains("report.pdf"));
}

#[test]
fn text_can_be_disabled_for_an_image_only_model() {
    let capabilities = ModelCapabilities {
        text: false,
        vision: true,
        ..ModelCapabilities::default()
    };
    let message = build_user_message(
        &event(
            vec![Segment::Image(ImageSegment {
                source: Some(image_segment::Source::Url(
                    "https://example.invalid/a.png".to_string(),
                )),
                mime_type: None,
                filename: None,
            })],
            "[image]",
        ),
        &capabilities,
    );

    assert!(
        message.content.as_deref().is_none_or(str::is_empty),
        "an image-only model must not receive the textual projection"
    );
    assert!(message.has_parts());
}

#[test]
fn an_unreadable_event_falls_back_to_the_adapter_rendering() {
    let message = build_user_message(
        &event(
            vec![Segment::Mention(MentionSegment {
                target_user_id: String::new(),
                display_name: String::new(),
                is_all: false,
            })],
            "原始文本",
        ),
        &caps(false),
    );

    // An empty mention target renders as `@`, which is not empty text, so the raw fallback only
    // applies when every segment produced nothing at all.
    assert!(message.content.is_some());

    let empty = build_user_message(&event(Vec::new(), "fallback text"), &caps(false));
    assert_eq!(empty.content.as_deref(), Some("fallback text"));
}
