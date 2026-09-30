//! Inbound mapping, with the focus on quoted messages: whatever the user quoted — text, images,
//! stickers, voice, files — must reach the model, and transport noise (hash file names, base64
//! face payloads, media URLs of voice) must not.

use base64::Engine;
use kanon_adapter_qqofficial::mapping::{Quote, QuoteStore, clean_content, map_event};
use kanon_proto::prost_types::value::Kind;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{PipelineEventRequest, image_segment};
use serde_json::{Value, json};

/// A QQ face tag as the gateway sends it: the name hides in base64 JSON.
fn face(name: &str) -> String {
    let ext = base64::engine::general_purpose::STANDARD.encode(json!({"text": name}).to_string());
    format!(r#"<faceType=1,faceId="13",ext="{ext}">"#)
}

fn group_at(extra: Value) -> Value {
    let mut event = json!({
        "id": "ROBOT1.0_msg1",
        "content": " 这是什么？",
        "timestamp": "2026-09-30T12:00:00+08:00",
        "group_openid": "G1",
        "author": {"member_openid": "M1", "id": "M1"},
        "message_scene": {"source": "default", "ext": ["msg_idx=REFIDX_self"]},
    });
    event
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    event
}

fn map(event_type: &str, data: &Value, quotes: &mut QuoteStore) -> PipelineEventRequest {
    map_event(event_type, data, "BOT", quotes)
        .expect("event should map")
        .expect("message events produce a pipeline event")
}

fn metadata(event: &PipelineEventRequest, key: &str) -> Kind {
    event.metadata.as_ref().unwrap().fields[key]
        .kind
        .clone()
        .unwrap()
}

/// A quote of text and a picture: the snippet carries the text, the picture follows as an image.
#[test]
fn quoted_text_and_image_reach_the_model() {
    let data = group_at(json!({
        "message_type": 103,
        "message_scene": {"source": "default", "ext": ["msg_idx=REFIDX_self", "ref_msg_idx=REFIDX_q"]},
        "msg_elements": [{
            "msg_idx": "REFIDX_q",
            "content": "看我的猫",
            "attachments": [{
                "content_type": "image/jpeg",
                "filename": "5F2C0D1E9A6B4C3D8E7F.jpg",
                "url": "https://multimedia.nt.qq.com.cn/download?fileid=abc",
            }],
        }],
    }));
    let mut quotes = QuoteStore::new(16);
    let event = map("GROUP_AT_MESSAGE_CREATE", &data, &mut quotes);

    assert_eq!(event.event_id, "ROBOT1.0_msg1");
    assert_eq!(event.channel_id, "group:G1");
    assert_eq!(event.sender_id, "M1");
    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("a quote starts with a reply segment: {:?}", event.segments);
    };
    assert_eq!(reply.target_message_id, "REFIDX_q");
    assert_eq!(reply.snippet, "看我的猫 [image]");
    assert!(
        !reply.snippet.contains("5F2C0D1E"),
        "hash file names are noise"
    );
    let Some(Segment::Image(image)) = &event.segments[1].segment else {
        panic!("the quoted picture follows the reply: {:?}", event.segments);
    };
    assert_eq!(
        image.source,
        Some(image_segment::Source::Url(
            "https://multimedia.nt.qq.com.cn/download?fileid=abc".into()
        ))
    );
    let Some(Segment::Text(text)) = &event.segments[2].segment else {
        panic!("the user's own words come last");
    };
    assert_eq!(text.content, "这是什么？");
    assert_eq!(
        metadata(&event, "kanon.bot_mentioned"),
        Kind::BoolValue(true)
    );
    assert_eq!(
        metadata(&event, "kanon.conversation_kind"),
        Kind::StringValue("group".into())
    );
}

/// Stickers, voice and files are named by what they mean to a reader, never by payload.
#[test]
fn quoted_sticker_voice_and_file_are_readable() {
    let data = group_at(json!({
        "message_type": 103,
        "msg_elements": [{
            "msg_idx": "REFIDX_q",
            "content": format!("好的{}", face("呲牙")),
            "attachments": [
                {"content_type": "voice", "url": "https://x/voice.silk", "asr_refer_text": "明天见"},
                {"content_type": "file", "url": "https://x/f", "filename": "季度报告.pdf", "size": 2048},
            ],
        }],
    }));
    let event = map("GROUP_AT_MESSAGE_CREATE", &data, &mut QuoteStore::new(16));

    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("expected a reply segment");
    };
    assert_eq!(
        reply.snippet,
        "好的[表情:呲牙] [voice: 明天见] [file:季度报告.pdf]"
    );
    assert!(!reply.snippet.contains("ext="), "face payloads are decoded");
    assert!(!reply.snippet.contains("silk"), "voice URLs are noise");
}

/// When QQ omits the quoted content, the adapter's own memory supplies it — here a reply the bot
/// sent earlier — and an unknown quote is still announced rather than dropped.
#[test]
fn quote_without_elements_falls_back_to_memory() {
    let mut quotes = QuoteStore::new(16);
    quotes.insert(
        "REFIDX_bot".into(),
        Quote {
            text: "北京明天晴".into(),
            images: vec![],
        },
    );
    let remembered = group_at(json!({
        "message_type": 103,
        "message_scene": {"ext": ["ref_msg_idx=REFIDX_bot"]},
        "msg_elements": [{"msg_idx": "REFIDX_bot"}],
    }));
    let event = map("GROUP_AT_MESSAGE_CREATE", &remembered, &mut quotes);
    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("expected a reply segment");
    };
    assert_eq!(reply.snippet, "北京明天晴");

    let unknown = group_at(json!({"message_scene": {"ext": ["ref_msg_idx=REFIDX_gone"]}}));
    let event = map("GROUP_AT_MESSAGE_CREATE", &unknown, &mut quotes);
    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("an unresolvable quote is still a reply segment");
    };
    assert_eq!(reply.snippet, "[unavailable]");
}

/// Every message is remembered under its `msg_idx`, so quoting it later works without QQ's help.
#[test]
fn inbound_messages_are_remembered_for_later_quotes() {
    let mut quotes = QuoteStore::new(16);
    let first = json!({
        "id": "m1",
        "content": "原话",
        "author": {"user_openid": "U1"},
        "message_scene": {"ext": ["msg_idx=REFIDX_1"]},
        "attachments": [{"content_type": "image/png", "url": "https://x/p.png"}],
    });
    map("C2C_MESSAGE_CREATE", &first, &mut quotes);
    assert_eq!(
        quotes.get("REFIDX_1"),
        Some(&Quote {
            text: "原话 [image]".into(),
            images: vec!["https://x/p.png".into()],
        })
    );

    let second = json!({
        "id": "m2",
        "content": "再看看",
        "author": {"user_openid": "U1"},
        "message_scene": {"ext": ["ref_msg_idx=REFIDX_1"]},
    });
    let event = map("C2C_MESSAGE_CREATE", &second, &mut quotes);
    assert_eq!(event.channel_id, "c2c:U1");
    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("expected a reply segment");
    };
    assert_eq!(reply.snippet, "原话 [image]");
    assert!(matches!(event.segments[1].segment, Some(Segment::Image(_))));
}

/// Own attachments: voice becomes its transcript, files keep their name and size.
#[test]
fn own_voice_and_file_attachments_map_to_readable_segments() {
    let data = json!({
        "id": "m1",
        "content": "",
        "author": {"user_openid": "U1"},
        "attachments": [
            {"content_type": "voice", "url": "https://x/v.silk", "asr_refer_text": "帮我查天气"},
            {"content_type": "file", "url": "https://x/f", "filename": "a.txt", "size": 12},
        ],
    });
    let event = map("C2C_MESSAGE_CREATE", &data, &mut QuoteStore::new(4));
    assert_eq!(event.raw_text, "[voice: 帮我查天气] [file:a.txt]");
    let Some(Segment::Custom(voice)) = &event.segments[0].segment else {
        panic!("voice is a custom segment");
    };
    assert_eq!(voice.type_name, "qqofficial.voice");
    assert_eq!(
        voice.payload.as_ref().unwrap().fields["text"].kind,
        Some(Kind::StringValue("帮我查天气".into()))
    );
    let Some(Segment::Custom(file)) = &event.segments[1].segment else {
        panic!("file is a custom segment");
    };
    assert_eq!(file.type_name, "qqofficial.file");
    assert_eq!(
        file.payload.as_ref().unwrap().fields["file_name"].kind,
        Some(Kind::StringValue("a.txt".into()))
    );
}

/// Guild messages: the bot's own @-marker goes, schemeless attachment URLs get https, and a
/// `message_reference` quote resolves from memory.
#[test]
fn guild_messages_are_cleaned_and_quote_by_reference() {
    let mut quotes = QuoteStore::new(8);
    quotes.insert(
        "g0".into(),
        Quote {
            text: "上一条".into(),
            images: vec![],
        },
    );
    let data = json!({
        "id": "g1",
        "content": "<@!BOT> 看 <@!U2>",
        "channel_id": "C1",
        "guild_id": "GU1",
        "author": {"id": "U1", "username": "alice"},
        "attachments": [{"content_type": "image/png", "url": "gchat.qpic.cn/x.png"}],
        "message_reference": {"message_id": "g0"},
    });
    let event = map("AT_MESSAGE_CREATE", &data, &mut quotes);
    assert_eq!(event.channel_id, "guild:C1");
    assert_eq!(event.raw_text, "看 @U2 [image]");
    let Some(Segment::Reply(reply)) = &event.segments[0].segment else {
        panic!("expected a reply segment");
    };
    assert_eq!(reply.snippet, "上一条");
    let Some(Segment::Image(image)) = &event.segments[2].segment else {
        panic!("expected the guild image");
    };
    assert_eq!(
        image.source,
        Some(image_segment::Source::Url(
            "https://gchat.qpic.cn/x.png".into()
        ))
    );
}

/// Unmentioned group messages report whether the bot was mentioned from `mentions[].is_you`.
#[test]
fn group_message_mention_comes_from_is_you() {
    let data = group_at(json!({"mentions": [{"is_you": false}, {"is_you": true}]}));
    let event = map("GROUP_MESSAGE_CREATE", &data, &mut QuoteStore::new(4));
    assert_eq!(
        metadata(&event, "kanon.bot_mentioned"),
        Kind::BoolValue(true)
    );

    let data = group_at(json!({}));
    let event = map("GROUP_MESSAGE_CREATE", &data, &mut QuoteStore::new(4));
    assert_eq!(
        metadata(&event, "kanon.bot_mentioned"),
        Kind::BoolValue(false)
    );
}

/// Non-message dispatches are ignored; a message without its IDs is an explicit error.
#[test]
fn other_events_are_ignored_and_incomplete_messages_rejected() {
    let mut quotes = QuoteStore::new(4);
    assert!(
        map_event("GROUP_ADD_ROBOT", &json!({}), "BOT", &mut quotes)
            .unwrap()
            .is_none()
    );
    assert!(
        map_event(
            "C2C_MESSAGE_CREATE",
            &json!({"id": "x"}),
            "BOT",
            &mut quotes
        )
        .is_err()
    );
}

#[test]
fn clean_content_keeps_ordinary_angle_brackets() {
    assert_eq!(clean_content("a <b> c", ""), "a <b> c");
    assert_eq!(clean_content("1 < 2", ""), "1 < 2");
    assert_eq!(
        clean_content(r#"<faceType=1,faceId="1",ext="!!">"#, ""),
        "[表情]"
    );
}
