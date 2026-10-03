//! Shutdown must not lose anything the node already accepted.
//!
//! Events still queued when the node stops are written to the dead-letter log instead of being
//! dropped with the worker, and replies a platform cannot take before the delivery grace runs out
//! are recorded instead of vanishing with the dispatcher.

use std::sync::Arc;

use kanon_core::pipeline::{DeadLetterDirection, DeadLetterRecord, DeadLetterWriter};
use kanon_core::supervisor::Supervisor;
use kanon_core::{AdapterError, PipelineEngine, PlatformAdapter};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest, MessageSegment,
    PipelineEventRequest, TextSegment,
};
use tokio::sync::mpsc;

/// A platform that accepts a delivery and never answers.
struct HangingAdapter;

#[async_trait::async_trait]
impl PlatformAdapter for HangingAdapter {
    fn platform(&self) -> &str {
        "hanging_im"
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        std::future::pending().await
    }
}

fn text(content: &str) -> Vec<MessageSegment> {
    vec![MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }]
}

fn dead_letters(dir: &std::path::Path) -> Vec<DeadLetterRecord> {
    let mut records = Vec::new();
    for entry in std::fs::read_dir(dir).expect("dead letter dir") {
        let raw = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        records.extend(raw.lines().map(|line| serde_json::from_str(line).unwrap()));
    }
    records
}

async fn engine(dir: &std::path::Path) -> Arc<PipelineEngine> {
    let supervisor = Arc::new(Supervisor::new(Some(dir.to_path_buf()), None));
    supervisor
        .adapters()
        .register(Arc::new(HangingAdapter))
        .await
        .expect("register adapter");
    Arc::new(
        PipelineEngine::new(supervisor)
            .with_dead_letter(Arc::new(DeadLetterWriter::new(dir.join("dead_letter")))),
    )
}

/// Queued events are recorded, not started, and the queue refuses new ones afterwards.
#[tokio::test]
async fn queued_events_are_dead_lettered_on_shutdown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = engine(dir.path()).await;

    let (ingest, events) = mpsc::channel(8);
    for id in ["evt-1", "evt-2"] {
        ingest
            .try_send(IngestEventRequest {
                platform: "hanging_im".to_string(),
                event: Some(PipelineEventRequest {
                    event_id: id.to_string(),
                    platform: "hanging_im".to_string(),
                    channel_id: "group:1".to_string(),
                    sender_id: "alice".to_string(),
                    raw_text: "hello".to_string(),
                    segments: text("hello"),
                    metadata: None,
                }),
            })
            .unwrap();
    }

    // The test runtime is single-threaded, so the worker first runs after shutdown has begun.
    let worker = engine.clone().start_worker(events);
    engine.drain(worker, None).await;

    let records = dead_letters(&dir.path().join("dead_letter"));
    let ids: Vec<_> = records.iter().map(|r| r.event_id.as_str()).collect();
    assert_eq!(ids, ["evt-1", "evt-2"]);
    assert!(
        records
            .iter()
            .all(|r| r.direction == DeadLetterDirection::Inbound && r.sender_id == "alice")
    );
    assert!(matches!(
        ingest.try_send(IngestEventRequest::default()),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
}

/// Replies the platform does not take before the delivery grace ends are recorded.
#[tokio::test]
async fn undelivered_replies_are_dead_lettered_on_shutdown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = engine(dir.path()).await;
    let dispatcher = engine.clone().start_outbound_dispatcher();
    let (_ingest, events) = mpsc::channel(1);
    let worker = engine.clone().start_worker(events);

    let outbound = engine.outbound_sender();
    for id in ["reply-1", "reply-2"] {
        outbound
            .send(
                DeliverMessageRequest {
                    platform: "hanging_im".to_string(),
                    channel_id: "group:1".to_string(),
                    recipient_id: "alice".to_string(),
                    segments: text(id),
                    event_id: id.to_string(),
                }
                .into(),
            )
            .await
            .unwrap();
    }
    // Let the platform worker start the first delivery, which never completes.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    engine.drain(worker, dispatcher).await;

    let records = dead_letters(&dir.path().join("dead_letter"));
    let ids: Vec<_> = records.iter().map(|r| r.event_id.as_str()).collect();
    assert_eq!(ids, ["reply-1", "reply-2"]);
    assert!(records.iter().all(|r| {
        r.direction == DeadLetterDirection::Outbound && r.reason.contains("shut down")
    }));
    assert!(
        outbound
            .try_send(DeliverMessageRequest::default().into())
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_delivery_releases_half_open_probe() {
    use kanon_core::supervisor::{CircuitBreaker, CircuitBreakerConfig};
    use std::future::Future;
    use std::task::Poll;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path()).await;
    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        cooldown_period: Duration::ZERO,
        ..CircuitBreakerConfig::for_platform()
    });
    breaker.trip("recovering platform");
    let mut call = Box::pin(engine.dispatch_outbound_request_with_breaker(
        DeliverMessageRequest {
            platform: "hanging_im".to_string(),
            ..Default::default()
        },
        &breaker,
    ));
    // Poll into the hanging adapter, then cancel the caller while the probe is held.
    std::future::poll_fn(|cx| {
        assert!(call.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(!breaker.is_available());
    drop(call);
    assert!(breaker.try_acquire().is_some());
}
