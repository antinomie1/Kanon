//! Tests for turning inbound segments into the model-visible user message.
//!
//! An adapter reports what arrived; this translation decides what the model actually sees. The
//! cases below cover the media kinds the request called out — images, stickers, quoted media,
//! forwarded messages, audio and files — plus the conservative fallback when a segment produces no
//! readable text.

use kanon_core::ContextPolicy;
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

/// Calls the builder with the node default context policy (no sender id, no timestamp).
fn build_with_defaults(
    event: &PipelineEventRequest,
    capabilities: &ModelCapabilities,
) -> kanon_llm::ChatMessage {
    build_user_message(event, capabilities, &ContextPolicy::default())
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
    let message = build_with_defaults(
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
    let message = build_with_defaults(
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

    let without_vision = build_with_defaults(&event(vec![image()], "[image]"), &caps(false));
    assert!(
        !without_vision.has_parts(),
        "a text-only model must not receive an image part"
    );
    assert_eq!(without_vision.content.as_deref(), Some("[图片]"));

    let with_vision = build_with_defaults(&event(vec![image()], "[image]"), &caps(true));
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

    let with_vision = build_with_defaults(&event(vec![sticker()], "[emoji]"), &caps(true));
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

    let without_vision = build_with_defaults(&event(vec![sticker()], "[emoji]"), &caps(false));
    assert!(!without_vision.has_parts());
    assert_eq!(without_vision.content.as_deref(), Some("[表情]"));
}

#[test]
fn forwarded_messages_expand_their_title_and_summary() {
    let message = build_with_defaults(
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

/// A forward whose content the adapter fetched, as `messages: [{sender, text, images}]`.
fn fetched_forward() -> Segment {
    let payload = serde_json::json!({
        "forward_id": "f1",
        "title": "群聊的聊天记录",
        "summary": "查看 2 条转发消息",
        "messages": [
            {"sender": "张三", "text": "明天几点集合", "images": []},
            {"sender": "李四", "text": "[image] 八点，看图", "images": ["https://img/map.png"]},
        ],
    });
    Segment::Custom(RawCustomSegment {
        type_name: "onebot.forward".to_string(),
        payload: kanon_llm::tool_router::json_to_prost_struct(&payload),
    })
}

#[test]
fn a_fetched_forward_is_expanded_with_its_pictures() {
    let message = build_with_defaults(&event(vec![fetched_forward()], "[forward]"), &caps(true));

    assert_eq!(
        message.content.as_deref(),
        Some("[合并转发: 群聊的聊天记录]\n张三: 明天几点集合\n李四: [image] 八点，看图")
    );
    assert_eq!(
        message.parts.as_ref().map(Vec::len),
        Some(1),
        "the forwarded picture is attached"
    );
}

#[test]
fn forward_expansion_can_be_turned_off() {
    let policy = ContextPolicy {
        expand_forward: false,
        ..ContextPolicy::default()
    };
    let message = build_user_message(
        &event(vec![fetched_forward()], "[forward]"),
        &caps(true),
        &policy,
    );

    assert_eq!(
        message.content.as_deref(),
        Some("[合并转发: 群聊的聊天记录] 查看 2 条转发消息")
    );
    assert!(
        !message.has_parts(),
        "no forwarded pictures when expansion is off"
    );
}

#[test]
fn audio_and_files_are_described_in_text() {
    let message = build_with_defaults(
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
    let message = build_with_defaults(
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
    let message = build_with_defaults(
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

    let empty = build_with_defaults(&event(Vec::new(), "fallback text"), &caps(false));
    assert_eq!(empty.content.as_deref(), Some("fallback text"));
}

#[test]
fn metadata_does_not_replace_a_raw_text_only_message() {
    let mut event = event(Vec::new(), "  original body  ");
    event.metadata = Some(string_struct(&[(
        kanon_core::META_TIMESTAMP_TEXT,
        "2026-10-04 12:30:00",
    )]));
    let policies = [
        ContextPolicy {
            include_channel_id: true,
            ..Default::default()
        },
        ContextPolicy {
            include_sender_id: true,
            ..Default::default()
        },
        ContextPolicy {
            include_timestamp: true,
            ..Default::default()
        },
    ];
    for policy in policies {
        let content = build_user_message(&event, &caps(false), &policy)
            .content
            .unwrap();
        assert!(content.starts_with('['), "metadata is missing: {content}");
        assert!(
            content.ends_with(&event.raw_text),
            "metadata must not hide or rewrite the fallback body: {content}"
        );
        assert_eq!(content.matches("original body").count(), 1);
    }
}

#[test]
fn the_sender_id_and_time_are_included_only_when_the_policy_asks() {
    let mut event = event(
        vec![Segment::Text(TextSegment {
            content: "hi".to_string(),
        })],
        "hi",
    );
    event.sender_id = "1705702687".to_string();
    event.metadata = Some(prost_types::Struct {
        fields: [(
            kanon_core::META_TIMESTAMP.to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(0.0)),
            },
        )]
        .into_iter()
        .collect(),
    });

    // Default: neither extra is added, so a private id never reaches the model by accident.
    let off = build_user_message(&event, &caps(false), &ContextPolicy::default());
    assert_eq!(off.content.as_deref(), Some("hi"));

    let on = build_user_message(
        &event,
        &caps(false),
        &ContextPolicy {
            include_channel_id: true,
            include_sender_id: true,
            include_timestamp: true,
            ..Default::default()
        },
    );
    let content = on.content.unwrap_or_default();
    assert!(content.contains("[群号: group:1]"), "{content}");
    assert!(content.contains("[发送者: 1705702687]"), "{content}");

    // The timestamp is rendered in the host's local timezone, so the exact value depends on the
    // machine running the test; the shape (`YYYY-MM-DD HH:MM:SS`) and the label are what matter.
    let timestamp = content
        .split("[时间: ")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or_default();
    assert_eq!(timestamp.len(), 19, "unexpected timestamp: {content}");
    assert!(
        !timestamp.contains("UTC"),
        "local time must not be labelled UTC: {content}"
    );
}
