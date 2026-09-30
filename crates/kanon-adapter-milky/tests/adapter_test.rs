//! End-to-end adapter behaviour against a fake Milky protocol implementation.
//!
//! These tests drive the real adapter — real HTTP client, real SSE/WebSocket framing, real
//! reconnect loop — and assert what an operator or a user would observe: which conversation a
//! message lands in, whether a reply reaches the platform, what the console reports, and that a
//! disabled or misconfigured adapter fails loudly instead of quietly.

mod common;

use std::time::Duration;

use common::{FakeMilky, Transport, friend_message_event, group_message_event};
use kanon_adapter_milky::adapter::{ConnectionState, MilkyAdapter, MilkyStatus};
use kanon_adapter_milky::config::MilkyConfig;
use kanon_core::{EventIngress, PlatformAdapter};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, IngestEventRequest, MessageSegment, TextSegment};
use serde_json::json;
use tokio::sync::mpsc;

/// Timeout for conditions that should be satisfied almost immediately.
const SETTLE: Duration = Duration::from_secs(5);

/// Builds a configuration pointing at a fake implementation.
fn config_for(fake: &FakeMilky, enabled: bool) -> MilkyConfig {
    MilkyConfig {
        enabled,
        base_url: fake.base_url(),
        ..MilkyConfig::default()
    }
    .prepare()
    .expect("test configuration should be valid")
}

/// Builds a Kanon text segment.
fn text(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }
}

/// Builds a delivery request for a channel identifier.
fn deliver_request(channel_id: &str, segments: Vec<MessageSegment>) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: "milky".to_string(),
        channel_id: channel_id.to_string(),
        recipient_id: "20002".to_string(),
        segments,
        event_id: "evt-test".to_string(),
    }
}

/// Polls a synchronous predicate until it holds or the settle timeout expires.
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was not satisfied within {SETTLE:?}");
}

/// Waits until the adapter has cached the login information of the endpoint.
async fn wait_for_login(adapter: &MilkyAdapter) -> MilkyStatus {
    for _ in 0..500 {
        let status = adapter.status();
        if status.login.is_some() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("login information was not cached within {SETTLE:?}");
}

/// Receives one ingested event, failing the test if none arrives.
async fn next_ingest(receiver: &mut mpsc::Receiver<IngestEventRequest>) -> IngestEventRequest {
    tokio::time::timeout(SETTLE, receiver.recv())
        .await
        .expect("an event should be ingested within the timeout")
        .expect("the ingest channel should stay open")
}

/// An SSE deployment ingests inbound messages and delivers replies back to the platform.
#[tokio::test]
async fn sse_stream_ingests_messages_and_delivers_replies() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.wait_for_subscription(0).await;
    fake.push_event(friend_message_event(77, 20002, "hello"));

    let ingested = next_ingest(&mut receiver).await;
    assert_eq!(ingested.platform, "milky");
    let request = ingested
        .event
        .expect("the ingest request should carry an event");
    assert_eq!(request.channel_id, "friend:20002");
    assert_eq!(request.sender_id, "20002");
    assert_eq!(request.raw_text, "hello");
    assert_eq!(request.event_id, "milky:10001:friend:20002:20002:77");

    wait_until(|| adapter.is_connected()).await;
    let status = wait_for_login(&adapter).await;
    assert_eq!(status.state, ConnectionState::Connected);
    assert!(status.connected);
    assert_eq!(status.events_received, 1);
    assert_eq!(status.messages_ingested, 1);
    assert_eq!(status.messages_rejected, 0);
    assert_eq!(status.login.expect("login").uin, 10001);
    assert_eq!(
        status.implementation.expect("implementation").impl_name,
        "FakeMilky"
    );

    let response = adapter
        .deliver(deliver_request(
            "friend:20002",
            vec![text("hi back"), text("!")],
        ))
        .await
        .expect("delivery should succeed");

    assert!(response.success);
    assert_eq!(response.message_id, "4242");
    assert!(response.error_message.is_empty());
    assert_eq!(
        fake.call("send_private_message").body,
        json!({
            "user_id": 20002,
            "message": [
                { "type": "text", "data": { "text": "hi back" } },
                { "type": "text", "data": { "text": "!" } },
            ],
        })
    );
    assert_eq!(adapter.status().messages_delivered, 1);
}

/// A WebSocket deployment behaves identically, including group routing.
#[tokio::test]
async fn websocket_stream_ingests_messages_and_delivers_group_replies() {
    let fake = FakeMilky::start(Transport::Ws).await;
    let adapter = MilkyAdapter::new(MilkyConfig {
        transport: kanon_adapter_milky::TransportKind::Websocket,
        ..config_for(&fake, true)
    })
    .expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.wait_for_subscription(0).await;
    fake.push_event(group_message_event(88, 30003, 20002, "ping"));

    let request = next_ingest(&mut receiver)
        .await
        .event
        .expect("the ingest request should carry an event");
    assert_eq!(request.channel_id, "group:30003");

    let response = adapter
        .deliver(deliver_request("group:30003", vec![text("pong")]))
        .await
        .expect("delivery should succeed");
    assert!(response.success);
    assert_eq!(
        fake.call("send_group_message").body,
        json!({
            "group_id": 30003,
            "message": [ { "type": "text", "data": { "text": "pong" } } ],
        })
    );
}

/// A disabled adapter holds no connection and refuses deliveries explicitly.
#[tokio::test]
async fn disabled_adapter_opens_no_connection_and_rejects_delivery() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, false)).expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("starting a disabled adapter is not an error");

    let error = adapter
        .deliver(deliver_request("friend:20002", vec![text("hi")]))
        .await
        .expect_err("a disabled adapter must not claim a delivery");

    assert!(error.to_string().contains("disabled"));
    assert!(fake.calls().is_empty(), "no API call should be attempted");
    assert!(receiver.try_recv().is_err());

    let status = adapter.status();
    assert_eq!(status.state, ConnectionState::Disabled);
    assert!(!status.connected);
    assert!(!status.enabled);
}

/// A recall reaches the core as a notice naming the recalled message, never as chat; an event
/// Kanon has no use for is only counted.
#[tokio::test]
async fn recalls_become_notices_and_other_events_are_only_counted() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.wait_for_subscription(0).await;
    fake.push_event(json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "group_name_change",
        "data": {"group_id": 30003, "new_group_name": "新群名", "operator_id": 20002}
    }));
    fake.push_event(json!({
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

    let notice = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("the recall should be ingested")
        .expect("channel open")
        .event
        .expect("event present");
    let field = |key: &str| {
        notice.metadata.as_ref().unwrap().fields[key]
            .kind
            .clone()
            .unwrap()
    };
    use kanon_proto::prost_types::value::Kind;
    assert_eq!(field("kanon.notice"), Kind::StringValue("recall".into()));
    assert_eq!(
        field("kanon.notice_target"),
        Kind::StringValue("milky:10001:group:30003:20002:5".into())
    );
    assert!(
        notice.segments.is_empty(),
        "a recall carries no chat content"
    );
    assert!(receiver.try_recv().is_err(), "the rename is not ingested");
    assert_eq!(adapter.status().events_received, 2);
}

/// A dropped connection is re-established and inbound delivery resumes.
#[tokio::test]
async fn stream_reconnects_after_the_connection_drops() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.wait_for_subscription(0).await;
    fake.push_event(friend_message_event(1, 20002, "first"));
    assert_eq!(
        next_ingest(&mut receiver)
            .await
            .event
            .expect("event")
            .raw_text,
        "first"
    );

    // Ending the stream must not end the adapter: it reconnects after its backoff.
    fake.drop_subscribers();
    fake.wait_for_subscription(1).await;
    fake.push_event(friend_message_event(2, 20002, "second"));

    let second = next_ingest(&mut receiver).await;
    assert_eq!(
        second.event.expect("event").raw_text,
        "second",
        "the adapter should keep ingesting after a reconnect"
    );
    assert_eq!(adapter.status().messages_ingested, 2);
}

/// Reconfiguration is hot: the endpoint changes without restarting the node.
#[tokio::test]
async fn apply_swaps_the_endpoint_without_a_restart() {
    let first = FakeMilky::start(Transport::Sse).await;
    let second = FakeMilky::start(Transport::Sse).await;

    let adapter = MilkyAdapter::new(config_for(&first, true)).expect("adapter should build");
    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    first.wait_for_subscription(0).await;
    first.push_event(friend_message_event(1, 20002, "from first"));
    assert_eq!(
        next_ingest(&mut receiver)
            .await
            .event
            .expect("event")
            .raw_text,
        "from first"
    );

    let status = adapter
        .apply(config_for(&second, true))
        .await
        .expect("reconfiguration should succeed");
    assert_eq!(status.base_url, second.base_url());
    assert!(status.enabled);

    // The new endpoint is the one that is now observed and used.
    second.wait_for_subscription(0).await;
    second.push_event(friend_message_event(2, 20002, "from second"));
    assert_eq!(
        next_ingest(&mut receiver)
            .await
            .event
            .expect("event")
            .raw_text,
        "from second"
    );

    adapter
        .deliver(deliver_request("friend:20002", vec![text("hello")]))
        .await
        .expect("delivery should succeed");
    assert_eq!(
        second.call("send_private_message").body,
        json!({
            "user_id": 20002,
            "message": [ { "type": "text", "data": { "text": "hello" } } ],
        })
    );
    assert!(
        first
            .calls()
            .iter()
            .all(|call| call.endpoint != "send_private_message"),
        "the retired endpoint must not receive deliveries"
    );
}

/// Identity fields are rejected rather than silently diverging from the registry catalog.
#[tokio::test]
async fn apply_rejects_identity_changes() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");
    let original = adapter.config();

    let renamed = MilkyConfig {
        display_name: Some("Another name".to_string()),
        ..original.clone()
    };
    let error = adapter
        .apply(renamed)
        .await
        .expect_err("a display-name change must be rejected");
    assert!(error.to_string().contains("display name is fixed"));

    let replatformed = MilkyConfig {
        platform: "other".to_string(),
        ..original.clone()
    };
    let error = adapter
        .apply(replatformed)
        .await
        .expect_err("a platform change must be rejected");
    assert!(error.to_string().contains("platform identifier is fixed"));

    // A rejected reconfiguration leaves the running adapter untouched.
    assert_eq!(adapter.config(), original);
}

/// A configuration saved before the core supplies its ingest handle comes up when it arrives.
#[tokio::test]
async fn configuration_before_start_connects_once_started() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    // Reconfiguring before the core starts the adapter must not open a stream that can deliver
    // nowhere.
    adapter
        .apply(config_for(&fake, true))
        .await
        .expect("reconfiguration should succeed");
    assert_eq!(adapter.status().state, ConnectionState::Connecting);
    assert!(fake.calls().is_empty());

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.wait_for_subscription(0).await;
    fake.push_event(friend_message_event(9, 20002, "late start"));
    assert_eq!(
        next_ingest(&mut receiver)
            .await
            .event
            .expect("event")
            .raw_text,
        "late start"
    );
}

/// Stopping the adapter releases the stream and stops ingesting.
#[tokio::test]
async fn stop_releases_the_stream() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    let (sender, mut receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");
    fake.wait_for_subscription(0).await;

    adapter.stop().await.expect("stop should succeed");

    let status = adapter.status();
    assert_eq!(status.state, ConnectionState::Disabled);
    assert!(!adapter.is_connected());

    fake.push_event(friend_message_event(1, 20002, "after stop"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        receiver.try_recv().is_err(),
        "a stopped adapter must not ingest"
    );
}

/// A platform-side rejection becomes an explicit delivery error and is surfaced in the status.
#[tokio::test]
async fn platform_rejection_is_reported_as_a_delivery_error() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");
    let (sender, _receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    fake.fail_with_retcode(-404, "friend not found");
    let error = adapter
        .deliver(deliver_request("friend:20002", vec![text("hi")]))
        .await
        .expect_err("a rejected send must fail");

    assert!(error.to_string().contains("-404"));
    assert!(error.to_string().contains("friend not found"));
    assert_eq!(adapter.status().messages_delivered, 0);
    assert!(
        adapter
            .status()
            .last_error
            .expect("failure should be recorded")
            .contains("friend not found")
    );
}

/// Temporary conversations cannot be answered and say so instead of sending somewhere else.
#[tokio::test]
async fn temporary_conversation_delivery_fails_explicitly() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");
    let (sender, _receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    let error = adapter
        .deliver(deliver_request("temp:40004", vec![text("hi")]))
        .await
        .expect_err("a temporary conversation has no send endpoint");

    assert!(error.to_string().contains("temporary session"));
    assert!(fake.calls().is_empty());
}

/// A malformed channel identifier fails loudly rather than guessing a destination.
#[tokio::test]
async fn unroutable_channel_fails_explicitly() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");
    let (sender, _receiver) = mpsc::channel(8);
    adapter
        .start(EventIngress::new(sender))
        .await
        .expect("adapter should start");

    let error = adapter
        .deliver(deliver_request("20002", vec![text("hi")]))
        .await
        .expect_err("a bare peer number is not routable");

    assert!(
        error
            .to_string()
            .contains("does not name a Milky conversation")
    );
    assert!(fake.calls().is_empty());
}

/// A connectivity probe reports what answered, without touching the running adapter.
#[tokio::test]
async fn connection_test_reports_identity_and_latency() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let adapter = MilkyAdapter::new(config_for(&fake, false)).expect("adapter should build");

    let report = adapter
        .test_connection(Some(config_for(&fake, true)))
        .await
        .expect("the probe should succeed");

    assert_eq!(report.login.uin, 10001);
    assert_eq!(report.login.nickname, "Kanon Test");
    assert_eq!(report.implementation.impl_name, "FakeMilky");
    assert_eq!(report.implementation.milky_version, "1.3");

    // Testing is a probe, not an activation: the stored configuration stays disabled.
    assert!(!adapter.status().enabled);
    assert_eq!(adapter.status().state, ConnectionState::Disabled);
}

/// A failed probe reports the platform's own reason.
#[tokio::test]
async fn connection_test_reports_failure() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_retcode(-403, "not logged in");
    let adapter = MilkyAdapter::new(config_for(&fake, true)).expect("adapter should build");

    let error = adapter
        .test_connection(None)
        .await
        .expect_err("the probe should fail");

    assert!(error.to_string().contains("not logged in"));
}

/// An invalid configuration is rejected at construction, so a bad node never starts.
#[tokio::test]
async fn invalid_configuration_is_rejected_at_construction() {
    // `MilkyAdapter` deliberately implements neither `Debug` nor `Clone`, so the error is matched
    // rather than unwrapped with `expect_err`.
    let error = match MilkyAdapter::new(MilkyConfig {
        platform: "Milky Bot".to_string(),
        ..MilkyConfig::default()
    }) {
        Ok(_) => panic!("an invalid platform identifier must be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("invalid character"));
}
