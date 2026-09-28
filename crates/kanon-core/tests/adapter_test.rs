//! Platform adapter contract coverage: registration, routing, delivery and ingest semantics.
//!
//! The plugin half of the contract is verified against a real gRPC host served in-process, so the
//! exact `OnDeliverMessage` round trip the core performs is exercised rather than simulated.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kanon_core::adapter::{AdapterError, AdapterKind, EventIngress, IngestError};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{ManagedHost, PluginManifest};
use kanon_proto::v1::message_pipeline_service_server::{
    MessagePipelineService, MessagePipelineServiceServer,
};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DeliverMessageRequest, DeliverMessageResponse,
    EventAck, EventNotification, IngestEventRequest, MessageSegment, PipelineEventRequest,
    PluginMeta, PreFilterResult, TextSegment, ToolCallRequest, ToolCallResponse,
};
use kanon_transport::connect_ipc;
use tempfile::tempdir;
use tokio::sync::{Mutex, mpsc};

use common::ChannelAdapter;

/// Manifest declaring a platform adapter, as a plugin author would write it.
const ADAPTER_MANIFEST: &str = r#"
[plugin]
id = "org.kanon.plugin.adapter_fixture"
name = "Adapter Fixture"
version = "1.0.0"
runtime = "rust"
entrypoint = "target/debug/adapter_fixture"

[adapter]
platform = "fixture_platform"
display_name = "Fixture Platform"
"#;

/// Mock plugin host that records the outbound messages it receives.
#[derive(Default)]
struct RecordingHost {
    deliveries: Mutex<Vec<DeliverMessageRequest>>,
    /// When true the host rejects every delivery, mimicking a platform-side failure.
    reject_delivery: bool,
}

#[tonic::async_trait]
impl MessagePipelineService for RecordingHost {
    async fn on_pre_filter(
        &self,
        _request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        Ok(tonic::Response::new(PreFilterResult {
            action: kanon_proto::v1::pre_filter_result::Action::Pass as i32,
            modified_text: String::new(),
            reply_messages: vec![],
        }))
    }

    async fn on_execute_command(
        &self,
        _request: tonic::Request<CommandExecuteRequest>,
    ) -> Result<tonic::Response<CommandExecuteResponse>, tonic::Status> {
        Ok(tonic::Response::new(CommandExecuteResponse {
            success: true,
            replies: vec![],
            error_message: String::new(),
        }))
    }

    async fn on_call_tool(
        &self,
        _request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("no tools"))
    }

    async fn on_event(
        &self,
        _request: tonic::Request<EventNotification>,
    ) -> Result<tonic::Response<EventAck>, tonic::Status> {
        Ok(tonic::Response::new(EventAck { received: true }))
    }

    async fn on_deliver_message(
        &self,
        request: tonic::Request<DeliverMessageRequest>,
    ) -> Result<tonic::Response<DeliverMessageResponse>, tonic::Status> {
        let req = request.into_inner();

        if self.reject_delivery {
            return Ok(tonic::Response::new(DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: "platform rejected the message".to_string(),
            }));
        }

        let message_id = format!("plugin-{}", req.channel_id);
        self.deliveries.lock().await.push(req);

        Ok(tonic::Response::new(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        }))
    }
}

/// Starts an in-process plugin host serving `MessagePipelineService` on a temporary socket.
async fn start_recording_host(socket_path: PathBuf, reject_delivery: bool) -> Arc<RecordingHost> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let service = Arc::new(RecordingHost {
        deliveries: Mutex::new(Vec::new()),
        reject_delivery,
    });

    let listener = kanon_transport::IpcListener::bind(&socket_path).expect("host socket binds");
    let incoming = listener.incoming();
    let served = service.clone();

    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(MessagePipelineServiceServer::new(RecordingHostServer {
                inner: served,
            }))
            .serve_with_incoming(incoming)
            .await;
    });

    service
}

/// Tonic adapter around [`RecordingHost`] (the trait impl itself owns the state).
struct RecordingHostServer {
    inner: Arc<RecordingHost>,
}

#[tonic::async_trait]
impl MessagePipelineService for RecordingHostServer {
    async fn on_pre_filter(
        &self,
        request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        self.inner.on_pre_filter(request).await
    }

    async fn on_execute_command(
        &self,
        request: tonic::Request<CommandExecuteRequest>,
    ) -> Result<tonic::Response<CommandExecuteResponse>, tonic::Status> {
        self.inner.on_execute_command(request).await
    }

    async fn on_call_tool(
        &self,
        request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        self.inner.on_call_tool(request).await
    }

    async fn on_event(
        &self,
        request: tonic::Request<EventNotification>,
    ) -> Result<tonic::Response<EventAck>, tonic::Status> {
        self.inner.on_event(request).await
    }

    async fn on_deliver_message(
        &self,
        request: tonic::Request<DeliverMessageRequest>,
    ) -> Result<tonic::Response<DeliverMessageResponse>, tonic::Status> {
        self.inner.on_deliver_message(request).await
    }
}

/// Builds an outbound request for the fixture platform.
fn outbound_request(platform: &str) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: platform.to_string(),
        channel_id: "chan-1".to_string(),
        recipient_id: "alice".to_string(),
        segments: vec![MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: "hello".to_string(),
            })),
        }],
        event_id: "evt-fixture-1".to_string(),
    }
}

/// Registers a plugin host whose manifest declares `fixture_platform`.
async fn register_adapter_host(
    supervisor: &Supervisor,
    run_dir: &std::path::Path,
) -> Arc<RecordingHost> {
    let socket_path = run_dir.join("host_fixture_adapter.sock");
    let recorded = start_recording_host(socket_path.clone(), false).await;

    let channel = connect_ipc(&socket_path)
        .await
        .expect("test host reachable");

    let manifest: PluginManifest = toml::from_str(ADAPTER_MANIFEST).expect("manifest parses");
    let host = Arc::new(
        ManagedHost::new(
            "org_kanon_plugin_adapter_fixture".to_string(),
            socket_path,
            channel,
            vec![PluginMeta {
                id: "org.kanon.plugin.adapter_fixture".to_string(),
                name: "Adapter Fixture".to_string(),
                version: "1.0.0".to_string(),
                ..PluginMeta::default()
            }],
            500,
        )
        .with_manifest(manifest),
    );

    supervisor.register_managed_host(host).await;
    recorded
}

/// Built-in adapters are registered, listed and resolved by platform.
#[tokio::test]
async fn builtin_adapter_registers_and_resolves() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));

    let (tx, _rx) = mpsc::channel(4);
    supervisor
        .adapters()
        .register(ChannelAdapter::shared("local_test", tx))
        .await
        .expect("registration succeeds");

    // Duplicate platform registration would make routing order-dependent, so it must fail.
    let (tx2, _rx2) = mpsc::channel(4);
    let error = supervisor
        .adapters()
        .register(ChannelAdapter::shared("local_test", tx2))
        .await
        .expect_err("duplicate platform rejected");
    assert!(matches!(error, AdapterError::DuplicatePlatform(p) if p == "local_test"));

    let route = supervisor
        .resolve_adapter("local_test")
        .await
        .expect("platform resolves");
    match route {
        kanon_core::AdapterRoute::Builtin(adapter) => {
            assert_eq!(adapter.platform(), "local_test");
            assert!(adapter.is_connected());
        }
        other => panic!("expected a built-in route, got {other:?}"),
    }

    let catalog = supervisor.adapter_catalog().await;
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].kind, AdapterKind::Builtin);
    assert_eq!(catalog[0].display_name, "Test channel adapter");
    assert!(catalog[0].host_id.is_none());
}

/// Unrouted platforms are reported explicitly instead of silently dropped.
#[tokio::test]
async fn unknown_platform_is_reported() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));
    let engine = PipelineEngine::new(supervisor);

    let error = engine
        .deliver_outbound(outbound_request("nowhere"))
        .await
        .expect_err("unknown platforms must fail");
    assert!(matches!(error, AdapterError::UnknownPlatform(p) if p == "nowhere"));
}

/// Outbound messages reach a built-in adapter through the engine.
#[tokio::test]
async fn outbound_is_routed_to_builtin_adapter() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));

    let (tx, mut rx) = mpsc::channel(4);
    supervisor
        .adapters()
        .register(ChannelAdapter::shared("local_test", tx))
        .await
        .expect("registration succeeds");

    let engine = PipelineEngine::new(supervisor);
    let outcome = engine
        .deliver_outbound(outbound_request("local_test"))
        .await
        .expect("delivery succeeds");

    assert_eq!(outcome.kind, AdapterKind::Builtin);
    assert_eq!(outcome.platform, "local_test");
    assert_eq!(outcome.message_id, "test-msg-chan-1");

    let delivered = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("delivery observed")
        .expect("channel open");
    assert_eq!(delivered.recipient_id, "alice");
    assert_eq!(delivered.segments.len(), 1);
}

/// Plugin-declared platforms are discovered from the manifest and delivered over gRPC.
#[tokio::test]
async fn outbound_is_routed_to_plugin_adapter_host() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));
    let recorded = register_adapter_host(&supervisor, run_dir.path()).await;

    // Discovery derives strictly from the manifest declaration.
    let catalog = supervisor.adapter_catalog().await;
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].platform, "fixture_platform");
    assert_eq!(catalog[0].kind, AdapterKind::Plugin);
    assert_eq!(catalog[0].display_name, "Fixture Platform");
    assert_eq!(
        catalog[0].host_id.as_deref(),
        Some("org_kanon_plugin_adapter_fixture")
    );
    assert_eq!(
        catalog[0].plugin_id.as_deref(),
        Some("org.kanon.plugin.adapter_fixture")
    );

    let engine = PipelineEngine::new(supervisor);
    let outcome = engine
        .deliver_outbound(outbound_request("fixture_platform"))
        .await
        .expect("plugin delivery succeeds");

    assert_eq!(outcome.kind, AdapterKind::Plugin);
    assert_eq!(outcome.platform, "fixture_platform");
    assert_eq!(outcome.message_id, "plugin-chan-1");

    let deliveries = recorded.deliveries.lock().await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].channel_id, "chan-1");
    assert_eq!(deliveries[0].recipient_id, "alice");
}

/// A plugin that refuses the message turns into an explicit delivery error.
#[tokio::test]
async fn plugin_rejection_becomes_adapter_error() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));

    let socket_path = run_dir.path().join("host_fixture_adapter.sock");
    start_recording_host(socket_path.clone(), true).await;
    let channel = connect_ipc(&socket_path)
        .await
        .expect("test host reachable");

    let manifest: PluginManifest = toml::from_str(ADAPTER_MANIFEST).expect("manifest parses");
    supervisor
        .register_managed_host(Arc::new(
            ManagedHost::new(
                "org_kanon_plugin_adapter_fixture".to_string(),
                socket_path,
                channel,
                vec![PluginMeta {
                    id: "org.kanon.plugin.adapter_fixture".to_string(),
                    ..PluginMeta::default()
                }],
                500,
            )
            .with_manifest(manifest),
        ))
        .await;

    let engine = PipelineEngine::new(supervisor);
    let error = engine
        .deliver_outbound(outbound_request("fixture_platform"))
        .await
        .expect_err("plugin rejection must surface");
    assert!(
        matches!(error, AdapterError::Delivery { ref reason, .. } if reason.contains("platform rejected the message")),
        "unexpected error: {error}"
    );
}

/// The ingest handle enforces Fast-ACK semantics: accepted, saturated, or closed.
#[tokio::test]
async fn ingest_handle_reports_backpressure_and_closure() {
    // Capacity 1: the second enqueue must report saturation instead of blocking.
    let (tx, mut rx) = mpsc::channel(1);
    let ingress = EventIngress::new(tx);
    assert_eq!(ingress.capacity(), 1);

    let event = |id: &str| IngestEventRequest {
        platform: "local_test".to_string(),
        event: Some(PipelineEventRequest {
            event_id: id.to_string(),
            platform: "local_test".to_string(),
            channel_id: "chan-1".to_string(),
            sender_id: "alice".to_string(),
            raw_text: "hello".to_string(),
            segments: vec![],
            metadata: None,
        }),
    };

    assert!(ingress.try_ingest(event("evt-1")).is_ok());
    assert_eq!(
        ingress.try_ingest(event("evt-2")),
        Err(IngestError::QueueFull)
    );

    // Draining frees capacity again.
    let drained = rx.recv().await.expect("event received");
    assert_eq!(drained.event.expect("payload").event_id, "evt-1");
    assert!(ingress.try_ingest(event("evt-3")).is_ok());

    // Closing the consumer surfaces as an explicit, non-retryable state.
    drop(rx);
    assert!(ingress.is_closed());
    assert_eq!(ingress.try_ingest(event("evt-4")), Err(IngestError::Closed));
    assert_eq!(
        ingress.ingest(event("evt-5")).await,
        Err(IngestError::Closed)
    );
}

/// Helper adapter to verify partitioned dispatch concurrency and sequencing.
struct InstrumentedAdapter {
    platform: String,
    delay: Duration,
    delivered_order: Arc<Mutex<Vec<String>>>,
}

#[tonic::async_trait]
impl kanon_core::PlatformAdapter for InstrumentedAdapter {
    fn platform(&self) -> &str {
        &self.platform
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.delivered_order
            .lock()
            .await
            .push(format!("{}:{}", self.platform, request.channel_id));
        Ok(DeliverMessageResponse {
            success: true,
            message_id: format!("msg-{}", request.channel_id),
            error_message: String::new(),
        })
    }
}

/// Outbound dispatch is partitioned per platform: a stalled platform never blocks deliveries to
/// other platforms, while intra-platform FIFO delivery order is strictly preserved.
#[tokio::test]
async fn test_partitioned_outbound_dispatch_cross_platform_isolation_and_fifo_order() {
    let supervisor = Arc::new(Supervisor::new(None, None));
    let engine = Arc::new(PipelineEngine::new(supervisor.clone()));
    let delivered = Arc::new(Mutex::new(Vec::new()));

    // "slow" platform takes 80ms per delivery; "fast" platform delivers immediately (0ms).
    let slow_adapter = Arc::new(InstrumentedAdapter {
        platform: "slow_plat".to_string(),
        delay: Duration::from_millis(80),
        delivered_order: delivered.clone(),
    });
    let fast_adapter = Arc::new(InstrumentedAdapter {
        platform: "fast_plat".to_string(),
        delay: Duration::ZERO,
        delivered_order: delivered.clone(),
    });

    supervisor
        .adapters()
        .register(slow_adapter)
        .await
        .expect("register slow adapter");
    supervisor
        .adapters()
        .register(fast_adapter)
        .await
        .expect("register fast adapter");

    let _dispatcher = engine.clone().start_outbound_dispatcher();
    let sender = engine.outbound_sender();

    let make_req = |plat: &str, chan: &str| DeliverMessageRequest {
        platform: plat.to_string(),
        channel_id: chan.to_string(),
        recipient_id: "user1".to_string(),
        segments: vec![],
        event_id: format!("evt-{plat}-{chan}"),
    };

    // Send 1 to slow, then 2 and 3 to fast
    sender
        .send(make_req("slow_plat", "c1").into())
        .await
        .expect("send slow 1");
    sender
        .send(make_req("fast_plat", "f1").into())
        .await
        .expect("send fast 1");
    sender
        .send(make_req("fast_plat", "f2").into())
        .await
        .expect("send fast 2");

    // After 25ms, "fast" platform should have already delivered both f1 and f2 in FIFO order,
    // while "slow" platform is still sleeping on its 80ms delay!
    tokio::time::sleep(Duration::from_millis(25)).await;

    {
        let log = delivered.lock().await;
        assert_eq!(
            *log,
            vec!["fast_plat:f1".to_string(), "fast_plat:f2".to_string()],
            "fast platform messages must be delivered concurrently without waiting for slow platform"
        );
    }

    // After 100ms, the slow message finishes too
    tokio::time::sleep(Duration::from_millis(80)).await;

    {
        let log = delivered.lock().await;
        assert_eq!(
            *log,
            vec![
                "fast_plat:f1".to_string(),
                "fast_plat:f2".to_string(),
                "slow_plat:c1".to_string()
            ],
            "all messages delivered, maintaining per-platform FIFO order and cross-platform concurrency"
        );
    }
}

/// Helper observer that captures outbound failure stages.
struct FailureObserver {
    failures: Arc<Mutex<Vec<String>>>,
}

impl kanon_core::PipelineObserver for FailureObserver {
    fn on_stage(&self, stage: &kanon_core::PipelineStage) {
        if let kanon_core::PipelineStage::OutboundFailed {
            platform, reason, ..
        } = stage
        {
            let mut list = self.failures.try_lock().expect("lock failure list");
            list.push(format!("{platform}:{reason}"));
        }
    }
}

/// Saturated platform queue drops excess messages and emits OutboundFailed without blocking.
#[tokio::test]
async fn test_partitioned_outbound_dispatch_queue_saturation_drop() {
    let supervisor = Arc::new(Supervisor::new(None, None));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let observer = Arc::new(FailureObserver {
        failures: failures.clone(),
    });

    let engine = Arc::new(PipelineEngine::new(supervisor.clone()).with_observer(observer));
    let delivered = Arc::new(Mutex::new(Vec::new()));

    // Platform blocks for 500ms so its 64-capacity queue will overflow quickly
    let blocked_adapter = Arc::new(InstrumentedAdapter {
        platform: "blocked_plat".to_string(),
        delay: Duration::from_millis(500),
        delivered_order: delivered.clone(),
    });

    supervisor
        .adapters()
        .register(blocked_adapter)
        .await
        .expect("register blocked adapter");

    let _dispatcher = engine.clone().start_outbound_dispatcher();
    let sender = engine.outbound_sender();

    // Send 80 messages: 1 is in-flight, 64 fit into the queue, and remaining ~15 must drop
    for i in 0..80 {
        sender
            .send(
                DeliverMessageRequest {
                    platform: "blocked_plat".to_string(),
                    channel_id: format!("c-{i}"),
                    recipient_id: "u1".to_string(),
                    segments: vec![],
                    event_id: format!("evt-drop-{i}"),
                }
                .into(),
            )
            .await
            .expect("send to global outbound queue");
    }

    // Wait a brief moment for the dispatcher to drain into the platform worker
    tokio::time::sleep(Duration::from_millis(50)).await;

    let drops = failures.lock().await;
    assert!(
        !drops.is_empty(),
        "excess messages beyond platform queue capacity must be dropped"
    );
    assert!(
        drops[0].contains("platform outbound queue full"),
        "failure stage reason must indicate platform queue saturation: {}",
        drops[0]
    );
}

/// A platform adapter whose delivery fails repeatedly.
struct FailingPlatformAdapter {
    platform: String,
    delivery_count: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl kanon_core::PlatformAdapter for FailingPlatformAdapter {
    fn platform(&self) -> &str {
        &self.platform
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        self.delivery_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(AdapterError::Delivery {
            platform: self.platform.clone(),
            reason: "connection refused: platform API endpoint offline".to_string(),
        })
    }
}

/// A platform whose deliveries fail trips its circuit breaker to Open and flushes undeliverable
/// messages into partitioned cold storage dead-letter JSONL files.
#[tokio::test]
async fn platform_circuit_breaker_trips_and_persists_to_dead_letter() {
    use kanon_core::CircuitState;
    use kanon_core::pipeline::dead_letter::DeadLetterWriter;

    let tmp = tempdir().expect("temp dir");
    let dlq_dir = tmp.path().join("dead_letter");

    let supervisor = Arc::new(Supervisor::new(Some(tmp.path().to_path_buf()), None));
    let failing_adapter = Arc::new(FailingPlatformAdapter {
        platform: "unstable_im".to_string(),
        delivery_count: std::sync::atomic::AtomicUsize::new(0),
    });
    supervisor
        .adapters()
        .register(failing_adapter.clone())
        .await
        .expect("register adapter");

    let failures = Arc::new(Mutex::new(Vec::new()));
    let observer = FailureObserver {
        failures: failures.clone(),
    };

    let dead_letter_writer = Arc::new(DeadLetterWriter::new(dlq_dir.clone()));
    let engine = Arc::new(
        PipelineEngine::new(supervisor.clone())
            .with_observer(Arc::new(observer))
            .with_dead_letter(dead_letter_writer),
    );

    let _dispatcher = engine.clone().start_outbound_dispatcher();
    let sender = engine.outbound_sender();

    // 1. Initial circuit state must be Closed
    assert_eq!(
        engine.platform_circuit_state("unstable_im").await,
        CircuitState::Closed
    );

    // 2. Send 5 messages: all 5 fail in the adapter, reaching the failure_threshold (5)
    for i in 1..=5 {
        sender
            .send(
                DeliverMessageRequest {
                    platform: "unstable_im".to_string(),
                    channel_id: format!("chan-{i}"),
                    recipient_id: "user1".to_string(),
                    segments: vec![MessageSegment {
                        segment: Some(Segment::Text(TextSegment {
                            content: format!("msg {i}"),
                        })),
                    }],
                    event_id: format!("evt-fail-{i}"),
                }
                .into(),
            )
            .await
            .expect("enqueue message");
    }

    // Wait until the sequential worker processes all 5 deliveries
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(15)).await;
        if failing_adapter
            .delivery_count
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 5
        {
            break;
        }
    }
    assert_eq!(
        failing_adapter
            .delivery_count
            .load(std::sync::atomic::Ordering::SeqCst),
        5
    );

    // 3. Circuit breaker must now be tripped to Open
    assert_eq!(
        engine.platform_circuit_state("unstable_im").await,
        CircuitState::Open
    );

    // Verify adapter catalog exposes the Open circuit state to the control plane
    let catalog = engine.adapter_catalog().await;
    let desc = catalog
        .iter()
        .find(|d| d.platform == "unstable_im")
        .expect("adapter in catalog");
    assert_eq!(desc.circuit_state, CircuitState::Open);

    // 4. Send a 6th message while circuit is Open: must be fast-skipped WITHOUT calling deliver()
    sender
        .send(
            DeliverMessageRequest {
                platform: "unstable_im".to_string(),
                channel_id: "chan-short-circuit".to_string(),
                recipient_id: "user1".to_string(),
                segments: vec![MessageSegment {
                    segment: Some(Segment::Text(TextSegment {
                        content: "short circuited".to_string(),
                    })),
                }],
                event_id: "evt-short-circuit-6".to_string(),
            }
            .into(),
        )
        .await
        .expect("enqueue message");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // The adapter's deliver() was NOT called a 6th time
    assert_eq!(
        failing_adapter
            .delivery_count
            .load(std::sync::atomic::Ordering::SeqCst),
        5
    );

    // 5. Inspect the dead-letter cold storage files
    assert!(dlq_dir.exists(), "DLQ directory must be created");
    let mut files = std::fs::read_dir(&dlq_dir).expect("read dlq dir");
    let entry = files
        .next()
        .expect("at least one dlq file")
        .expect("file entry");
    let file_name = entry.file_name().to_string_lossy().to_string();
    assert!(
        file_name.starts_with("unstable_im_"),
        "Filename must be partitioned by platform: {file_name}"
    );
    assert!(
        file_name.ends_with(".jsonl"),
        "Filename must end in .jsonl: {file_name}"
    );

    let content = std::fs::read_to_string(entry.path()).expect("read dlq file");
    let lines: Vec<&str> = content
        .trim()
        .split('\n')
        .filter(|s| !s.is_empty())
        .collect();
    // 5 failed deliveries + 1 short-circuited delivery = 6 dead-letter records
    assert_eq!(lines.len(), 6);

    // Validate structured JSON of the first record (failed delivery)
    let record1: serde_json::Value = serde_json::from_str(lines[0]).expect("parse JSON");
    assert_eq!(record1["platform"], "unstable_im");
    assert_eq!(record1["channel_id"], "chan-1");
    assert_eq!(record1["event_id"], "evt-fail-1");
    assert!(
        record1["reason"]
            .as_str()
            .unwrap()
            .contains("connection refused")
    );
    assert!(record1["timestamp_ms"].as_u64().unwrap() > 0);
    assert!(record1["iso_time"].as_str().unwrap().contains("T"));
    assert_eq!(record1["segments"][0]["content"], "msg 1");

    // Validate structured JSON of the last record (circuit breaker open)
    let record6: serde_json::Value = serde_json::from_str(lines[5]).expect("parse JSON");
    assert_eq!(record6["event_id"], "evt-short-circuit-6");
    assert_eq!(record6["reason"], "platform circuit breaker open");
}
