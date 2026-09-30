//! Milky notices and merged forwards.

use kanon_adapter_milky::mapping::{attach_forward, forward_ids, inbound_message, map_notice};
use kanon_adapter_milky::protocol::{Event, IncomingForwardedMessage};
use kanon_proto::prost_types::value::Kind;
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::message_segment::Segment;
use serde_json::{Value, json};

fn event(value: Value) -> Event {
    serde_json::from_value(value).expect("fixture event should decode")
}

fn meta(event: &PipelineEventRequest, key: &str) -> Option<String> {
    match event.metadata.as_ref()?.fields.get(key)?.kind.as_ref()? {
        Kind::StringValue(text) => Some(text.clone()),
        _ => None,
    }
}

fn friend_message(segments: Value) -> Event {
    event(json!({
        "time": 1_700_000_000_i64, "self_id": 10001, "event_type": "message_receive",
        "data": {
            "message_scene": "friend", "peer_id": 20002, "message_seq": 88, "sender_id": 20002,
            "time": 1_700_000_000_i64, "segments": segments,
            "friend": {"user_id": 20002, "nickname": "Alice", "sex": "female", "qid": "", "remark": "",
                       "category": {"category_id": 1, "category_name": "我的好友"}}
        }
    }))
}

#[test]
fn a_recall_names_the_event_id_its_message_was_ingested_with() {
    let message = friend_message(json!([{"type": "text", "data": {"text": "hi"}}]));
    let ingested = inbound_message("milky", 10001, &message).unwrap().unwrap();

    let recall = event(json!({
        "time": 1_700_000_001_i64, "self_id": 10001, "event_type": "message_recall",
        "data": {"message_scene": "friend", "peer_id": 20002, "message_seq": 88,
                 "sender_id": 20002, "operator_id": 20002, "display_suffix": ""}
    }));
    let notice = map_notice("milky", &recall).expect("a recall is a notice");
    assert_eq!(
        meta(&notice.event, "kanon.notice").as_deref(),
        Some("recall")
    );
    assert_eq!(
        meta(&notice.event, "kanon.notice_target"),
        Some(ingested.event_id)
    );
}

#[test]
fn nudges_of_the_bot_and_joins_become_notices() {
    let nudge = |receiver: i64| {
        map_notice(
            "milky",
            &event(json!({
                "time": 1, "self_id": 10001, "event_type": "group_nudge",
                "data": {"group_id": 30003, "sender_id": 20002, "receiver_id": receiver,
                         "display_action": "戳了戳", "display_suffix": "", "display_action_img_url": ""}
            })),
        )
    };
    let poke = nudge(10001).expect("the bot was nudged");
    assert_eq!(meta(&poke.event, "kanon.notice").as_deref(), Some("poke"));
    assert_eq!(poke.event.channel_id, "group:30003");
    assert_eq!(poke.actor_id, Some(20002));
    assert!(nudge(40004).is_none());

    let joined = map_notice(
        "milky",
        &event(json!({
            "time": 1, "self_id": 10001, "event_type": "group_member_increase",
            "data": {"group_id": 30003, "user_id": 10001, "invitor_id": 20002}
        })),
    )
    .expect("the bot joined");
    assert_eq!(
        meta(&joined.event, "kanon.notice").as_deref(),
        Some("bot_join")
    );
    assert_eq!(joined.actor_id, Some(20002));
}

#[test]
fn a_fetched_forward_is_stored_in_its_segment() {
    let message = friend_message(json!([{"type": "forward", "data": {
        "forward_id": "F1", "title": "聊天记录", "preview": [], "summary": "查看2条"
    }}]));
    let mut request = inbound_message("milky", 10001, &message).unwrap().unwrap();
    assert_eq!(forward_ids(&request), vec!["F1".to_string()]);

    let forwarded: Vec<IncomingForwardedMessage> = serde_json::from_value(json!([
        {"message_seq": 1, "sender_name": "Bob", "avatar_url": "", "time": 1, "segments": [
            {"type": "text", "data": {"text": "看图"}},
            {"type": "image", "data": {"resource_id": "r", "temp_url": "https://img/a.png",
                                        "width": 1, "height": 1, "summary": "", "sub_type": "normal"}}
        ]}
    ]))
    .expect("fixture forwarded messages decode");
    attach_forward(&mut request, "F1", &forwarded).unwrap();

    let Some(Segment::Custom(custom)) = &request.segments[0].segment else {
        panic!("forward stays a custom segment");
    };
    let Some(Kind::ListValue(messages)) = &custom.payload.as_ref().unwrap().fields["messages"].kind
    else {
        panic!("messages is a list");
    };
    let Some(Kind::StructValue(first)) = &messages.values[0].kind else {
        panic!("each message is an object");
    };
    assert_eq!(
        first.fields["sender"].kind,
        Some(Kind::StringValue("Bob".into()))
    );
    assert_eq!(
        first.fields["text"].kind,
        Some(Kind::StringValue("看图[image]".into()))
    );
}

#[test]
fn requests_carry_a_token_that_names_what_to_accept() {
    use kanon_adapter_milky::mapping::{AcceptRequest, accept_request_input, map_request};

    let friend = map_request(
        "milky",
        &event(json!({
            "time": 1, "self_id": 10001, "event_type": "friend_request",
            "data": {"initiator_id": 20002, "initiator_uid": "u_abc", "comment": "hi", "via": "search"}
        })),
    )
    .expect("a friend request is reported");
    assert_eq!(
        meta(&friend, "kanon.notice").as_deref(),
        Some("friend_request")
    );
    assert!(matches!(
        accept_request_input(&friend),
        Ok(AcceptRequest::Friend(uid)) if uid == "u_abc"
    ));

    let invite = map_request(
        "milky",
        &event(json!({
            "time": 1, "self_id": 10001, "event_type": "group_invitation",
            "data": {"group_id": 30003, "invitation_seq": 7, "initiator_id": 20002}
        })),
    )
    .expect("an invitation is reported");
    assert!(matches!(
        accept_request_input(&invite),
        Ok(AcceptRequest::Group(30003, 7))
    ));
}
