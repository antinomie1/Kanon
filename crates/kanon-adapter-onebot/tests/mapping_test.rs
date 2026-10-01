use kanon_adapter_onebot::mapping::{attach_quote, delivery, map_event, message_id, reply_target};
use kanon_proto::prost_types::value::Kind;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, DeliverMessageRequest, ImageSegment, MessageSegment, RawCustomSegment,
    audio_segment, image_segment,
};
use serde_json::{Value, json};

fn event(message: Value) -> Value {
    json!({
        "post_type": "message", "message_type": "group", "sub_type": "normal",
        "self_id": 10001, "user_id": 20002, "group_id": 30003,
        "message_id": -77, "time": 1700000000,
        "sender": {"nickname": "Alice", "card": "A", "role": "member"},
        "message": message,
    })
}

fn request(segments: Vec<MessageSegment>) -> DeliverMessageRequest {
    DeliverMessageRequest {
        channel_id: "group:30003".into(),
        segments,
        ..Default::default()
    }
}

#[test]
fn group_events_carry_native_segments_and_core_policy_metadata() {
    let mapped = map_event(
        "onebot:main",
        event(json!([
            {"type": "text", "data": {"text": "hello "}},
            {"type": "at", "data": {"qq": "10001"}},
            {"type": "reply", "data": {"id": "-12"}},
            {"type": "image", "data": {"file": "a.image", "url": "https://cdn.example/a.png"}},
            {"type": "record", "data": {"file": "a.silk", "url": "https://cdn.example/a.silk"}}
        ])),
    )
    .unwrap()
    .unwrap();
    assert_eq!(mapped.channel_id, "group:30003");
    assert_eq!(mapped.sender_id, "20002");
    assert_eq!(mapped.event_id, "onebot:main:10001:group:30003:20002:-77");
    assert_eq!(mapped.raw_text, "hello @10001[image][voice]");
    assert!(
        matches!(&mapped.segments[2].segment, Some(Segment::Reply(reply)) if reply.target_message_id == "-12")
    );
    assert!(
        matches!(&mapped.segments[3].segment, Some(Segment::Image(image)) if image.source == Some(image_segment::Source::Url("https://cdn.example/a.png".into())))
    );
    let metadata = mapped.metadata.unwrap();
    assert_eq!(
        metadata.fields[kanon_core::META_CONVERSATION_KIND].kind,
        Some(Kind::StringValue("group".into()))
    );
    assert_eq!(
        metadata.fields[kanon_core::META_BOT_MENTIONED].kind,
        Some(Kind::BoolValue(true))
    );
    assert_eq!(
        metadata.fields[kanon_core::META_TIMESTAMP].kind,
        Some(Kind::NumberValue(1700000000.0))
    );
    assert_eq!(
        metadata.fields["onebot.sender_nickname"].kind,
        Some(Kind::StringValue("Alice".into()))
    );
}

#[test]
fn private_routing_and_mention_detection_are_explicit() {
    for (qq, expected) in [("all", true), ("10001", true), ("999", false)] {
        let mut input = event(json!([{"type": "at", "data": {"qq": qq}}]));
        input["message_type"] = json!("private");
        let mapped = map_event("onebot", input).unwrap().unwrap();
        assert_eq!(mapped.channel_id, "private:20002");
        assert_eq!(
            mapped.metadata.as_ref().unwrap().fields[kanon_core::META_BOT_MENTIONED].kind,
            Some(Kind::BoolValue(expected))
        );
        let mut outbound = request(mapped.segments);
        outbound.channel_id = mapped.channel_id;
        let (action, params) = delivery(&outbound).unwrap();
        assert_eq!(action, "send_private_msg");
        assert_eq!(params["user_id"], 20002);
        assert_eq!(params["message"][0]["data"]["qq"], qq);
    }
}

#[test]
fn cq_syntax_decodes_once_without_treating_escaped_text_as_segments() {
    let mapped = map_event("onebot", event(json!(
        "你好 &#91;CQ:at,qq=999&#93; &amp;#91;[CQ:at,qq=10001][CQ:face,id=14][CQ:json,data={&amp;quot;x&amp;quot;:&#91;1&#44;2&#93;}][CQ:reply,id=-7]"
    ))).unwrap().unwrap();
    assert_eq!(
        mapped.raw_text,
        "你好 [CQ:at,qq=999] &#91;@10001[face][json]"
    );
    let (_, params) = delivery(&request(mapped.segments)).unwrap();
    assert_eq!(
        params["message"][3]["data"]["data"],
        "{&quot;x&quot;:[1,2]}"
    );
    assert_eq!(
        params["message"][4],
        json!({"type": "reply", "data": {"id": "-7"}})
    );
}

#[test]
fn native_and_preserved_media_send_valid_files() {
    let input = json!([
        {"type": "image", "data": {"file": "cached.image", "type": "flash"}},
        {"type": "record", "data": {"file": "cached.silk"}},
        {"type": "face", "data": {"id": "14", "nested": {"enabled": true, "items": [1, null]}}}
    ]);
    let mapped = map_event("onebot", event(input.clone())).unwrap().unwrap();
    assert!(
        mapped
            .segments
            .iter()
            .all(|s| matches!(s.segment, Some(Segment::Custom(_))))
    );
    let (_, params) = delivery(&request(mapped.segments)).unwrap();
    assert_eq!(params["message"], input);

    let sources = vec![
        MessageSegment {
            segment: Some(Segment::Image(ImageSegment {
                source: Some(image_segment::Source::RawBytes(vec![1, 2, 3])),
                ..Default::default()
            })),
        },
        MessageSegment {
            segment: Some(Segment::Image(ImageSegment {
                source: Some(image_segment::Source::FilePath("/tmp/a.png".into())),
                ..Default::default()
            })),
        },
        MessageSegment {
            segment: Some(Segment::Audio(AudioSegment {
                source: Some(audio_segment::Source::RawBytes(vec![4, 5, 6])),
                ..Default::default()
            })),
        },
        MessageSegment {
            segment: Some(Segment::Audio(AudioSegment {
                source: Some(audio_segment::Source::Url(
                    "https://cdn.example/a.silk".into(),
                )),
                ..Default::default()
            })),
        },
    ];
    let (action, params) = delivery(&request(sources)).unwrap();
    assert_eq!(action, "send_group_msg");
    assert_eq!(params["group_id"], 30003);
    let files: Vec<_> = params["message"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["data"]["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        [
            "base64://AQID",
            "file:///tmp/a.png",
            "base64://BAUG",
            "https://cdn.example/a.silk"
        ]
    );
}

#[test]
fn integer_ids_never_pass_through_floating_point() {
    let mut input = event(json!([{"type": "at", "data": {"qq": "9007199254740993"}}]));
    input["self_id"] = json!(9007199254740993_i64);
    input["group_id"] = json!(9007199254740995_i64);
    input["message_id"] = json!(i64::MIN);
    let mapped = map_event("onebot", input.clone()).unwrap().unwrap();
    assert_eq!(mapped.channel_id, "group:9007199254740995");
    assert!(mapped.event_id.ends_with("-9223372036854775808"));
    assert_eq!(
        mapped.metadata.unwrap().fields["onebot.self_id"].kind,
        Some(Kind::StringValue("9007199254740993".into()))
    );
    let mut outbound = request(mapped.segments);
    outbound.channel_id = mapped.channel_id;
    let (_, params) = delivery(&outbound).unwrap();
    assert_eq!(params["group_id"].as_i64(), Some(9007199254740995));
    assert_eq!(params["message"][0]["data"]["qq"], "9007199254740993");
    input["self_id"] = json!(10001);
    let other = map_event("onebot", input).unwrap().unwrap();
    assert_ne!(mapped.event_id, other.event_id);
    assert_eq!(
        message_id(&json!("9007199254740993")).unwrap(),
        "9007199254740993"
    );
    assert_eq!(
        message_id(&json!(i64::MIN)).unwrap(),
        "-9223372036854775808"
    );
    assert!(message_id(&json!(1.5)).is_err());
    assert!(message_id(&json!(1.0)).is_err());
}

#[test]
fn mapped_facts_drive_the_current_core_reply_policy() {
    let policy = kanon_core::ReplyPolicy::new(kanon_core::ReplyMode::Mention);
    for (scene, message, expected) in [
        ("group", json!("hello"), false),
        ("group", json!("[CQ:at,qq=10001]hello"), true),
        ("group", json!("[CQ:at,qq=all]hello"), true),
        // Replying to a message alone does not identify its author in OneBot v11.
        ("group", json!("[CQ:reply,id=12]hello"), false),
        ("private", json!("hello"), true),
    ] {
        let mut input = event(message);
        input["message_type"] = json!(scene);
        let mapped = map_event("onebot", input).unwrap().unwrap();
        let metadata = mapped.metadata.as_ref();
        assert_eq!(
            policy.should_reply(
                kanon_core::ConversationKind::from_metadata(metadata),
                kanon_core::bot_mentioned(metadata),
                0.5,
            ),
            expected,
        );
    }
}

#[test]
fn nonmessages_are_ignored_and_malformed_inputs_fail() {
    for kind in ["notice", "request", "meta_event", "message_sent"] {
        assert_eq!(
            map_event("onebot", json!({"post_type": kind})).unwrap(),
            None
        );
    }
    for input in [
        json!(null),
        json!(123),
        json!([{"type": "text", "data": {}}]),
        json!([{"type": "at", "data": {"qq": "alice"}}]),
        json!([{"type": "reply", "data": {"id": 0.5}}]),
        json!("[CQ:at,qq=1"),
        json!("[CQ:at,qq=1,qq=2]"),
        json!("[CQ:face,bad]"),
        json!([{"type": "future", "data": {"id": 9007199254740993_i64}}]),
    ] {
        assert!(
            map_event("onebot", event(input.clone())).is_err(),
            "accepted {input}"
        );
    }
    assert!(delivery(&request(vec![])).is_err());
    for segment in [
        MessageSegment { segment: None },
        MessageSegment {
            segment: Some(Segment::Image(ImageSegment::default())),
        },
        MessageSegment {
            segment: Some(Segment::Custom(RawCustomSegment {
                type_name: "milky.face".into(),
                payload: None,
            })),
        },
    ] {
        assert!(delivery(&request(vec![segment])).is_err());
    }
    let good = map_event("onebot", event(json!("hello"))).unwrap().unwrap();
    for channel in ["30003", "guild:30003", "group:abc", "group:1:2"] {
        let mut outbound = request(good.segments.clone());
        outbound.channel_id = channel.into();
        assert!(delivery(&outbound).is_err());
    }
}

#[test]
fn quotes_carry_text_files_and_stickers_into_context() {
    let mut mapped = map_event(
        "onebot",
        event(json!([
            {"type": "reply", "data": {"id": "5"}},
            {"type": "text", "data": {"text": "?"}}
        ])),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reply_target(&mapped), Some(5));
    attach_quote(
        &mut mapped,
        &json!("see[CQ:file,file=report.pdf,file_id=x][CQ:mface,url=https://cdn.example/s.gif,summary=hi]"),
    )
    .unwrap();
    assert!(
        matches!(&mapped.segments[0].segment, Some(Segment::Reply(reply)) if reply.snippet == "see[file:report.pdf][mface]")
    );
    assert!(
        matches!(&mapped.segments[1].segment, Some(Segment::Image(image)) if image.source == Some(image_segment::Source::Url("https://cdn.example/s.gif".into())))
    );
    assert!(matches!(
        &mapped.segments[2].segment,
        Some(Segment::Text(_))
    ));
}

/// Typed video, file and face segments become the OneBot segments of the same kinds; a file
/// without a name is refused rather than sent anonymously.
#[test]
fn typed_video_file_and_face_segments_are_sent_natively() {
    use kanon_proto::v1::{FaceSegment, FileSegment, VideoSegment, file_segment, video_segment};

    let (_, params) = delivery(&request(vec![
        MessageSegment {
            segment: Some(Segment::Video(VideoSegment {
                source: Some(video_segment::Source::Url(
                    "https://cdn.example/v.mp4".into(),
                )),
                ..Default::default()
            })),
        },
        MessageSegment {
            segment: Some(Segment::File(FileSegment {
                source: Some(file_segment::Source::FilePath("/tmp/r.pdf".into())),
                name: "report.pdf".into(),
            })),
        },
        MessageSegment {
            segment: Some(Segment::Face(FaceSegment { id: "76".into() })),
        },
    ]))
    .unwrap();
    assert_eq!(
        params["message"],
        json!([
            {"type": "video", "data": {"file": "https://cdn.example/v.mp4"}},
            {"type": "file", "data": {"file": "file:///tmp/r.pdf", "name": "report.pdf"}},
            {"type": "face", "data": {"id": "76"}},
        ])
    );

    let unnamed = MessageSegment {
        segment: Some(Segment::File(FileSegment {
            source: Some(file_segment::Source::Url("https://cdn.example/x".into())),
            name: String::new(),
        })),
    };
    assert!(delivery(&request(vec![unnamed])).is_err());
}
