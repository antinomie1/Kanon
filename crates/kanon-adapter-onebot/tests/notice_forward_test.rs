//! OneBot notices, merged forwards, @-mention names and quotes of ingested events.

use kanon_adapter_onebot::mapping::{
    Request, attach_forward, delivery, forward_ids, map_event, map_notice, map_request,
    name_mention, needs_lookup, unnamed_mentions,
};
use kanon_proto::prost_types::value::Kind;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, MessageSegment, PipelineEventRequest, ReplySegment, TextSegment,
};
use serde_json::{Value, json};

fn meta(event: &PipelineEventRequest, key: &str) -> Option<String> {
    match event.metadata.as_ref()?.fields.get(key)?.kind.as_ref()? {
        Kind::StringValue(text) => Some(text.clone()),
        _ => None,
    }
}

fn group_message(message: Value) -> Value {
    json!({
        "post_type": "message", "message_type": "group", "self_id": 10001,
        "user_id": 20002, "group_id": 30003, "message_id": 555, "time": 1700000000,
        "sender": {"nickname": "Alice"}, "message": message,
    })
}

#[test]
fn a_recall_names_the_event_id_its_message_was_ingested_with() {
    let message = map_event(
        "onebot",
        group_message(json!([{"type": "text", "data": {"text": "hi"}}])),
    )
    .unwrap()
    .unwrap();
    let notice = map_notice(
        "onebot",
        &json!({"post_type": "notice", "notice_type": "group_recall", "self_id": 10001,
                "group_id": 30003, "user_id": 20002, "operator_id": 40004, "message_id": 555, "time": 1}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        meta(&notice.event, "kanon.notice").as_deref(),
        Some("recall")
    );
    assert_eq!(
        meta(&notice.event, "kanon.notice_target"),
        Some(message.event_id)
    );
    assert_eq!(
        notice.actor_id,
        Some(40004),
        "the operator who recalled it is the actor"
    );
}

#[test]
fn joins_and_pokes_of_the_bot_become_notices() {
    let join = |user: i64| {
        map_notice(
            "onebot",
            &json!({"post_type": "notice", "notice_type": "group_increase", "sub_type": "invite",
                    "self_id": 10001, "group_id": 30003, "user_id": user, "operator_id": 40004}),
        )
        .unwrap()
        .unwrap()
    };
    let member = join(20002);
    assert_eq!(
        meta(&member.event, "kanon.notice").as_deref(),
        Some("member_join")
    );
    assert_eq!(member.event.channel_id, "group:30003");
    assert_eq!(member.event.sender_id, "20002");
    let bot = join(10001);
    assert_eq!(
        meta(&bot.event, "kanon.notice").as_deref(),
        Some("bot_join")
    );
    assert_eq!(
        bot.actor_id,
        Some(40004),
        "the inviter is who the bot greets"
    );

    let poke = |target: i64| {
        map_notice(
            "onebot",
            &json!({"post_type": "notice", "notice_type": "notify", "sub_type": "poke",
                    "self_id": 10001, "user_id": 20002, "target_id": target}),
        )
        .unwrap()
    };
    let poked = poke(10001).unwrap();
    assert_eq!(meta(&poked.event, "kanon.notice").as_deref(), Some("poke"));
    assert_eq!(poked.event.channel_id, "private:20002");
    assert!(
        poke(99999).is_none(),
        "poking someone else is not the bot's business"
    );
}

#[test]
fn only_friend_requests_and_invitations_are_candidates_for_auto_accept() {
    let friend =
        map_request(&json!({"post_type": "request", "request_type": "friend", "flag": "f1"}));
    assert!(matches!(friend, Some(Request::Friend { flag }) if flag == "f1"));
    let invite = map_request(
        &json!({"post_type": "request", "request_type": "group", "sub_type": "invite", "flag": "g1"}),
    );
    assert!(matches!(invite, Some(Request::GroupInvite { flag }) if flag == "g1"));
    let join = map_request(
        &json!({"post_type": "request", "request_type": "group", "sub_type": "add", "flag": "a1"}),
    );
    assert!(
        join.is_none(),
        "someone else asking to join is for the group admins"
    );
}

/// Both response shapes: NapCat's `messages` objects and the standard's `message` nodes.
#[test]
fn forwards_expand_from_either_response_shape() {
    for data in [
        json!({"messages": [
            {"sender": {"nickname": "Bob", "card": "鲍勃"}, "message": [
                {"type": "text", "data": {"text": "看这个"}},
                {"type": "image", "data": {"file": "x.jpg", "url": "https://img/x.jpg"}}
            ]}
        ]}),
        json!({"message": [
            {"type": "node", "data": {"nickname": "鲍勃", "user_id": "1", "content": [
                {"type": "text", "data": {"text": "看这个"}},
                {"type": "image", "data": {"file": "x.jpg", "url": "https://img/x.jpg"}}
            ]}}
        ]}),
    ] {
        let mut event = map_event(
            "onebot",
            group_message(json!([{"type": "forward", "data": {"id": "F1"}}])),
        )
        .unwrap()
        .unwrap();
        assert_eq!(forward_ids(&event), vec!["F1".to_string()]);
        assert!(needs_lookup(&event));
        attach_forward(&mut event, "F1", &data).unwrap();
        let Some(Segment::Custom(custom)) = &event.segments[0].segment else {
            panic!("forward stays a custom segment");
        };
        let messages = kanon_proto::prost_types::Value {
            kind: custom.payload.as_ref().unwrap().fields["messages"]
                .kind
                .clone(),
        };
        let Some(Kind::ListValue(list)) = messages.kind else {
            panic!("messages is a list");
        };
        let Some(Kind::StructValue(first)) = &list.values[0].kind else {
            panic!("each message is an object");
        };
        let field = |key: &str| first.fields[key].kind.clone();
        assert_eq!(field("sender"), Some(Kind::StringValue("鲍勃".into())));
        assert_eq!(
            field("text"),
            Some(Kind::StringValue("看这个[image]".into()))
        );
        let Some(Kind::ListValue(images)) = field("images") else {
            panic!("images is a list");
        };
        assert_eq!(images.values.len(), 1);
    }
}

#[test]
fn mentions_get_names_and_quotes_of_events_use_the_message_id() {
    let mut event = map_event(
        "onebot",
        group_message(json!([{"type": "at", "data": {"qq": "20002"}}, {"type": "text", "data": {"text": " 你好"}}])),
    )
    .unwrap()
    .unwrap();
    assert_eq!(unnamed_mentions(&event), vec![20002]);
    name_mention(&mut event, 20002, "Alice");
    assert!(unnamed_mentions(&event).is_empty());
    assert!(!needs_lookup(&event));

    let (_, params) = delivery(&DeliverMessageRequest {
        channel_id: "group:30003".into(),
        segments: vec![
            MessageSegment {
                segment: Some(Segment::Reply(ReplySegment {
                    target_message_id: event.event_id.clone(),
                    snippet: String::new(),
                })),
            },
            MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "嗨".into(),
                })),
            },
        ],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        params["message"][0],
        json!({"type": "reply", "data": {"id": "555"}})
    );
}
