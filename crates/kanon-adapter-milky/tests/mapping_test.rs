//! Translation between the Milky wire model and the Kanon pipeline contract.
//!
//! These tests are the adapter's correctness core: every assertion here corresponds to something a
//! user would notice if it broke — a reply landing in the wrong conversation, an attachment
//! vanishing, or a mention being sent to the wrong account.

use kanon_adapter_milky::mapping::{
    self, CHANNEL_FRIEND, CHANNEL_GROUP, CHANNEL_TEMP, CUSTOM_SEGMENT_PREFIX, ChannelScene,
    MappingError, channel_id, delivery_target, inbound_message, outbound_segment,
    outbound_segments, parse_channel_id, render_text,
};
use kanon_adapter_milky::protocol::{Event, IncomingSegment, OutgoingSegment};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, ImageSegment, MentionSegment, MessageSegment, ReplySegment, TextSegment,
    audio_segment, image_segment,
};
use serde_json::json;

/// Builds a Kanon text segment.
fn text(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }
}

/// Parses one Milky inbound segment from its wire JSON.
fn segment(value: serde_json::Value) -> IncomingSegment {
    serde_json::from_value(value).expect("fixture segment should decode")
}

/// Parses a `message_receive` event from its wire JSON.
fn event(value: serde_json::Value) -> Event {
    serde_json::from_value(value).expect("fixture event should decode")
}

/// Builds a friend message event fixture.
fn friend_event(segments: serde_json::Value) -> serde_json::Value {
    json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "message_receive",
        "data": {
            "message_scene": "friend",
            "peer_id": 20002,
            "message_seq": 77,
            "sender_id": 20002,
            "time": 1_700_000_000_i64,
            "segments": segments,
            "friend": {
                "user_id": 20002,
                "nickname": "Alice",
                "sex": "female",
                "qid": "alice",
                "remark": "",
                "category": { "category_id": 0, "category_name": "" }
            }
        }
    })
}

/// Channel identifiers round-trip through the grammar both directions share.
#[test]
fn channel_identifiers_round_trip() {
    for (scene, peer_id) in [
        (ChannelScene::Friend, 20002_i64),
        (ChannelScene::Group, 30003_i64),
        (ChannelScene::Temp, 40004_i64),
    ] {
        let encoded = channel_id(scene, peer_id);
        let decoded = parse_channel_id(&encoded).expect("encoded channel should parse");

        assert_eq!(decoded.scene, scene);
        assert_eq!(decoded.peer_id, peer_id);
    }

    assert_eq!(channel_id(ChannelScene::Group, 1), "group:1");
    assert_eq!(CHANNEL_FRIEND, "friend");
    assert_eq!(CHANNEL_GROUP, "group");
    assert_eq!(CHANNEL_TEMP, "temp");
}

/// A channel identifier that cannot be routed is rejected instead of guessed at.
#[test]
fn malformed_channel_identifiers_are_rejected() {
    for raw in [
        "",
        "20002",
        "friend:",
        "friend:abc",
        "channel:1",
        ":1",
        "group:1:2",
    ] {
        assert!(
            matches!(parse_channel_id(raw), Err(MappingError::Channel(_))),
            "expected '{raw}' to be rejected"
        );
    }
}

/// Temporary sessions cannot be answered, because Milky defines no send endpoint for them.
#[test]
fn temporary_conversations_are_rejected_for_delivery() {
    let error = delivery_target("temp:40004").expect_err("temp delivery should be rejected");
    assert!(matches!(error, MappingError::TempConversation(_)));
    assert!(error.to_string().contains("temporary session"));

    // Friend and group conversations stay deliverable.
    assert!(delivery_target("friend:1").is_ok());
    assert!(delivery_target("group:1").is_ok());
}

/// A friend message becomes a pipeline event with the scene encoded in its channel identifier.
#[test]
fn friend_message_maps_to_pipeline_request() {
    let event = event(friend_event(json!([
        { "type": "text", "data": { "text": "hello " } },
        { "type": "mention", "data": { "user_id": 10001, "name": "Kanon" } },
        { "type": "text", "data": { "text": " there" } }
    ])));

    let request = inbound_message("milky", event.self_id(), &event)
        .expect("translation should succeed")
        .expect("a message event should ingest");

    assert_eq!(request.platform, "milky");
    assert_eq!(request.channel_id, "friend:20002");
    assert_eq!(request.sender_id, "20002");
    // The event identifier is deterministic, so a replayed frame maps to the same trace.
    assert_eq!(request.event_id, "milky:10001:friend:20002:20002:77");
    assert_eq!(request.raw_text, "hello @Kanon there");

    let metadata = request.metadata.expect("metadata should be present");
    assert_eq!(
        metadata
            .fields
            .get("milky.message_scene")
            .map(|value| value.kind.clone()),
        Some(Some(kanon_proto::prost_types::value::Kind::StringValue(
            "friend".to_string()
        )))
    );
    assert_eq!(
        metadata.fields.get("milky.sender_name").cloned(),
        Some(kanon_proto::prost_types::Value {
            kind: Some(kanon_proto::prost_types::value::Kind::StringValue(
                "Alice".to_string()
            )),
        })
    );
}

/// A group message carries the group context a command or a model prompt may need.
#[test]
fn group_message_metadata_includes_group_context() {
    let event = event(json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "message_receive",
        "data": {
            "message_scene": "group",
            "peer_id": 30003,
            "message_seq": 88,
            "sender_id": 20002,
            "time": 1_700_000_000_i64,
            "segments": [ { "type": "text", "data": { "text": "hi" } } ],
            "group": {
                "group_id": 30003,
                "group_name": "Test Group",
                "member_count": 3,
                "max_member_count": 200,
                "remark": "",
                "created_time": 1_600_000_000_i64,
                "description": "",
                "question": "",
                "announcement": ""
            },
            "group_member": {
                "user_id": 20002,
                "nickname": "Alice",
                "sex": "female",
                "group_id": 30003,
                "card": "Ali",
                "title": "",
                "level": 2,
                "role": "admin",
                "join_time": 1_600_000_000_i64,
                "last_sent_time": 1_700_000_000_i64
            }
        }
    }));

    let request = inbound_message("milky", event.self_id(), &event)
        .expect("translation should succeed")
        .expect("a message event should ingest");

    assert_eq!(request.channel_id, "group:30003");
    let metadata = request.metadata.expect("metadata should be present");
    let strings: Vec<(&str, &str)> = metadata
        .fields
        .iter()
        .filter_map(|(key, value)| match value.kind.as_ref() {
            Some(kanon_proto::prost_types::value::Kind::StringValue(text)) => {
                Some((key.as_str(), text.as_str()))
            }
            _ => None,
        })
        .collect();

    assert!(strings.contains(&("milky.group_name", "Test Group")));
    assert!(strings.contains(&("milky.sender_card", "Ali")));
    assert!(strings.contains(&("milky.sender_role", "admin")));
}

/// Every segment kind reaches Kanon: natively where a field exists, as a preserved payload where
/// it does not.
#[test]
fn inbound_segments_map_to_native_or_custom_segments() {
    let cases: Vec<(serde_json::Value, &str)> = vec![
        (json!({ "type": "text", "data": { "text": "hi" } }), "text"),
        (
            json!({ "type": "mention", "data": { "user_id": 10001, "name": "Kanon" } }),
            "mention",
        ),
        (json!({ "type": "mention_all", "data": {} }), "mention_all"),
        (
            json!({ "type": "face", "data": { "face_id": "1", "is_large": false } }),
            "milky.face",
        ),
        (
            json!({ "type": "reply", "data": {
                "message_seq": 12, "sender_id": 20002, "time": 1_700_000_000_i64,
                "segments": [ { "type": "text", "data": { "text": "quoted" } } ]
            } }),
            "reply",
        ),
        (
            json!({ "type": "image", "data": {
                "resource_id": "r1", "temp_url": "https://cdn.example.com/a.png",
                "width": 10, "height": 20, "summary": "[image]", "sub_type": "normal"
            } }),
            "image",
        ),
        (
            json!({ "type": "record", "data": {
                "resource_id": "r2", "temp_url": "https://cdn.example.com/a.silk", "duration": 3
            } }),
            "audio",
        ),
        (
            json!({ "type": "video", "data": {
                "resource_id": "r3", "temp_url": "https://cdn.example.com/a.mp4",
                "width": 10, "height": 20, "duration": 4
            } }),
            "milky.video",
        ),
        (
            json!({ "type": "file", "data": {
                "file_id": "f1", "file_name": "a.txt", "file_size": 10, "file_hash": "h"
            } }),
            "milky.file",
        ),
        (
            json!({ "type": "forward", "data": {
                "forward_id": "fw1", "title": "chat", "preview": ["a"], "summary": "s"
            } }),
            "milky.forward",
        ),
        (
            json!({ "type": "market_face", "data": {
                "emoji_package_id": 1, "emoji_id": "e1", "key": "k",
                "summary": "s", "url": "https://cdn.example.com/e.png"
            } }),
            "milky.market_face",
        ),
        (
            json!({ "type": "light_app", "data": { "app_name": "app", "json_payload": "{}" } }),
            "milky.light_app",
        ),
        (
            json!({ "type": "xml", "data": { "service_id": 1, "xml_payload": "<xml/>" } }),
            "milky.xml",
        ),
        (
            json!({ "type": "markdown", "data": { "content": "# title" } }),
            "milky.markdown",
        ),
        // A kind this protocol release does not define must survive as data, not as a placeholder.
        (
            json!({ "type": "future_thing", "data": { "answer": 42 } }),
            "milky.future_thing",
        ),
    ];

    for (wire, expected) in cases {
        let mapped = mapping::incoming_segment(&segment(wire.clone()));
        let actual = match mapped.segment.as_ref().expect("segment should be present") {
            Segment::Text(_) => "text",
            Segment::Mention(mention) if mention.is_all => "mention_all",
            Segment::Mention(_) => "mention",
            Segment::Reply(_) => "reply",
            Segment::Image(_) => "image",
            Segment::Audio(_) => "audio",
            Segment::Video(_) => "video",
            Segment::File(_) => "file",
            Segment::Face(_) => "face",
            Segment::Custom(custom) => custom.type_name.as_str(),
        };

        assert_eq!(actual, expected, "unexpected mapping for {wire}");
    }
}

/// The preserved payload of a custom segment keeps the fields the protocol defined.
#[test]
fn custom_segment_preserves_payload_fields() {
    let mapped = mapping::incoming_segment(&segment(json!({
        "type": "file",
        "data": { "file_id": "f1", "file_name": "report.pdf", "file_size": 2048, "file_hash": "h1" }
    })));

    let Segment::Custom(custom) = mapped.segment.expect("segment should be present") else {
        panic!("a file segment has no native Kanon field and must become a custom segment");
    };

    assert_eq!(custom.type_name, format!("{CUSTOM_SEGMENT_PREFIX}file"));
    let payload = custom.payload.expect("payload should be preserved");
    assert_eq!(
        payload.fields.get("file_name").cloned(),
        Some(kanon_proto::prost_types::Value {
            kind: Some(kanon_proto::prost_types::value::Kind::StringValue(
                "report.pdf".to_string()
            )),
        })
    );
}

/// An unknown segment type keeps its original name and payload.
#[test]
fn unknown_segment_type_is_preserved_verbatim() {
    let wire = json!({ "type": "future_thing", "data": { "answer": 42 } });
    let mapped = mapping::incoming_segment(&segment(wire.clone()));

    let Segment::Custom(custom) = mapped.segment.expect("segment should be present") else {
        panic!("an unknown segment must become a custom segment");
    };

    assert_eq!(custom.type_name, "milky.future_thing");
    let payload = mapping::struct_to_json(custom.payload.as_ref().expect("payload"));
    assert_eq!(payload, wire);
    // The textual view names the unknown kind instead of hiding it.
    assert_eq!(render_text(&[segment(wire)]), "[future_thing]");
}

/// The textual rendering is what commands and the model see, so media must not vanish.
#[test]
fn text_rendering_never_drops_a_segment() {
    let segments = vec![
        segment(json!({ "type": "text", "data": { "text": "look " } })),
        segment(json!({ "type": "image", "data": {
            "resource_id": "r", "temp_url": "https://x/y.png", "width": 1, "height": 1,
            "summary": "", "sub_type": "normal"
        } })),
        segment(json!({ "type": "record", "data": {
            "resource_id": "r", "temp_url": "https://x/y.silk", "duration": 1
        } })),
        segment(json!({ "type": "file", "data": {
            "file_id": "f", "file_name": "a.txt", "file_size": 1, "file_hash": ""
        } })),
        // A reply is not inlined: its content is carried by the reply segment itself.
        segment(json!({ "type": "reply", "data": {
            "message_seq": 1, "sender_id": 2, "time": 3,
            "segments": [ { "type": "text", "data": { "text": "old" } } ]
        } })),
    ];

    assert_eq!(render_text(&segments), "look [image][voice][file:a.txt]");
}

/// Non-conversational events are never translated as if a user had typed them; the notices among
/// them are handled by `map_notice` instead.
#[test]
fn non_message_events_are_not_ingested() {
    let event = event(json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "message_recall",
        "data": {
            "message_scene": "group",
            "peer_id": 30003,
            "message_seq": 5,
            "sender_id": 20002,
            "operator_id": 20002,
            "display_suffix": ""
        }
    }));

    assert_eq!(
        inbound_message("milky", event.self_id(), &event).expect("translation should not fail"),
        None
    );
}

/// Outbound native segments are rendered exactly as the protocol prescribes.
#[test]
fn outbound_native_segments_map_to_wire_form() {
    let cases: Vec<(MessageSegment, serde_json::Value)> = vec![
        (
            text("hello"),
            json!({ "type": "text", "data": { "text": "hello" } }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Mention(MentionSegment {
                    target_user_id: "10001".to_string(),
                    display_name: "Kanon".to_string(),
                    is_all: false,
                })),
            },
            json!({ "type": "mention", "data": { "user_id": 10001 } }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Mention(MentionSegment {
                    target_user_id: String::new(),
                    display_name: String::new(),
                    is_all: true,
                })),
            },
            json!({ "type": "mention_all", "data": {} }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Reply(ReplySegment {
                    target_message_id: "77".to_string(),
                    snippet: "quoted".to_string(),
                })),
            },
            json!({ "type": "reply", "data": { "message_seq": 77 } }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Image(ImageSegment {
                    source: Some(image_segment::Source::Url(
                        "https://cdn.example.com/a.png".to_string(),
                    )),
                    mime_type: None,
                    filename: Some("a.png".to_string()),
                })),
            },
            json!({ "type": "image", "data": {
                "uri": "https://cdn.example.com/a.png", "sub_type": "normal", "summary": "a.png"
            } }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Image(ImageSegment {
                    source: Some(image_segment::Source::FilePath("/tmp/a.png".to_string())),
                    mime_type: None,
                    filename: None,
                })),
            },
            json!({ "type": "image", "data": {
                "uri": "file:///tmp/a.png", "sub_type": "normal"
            } }),
        ),
        (
            MessageSegment {
                segment: Some(Segment::Audio(AudioSegment {
                    source: Some(audio_segment::Source::RawBytes(vec![1, 2, 3])),
                    duration_seconds: Some(2),
                })),
            },
            json!({ "type": "record", "data": { "uri": "base64://AQID" } }),
        ),
    ];

    for (segment, expected) in cases {
        let mapped = outbound_segment(&segment).expect("segment should map");
        assert_eq!(
            serde_json::to_value(&mapped).expect("segment should serialize"),
            expected
        );
    }
}

/// A custom segment produced on ingest round-trips to the very segment it came from.
#[test]
fn outbound_custom_segments_round_trip() {
    // Image and record map natively, so a preserved custom segment only exists for the kinds
    // without a native field; the outbound-capable ones among them are checked here.
    let face = mapping::incoming_segment(&segment(json!({
        "type": "face", "data": { "face_id": "21", "is_large": true }
    })));
    let restored = outbound_segment(&face).expect("face should round-trip");
    assert_eq!(
        serde_json::to_value(&restored).expect("segment should serialize"),
        json!({ "type": "face", "data": { "face_id": "21", "is_large": true } })
    );

    let light_app = mapping::incoming_segment(&segment(json!({
        "type": "light_app", "data": { "app_name": "app", "json_payload": "{\"a\":1}" }
    })));
    let restored = outbound_segment(&light_app).expect("light app should round-trip");
    assert_eq!(
        serde_json::to_value(&restored).expect("segment should serialize"),
        json!({ "type": "light_app", "data": { "json_payload": "{\"a\":1}" } })
    );
}

/// A native Kanon segment and its Milky counterpart round-trip through both directions.
#[test]
fn native_segments_round_trip_both_ways() {
    let wire = json!([
        { "type": "text", "data": { "text": "hi" } },
        { "type": "mention", "data": { "user_id": 10001, "name": "Kanon" } },
        { "type": "image", "data": {
            "resource_id": "r", "temp_url": "https://cdn.example.com/a.png", "width": 1,
            "height": 1, "summary": "", "sub_type": "normal"
        } },
        { "type": "record", "data": {
            "resource_id": "r", "temp_url": "https://cdn.example.com/a.silk", "duration": 3
        } }
    ]);

    let inbound: Vec<IncomingSegment> = serde_json::from_value(wire).expect("fixtures decode");
    let kanon: Vec<MessageSegment> = inbound.iter().map(mapping::incoming_segment).collect();
    let back: Vec<OutgoingSegment> = outbound_segments(&kanon).expect("segments should map");

    let rendered: Vec<serde_json::Value> = back
        .iter()
        .map(|segment| serde_json::to_value(segment).expect("segment should serialize"))
        .collect();

    assert_eq!(
        rendered,
        vec![
            json!({ "type": "text", "data": { "text": "hi" } }),
            json!({ "type": "mention", "data": { "user_id": 10001 } }),
            json!({ "type": "image", "data": {
                "uri": "https://cdn.example.com/a.png", "sub_type": "normal"
            } }),
            json!({ "type": "record", "data": { "uri": "https://cdn.example.com/a.silk" } }),
        ]
    );
}

/// Segments Kanon can express but Milky cannot send fail loudly instead of being dropped.
#[test]
fn unsendable_and_malformed_segments_fail_explicitly() {
    // An inbound-only custom segment has no Milky send endpoint.
    let inbound_only = mapping::incoming_segment(&segment(json!({
        "type": "file", "data": { "file_id": "f", "file_name": "a.txt", "file_size": 1, "file_hash": "" }
    })));
    assert!(matches!(
        outbound_segment(&inbound_only),
        Err(MappingError::CustomSegment(_))
    ));

    // A custom segment from another platform is not a Milky segment.
    let foreign = MessageSegment {
        segment: Some(Segment::Custom(kanon_proto::v1::RawCustomSegment {
            type_name: "some_other_platform.thing".to_string(),
            payload: None,
        })),
    };
    assert!(matches!(
        outbound_segment(&foreign),
        Err(MappingError::CustomSegment(_))
    ));

    // A mention target that is not a QQ number cannot be sent.
    let bad_mention = MessageSegment {
        segment: Some(Segment::Mention(MentionSegment {
            target_user_id: "alice".to_string(),
            display_name: String::new(),
            is_all: false,
        })),
    };
    assert!(matches!(
        outbound_segment(&bad_mention),
        Err(MappingError::MentionTarget(_))
    ));

    // A reply target that is not a sequence number cannot be sent.
    let bad_reply = MessageSegment {
        segment: Some(Segment::Reply(ReplySegment {
            target_message_id: "evt-1".to_string(),
            snippet: String::new(),
        })),
    };
    assert!(matches!(
        outbound_segment(&bad_reply),
        Err(MappingError::ReplyTarget(_))
    ));

    // An empty message has nothing to send.
    assert!(matches!(outbound_segments(&[]), Err(MappingError::Empty)));

    // A segment with no kind set carries nothing.
    assert!(matches!(
        outbound_segment(&MessageSegment { segment: None }),
        Err(MappingError::Empty)
    ));
}

/// JSON and protobuf structures survive a round trip through the metadata representation.
#[test]
fn json_struct_conversion_round_trips() {
    let value = json!({
        "text": "hello",
        "count": 3,
        "flag": true,
        "nothing": null,
        "nested": { "list": [1, 2, 3] }
    });

    let structured = mapping::json_to_struct(&value);
    assert_eq!(mapping::struct_to_json(&structured), value);
}

/// Typed video and face segments map to Milky's own; a file cannot travel inside a message, so
/// it is refused instead of silently dropped.
#[test]
fn typed_video_and_face_map_and_files_are_refused() {
    use kanon_proto::v1::{FaceSegment, FileSegment, VideoSegment, file_segment, video_segment};

    let video = MessageSegment {
        segment: Some(Segment::Video(VideoSegment {
            source: Some(video_segment::Source::FilePath("/tmp/v.mp4".into())),
            ..Default::default()
        })),
    };
    assert_eq!(
        serde_json::to_value(outbound_segment(&video).unwrap()).unwrap(),
        json!({ "type": "video", "data": { "uri": "file:///tmp/v.mp4" } })
    );

    let face = MessageSegment {
        segment: Some(Segment::Face(FaceSegment { id: "76".into() })),
    };
    assert_eq!(
        serde_json::to_value(outbound_segment(&face).unwrap()).unwrap(),
        json!({ "type": "face", "data": { "face_id": "76", "is_large": false } })
    );

    let file = MessageSegment {
        segment: Some(Segment::File(FileSegment {
            source: Some(file_segment::Source::Url(
                "https://cdn.example/r.pdf".into(),
            )),
            name: "r.pdf".into(),
        })),
    };
    assert!(matches!(
        outbound_segment(&file),
        Err(MappingError::Unsupported(_))
    ));
}
