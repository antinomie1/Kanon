//! Split replies must leave room for QQ's native media messages and asynchronous typing reply.

use kanon_adapter_qqofficial::{QqOfficialAdapter, QqOfficialConfig, mapping::EVENT_ID_PREFIX};
use kanon_core::PlatformAdapter;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, DeliverMessageRequest, FileSegment, ImageSegment, MessageSegment, ReplySegment,
    TextSegment, VideoSegment,
};

/// Reply with text, a quote, and a configurable number of native media messages.
fn reply(scene: &str, event_id: &str, media: usize) -> DeliverMessageRequest {
    let mut segments = vec![
        MessageSegment {
            segment: Some(Segment::Reply(ReplySegment::default())),
        },
        MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: "first\nsecond\nlast".to_string(),
            })),
        },
    ];
    for n in 0..media {
        segments.push(MessageSegment {
            segment: Some(match n % 4 {
                0 => Segment::Image(ImageSegment::default()),
                1 => Segment::Audio(AudioSegment::default()),
                2 => Segment::Video(VideoSegment::default()),
                _ => Segment::File(FileSegment::default()),
            }),
        });
    }
    DeliverMessageRequest {
        platform: "qqofficial".to_string(),
        channel_id: format!("{scene}:target"),
        event_id: event_id.to_string(),
        segments,
        ..Default::default()
    }
}

#[test]
fn group_and_c2c_budgets_include_media_and_possible_typing_acknowledgement() {
    let adapter = QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap();
    for media in 0..=3 {
        let group = adapter.reply_message_limit(&reply("group", "message-id", media));
        let c2c = adapter.reply_message_limit(&reply("c2c", "message-id", media));
        assert_eq!(group, 5 - media);
        assert_eq!(c2c, 4 - media);
        assert_eq!(group + media, 5);
        assert_eq!(
            c2c + media + 1,
            5,
            "reserve an acknowledgement even before it completes"
        );
    }
}

#[test]
fn gateway_events_do_not_reserve_a_typing_reply_that_cannot_be_sent() {
    let adapter = QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap();
    let event_id = format!("{EVENT_ID_PREFIX}gateway-event");
    assert_eq!(adapter.reply_message_limit(&reply("c2c", &event_id, 2)), 3);
}

#[test]
fn scenarios_without_a_passive_budget_do_not_create_additional_deliveries() {
    let adapter = QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap();
    for scene in ["guild", "guild_dm", "invalid"] {
        assert_eq!(
            adapter.reply_message_limit(&reply(scene, "message-id", 0)),
            1
        );
    }
    for scene in ["group", "c2c"] {
        assert_eq!(adapter.reply_message_limit(&reply(scene, "", 0)), 1);
        assert_eq!(
            adapter.reply_message_limit(&reply(scene, "message-id", 10)),
            1,
            "splitting must not add text deliveries when media already exhausts the budget"
        );
    }
}

#[test]
fn videos_and_files_reserve_native_messages_including_mixed_attachments() {
    let adapter = QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap();
    let video = Segment::Video(VideoSegment::default());
    let file = Segment::File(FileSegment::default());
    for attachments in [
        vec![video.clone()],
        vec![file.clone()],
        vec![Segment::Image(ImageSegment::default()), video, file],
    ] {
        for (scene, budget) in [("group", 5), ("c2c", 4)] {
            let mut request = reply(scene, "message-id", 0);
            request
                .segments
                .extend(attachments.iter().cloned().map(|segment| MessageSegment {
                    segment: Some(segment),
                }));
            assert_eq!(
                adapter.reply_message_limit(&request),
                budget - attachments.len(),
                "text deliveries must leave room for every native attachment in {scene}",
            );
        }
    }
}
