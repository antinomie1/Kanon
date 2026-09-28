//! Platform adapter route coverage: catalog, Fast-ACK ingress and outbound delivery.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, IngestEventRequest, MessageSegment, TextSegment};
use serde_json::json;

use common::{
    BUILTIN_PLATFORM, PLUGIN_PLATFORM, RecordingAdapter, adapter_state, empty_state, error_code,
    fixture_state, send_json,
};

/// The catalog merges built-in adapters with plugin manifest declarations.
#[tokio::test]
async fn adapter_catalog_lists_builtin_and_plugin_adapters() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;
    let app: Router = app(state);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/adapters", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["total"], 2);

    let adapters = body["adapters"].as_array().expect("adapter array");
    let builtin = adapters
        .iter()
        .find(|adapter| adapter["platform"] == BUILTIN_PLATFORM)
        .expect("built-in adapter listed");
    assert_eq!(builtin["kind"], "builtin");
    assert_eq!(builtin["display_name"], "Built-in fixture adapter");
    assert_eq!(builtin["connected"], true);
    assert!(builtin["host_id"].is_null());

    let plugin = adapters
        .iter()
        .find(|adapter| adapter["platform"] == PLUGIN_PLATFORM)
        .expect("plugin adapter listed");
    assert_eq!(plugin["kind"], "plugin");
    assert_eq!(plugin["display_name"], "Fixture Platform");
    assert_eq!(plugin["connected"], true);
    assert_eq!(plugin["host_id"], common::FIXTURE_HOST_ID);
    assert_eq!(plugin["plugin_id"], common::FIXTURE_PLUGIN_ID);
}

/// A node without adapters reports an empty catalog rather than failing.
#[tokio::test]
async fn adapter_catalog_is_empty_without_adapters() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/adapters", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["total"], 0);
    assert_eq!(body["adapters"].as_array().map(Vec::len), Some(0));
}

/// Inbound messages for a served platform are Fast-ACKed and enqueued verbatim.
#[tokio::test]
async fn ingest_accepts_and_enqueues_event() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, mut ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;
    let app: Router = app(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/adapters/{BUILTIN_PLATFORM}/ingest"),
        Some(json!({
            "channel_id": "chan-1",
            "sender_id": "alice",
            "text": "hello kanon",
            "event_id": "evt-42",
            "metadata": { "update_id": 9001 }
        })),
    )
    .await;

    assert_eq!(status, 202, "body: {body}");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["event_id"], "evt-42");
    assert_eq!(body["platform"], BUILTIN_PLATFORM);

    let queued: IngestEventRequest = tokio::time::timeout(Duration::from_secs(2), ingest_rx.recv())
        .await
        .expect("event enqueued")
        .expect("channel open");
    assert_eq!(queued.platform, BUILTIN_PLATFORM);
    let event = queued.event.expect("inner event");
    assert_eq!(event.event_id, "evt-42");
    assert_eq!(event.platform, BUILTIN_PLATFORM);
    assert_eq!(event.channel_id, "chan-1");
    assert_eq!(event.sender_id, "alice");
    assert_eq!(event.raw_text, "hello kanon");
    // Metadata round-trips into a `google.protobuf.Struct` (numbers are stored as doubles).
    let update_id = event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.fields.get("update_id"))
        .and_then(|value| value.kind.as_ref())
        .and_then(|kind| match kind {
            kanon_proto::prost_types::value::Kind::NumberValue(number) => Some(*number),
            _ => None,
        });
    assert_eq!(update_id, Some(9001.0));
}

/// Plugin-declared platforms accept inbound messages too.
#[tokio::test]
async fn ingest_accepts_plugin_platform() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, mut ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;
    let app: Router = app(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/adapters/{PLUGIN_PLATFORM}/ingest"),
        Some(json!({ "channel_id": "c", "sender_id": "u", "text": "hi" })),
    )
    .await;

    assert_eq!(status, 202, "body: {body}");
    // Without a caller-supplied id the gateway assigns one and reports it for trace correlation.
    assert!(
        body["event_id"].as_str().is_some_and(|id| !id.is_empty()),
        "event id must be generated: {body}"
    );

    let queued = ingest_rx.recv().await.expect("event enqueued");
    assert_eq!(queued.platform, PLUGIN_PLATFORM);
}

/// Messages for a platform no adapter serves are rejected: they could never be answered.
#[tokio::test]
async fn ingest_rejects_unknown_platform() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;
    let app: Router = app(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/adapters/nowhere/ingest",
        Some(json!({ "channel_id": "c", "sender_id": "u", "text": "hi" })),
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// Request bodies are validated before anything is enqueued.
#[tokio::test]
async fn ingest_validates_request() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, mut ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;
    let app: Router = app(state);
    let uri = format!("/api/v1/adapters/{BUILTIN_PLATFORM}/ingest");

    for payload in [
        json!({ "channel_id": "", "sender_id": "u", "text": "hi" }),
        json!({ "channel_id": "c", "sender_id": "  ", "text": "hi" }),
        json!({ "channel_id": "c", "sender_id": "u", "text": "   " }),
    ] {
        let (status, body) = send_json(&app, Method::POST, &uri, Some(payload)).await;
        assert_eq!(status, 400, "body: {body}");
        assert_eq!(error_code(&body), "bad_request");
    }

    assert!(
        ingest_rx.try_recv().is_err(),
        "rejected requests must not enqueue events"
    );
}

/// A saturated ingest queue reports backpressure instead of blocking the caller.
#[tokio::test]
async fn ingest_reports_queue_saturation() {
    let dir = tempfile::tempdir().expect("temp dir");
    // Capacity 1 with no consumer: the second message must be refused explicitly.
    let (state, _ingest_rx, _adapter) = adapter_state(PathBuf::from(dir.path()), 1).await;
    let app: Router = app(state);
    let uri = format!("/api/v1/adapters/{BUILTIN_PLATFORM}/ingest");
    let payload = json!({ "channel_id": "c", "sender_id": "u", "text": "hi" });

    let (first, _) = send_json(&app, Method::POST, &uri, Some(payload.clone())).await;
    assert_eq!(first, 202);

    let (second, body) = send_json(&app, Method::POST, &uri, Some(payload)).await;
    assert_eq!(second, 503);
    assert_eq!(error_code(&body), "unavailable");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("queue")),
        "error must name the saturated queue: {body}"
    );
}

/// A gateway without a pipeline attached refuses inbound messages explicitly.
#[tokio::test]
async fn ingest_without_pipeline_is_unavailable() {
    let dir = tempfile::tempdir().expect("temp dir");
    // `fixture_state` builds state without an ingest handle.
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/adapters/{PLUGIN_PLATFORM}/ingest"),
        Some(json!({ "channel_id": "c", "sender_id": "u", "text": "hi" })),
    )
    .await;

    assert_eq!(status, 503);
    assert_eq!(error_code(&body), "unavailable");
}

/// Minimal outbound request used by the delivery tests.
fn test_request() -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: BUILTIN_PLATFORM.to_string(),
        channel_id: "chan-9".to_string(),
        recipient_id: "alice".to_string(),
        segments: vec![MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: "hello".to_string(),
            })),
        }],
        event_id: "evt-test-request".to_string(),
    }
}

/// Built-in adapters are reachable from the pipeline through the registry.
#[tokio::test]
async fn builtin_adapter_delivery_is_recorded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _ingest_rx, adapter) = adapter_state(PathBuf::from(dir.path()), 8).await;

    let response = state
        .supervisor()
        .adapters()
        .get(BUILTIN_PLATFORM)
        .await
        .expect("adapter registered")
        .deliver(test_request())
        .await
        .expect("delivery succeeds");

    assert!(response.success);
    let deliveries: Vec<DeliverMessageRequest> = RecordingAdapter::deliveries(&adapter);
    assert_eq!(deliveries.len(), 1);
}

/// Conversational messages ingested via adapter route through the LLM pipeline and deliver to the adapter.
#[tokio::test]
async fn pipeline_llm_conversational_turn_routes_to_outbound_delivery() {
    let dir = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(PathBuf::from(dir.path())),
        None,
    ));

    let adapter = Arc::new(RecordingAdapter::new());
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .expect("built-in adapter registers");

    let (ingest_tx, ingest_rx) = tokio::sync::mpsc::channel(16);
    let mock_reply = "LLM conversational reply from kanon-core";
    let mock_provider = Arc::new(common::MockProvider::new(mock_reply));

    let state = kanon_api::ApiState::builder(supervisor.clone())
        .with_config_dir(PathBuf::from(dir.path()))
        .with_ingress(kanon_core::EventIngress::new(ingest_tx))
        .with_llm_provider(
            "kanon-core",
            mock_provider,
            kanon_api::default_agent_config("mock-model"),
        )
        .build();

    let agent = state.agent().expect("agent configured").clone();
    let engine = Arc::new(
        kanon_core::pipeline::PipelineEngine::new(supervisor.clone())
            .with_tool_router(Arc::new(kanon_llm::ToolRouter::from_arc(agent))),
    );
    let worker_handle = engine.clone().start_worker(ingest_rx);
    let dispatcher_handle = engine
        .clone()
        .start_outbound_dispatcher()
        .expect("dispatcher starts");

    let app: Router = app(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/adapters/{BUILTIN_PLATFORM}/ingest"),
        Some(json!({
            "channel_id": "chan-llm-1",
            "sender_id": "user-42",
            "text": "Hello, how are you?",
            "event_id": "evt-llm-1",
        })),
    )
    .await;

    assert_eq!(status, 202);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["event_id"], "evt-llm-1");

    // Wait for the pipeline worker to process the turn and dispatch the reply to the adapter
    let start = std::time::Instant::now();
    let mut deliveries = Vec::new();
    while start.elapsed() < Duration::from_secs(3) {
        deliveries = adapter.deliveries();
        if !deliveries.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        deliveries.len(),
        1,
        "expected 1 outbound delivery to adapter"
    );
    let delivery = &deliveries[0];
    assert_eq!(delivery.platform, BUILTIN_PLATFORM);
    assert_eq!(delivery.channel_id, "chan-llm-1");
    assert_eq!(delivery.recipient_id, "user-42");
    assert_eq!(delivery.event_id, "evt-llm-1");
    assert_eq!(delivery.segments.len(), 1);

    match &delivery.segments[0].segment {
        Some(Segment::Text(TextSegment { content })) => {
            assert_eq!(content, mock_reply);
        }
        other => panic!("expected Text segment, got {other:?}"),
    }

    worker_handle.abort();
    dispatcher_handle.abort();
}
