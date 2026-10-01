//! End-to-end integration test verifying the Kanon Core message processing pipeline.
//!
//! Tests the complete lifecycle:
//! 1. Fast-ACK event ingestion via `BotApiService.IngestEvent` over `core.sock` into an MPSC queue;
//! 2. Asynchronous consumption by the `PipelineEngine` worker loop;
//! 3. Ordered evaluation of plugin pre-filters (allowing pass or triggering block);
//! 4. Slash command matching against the GetPluginMeta registry;
//! 5. Execution on the target plugin host (`demo-rust-plugin`);
//! 6. Outbound message delivery to the adapter channel.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, IngestEventRequest, MessageSegment, PipelineEventRequest, TextSegment,
};
use kanon_transport::connect_ipc;

/// Locates the compiled `demo-rust-plugin` binary in the target directory.
fn find_demo_plugin_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root");

    let debug_bin = root_dir.join("target/debug/demo-rust-plugin");
    if debug_bin.exists() {
        return debug_bin;
    }

    panic!(
        "Demo plugin executable not found at '{}'. Ensure 'cargo build -p demo-rust-plugin' has run.",
        debug_bin.display()
    );
}

#[tokio::test]
async fn test_pipeline_router_end_to_end_lifecycle() {
    // 1. Create an isolated runtime directory for IPC sockets.
    let temp_dir = tempdir().expect("Failed to create temporary directory");
    let run_dir = temp_dir.path().to_path_buf();
    let core_sock = run_dir.join("core.sock");

    // 2. Start Core IPC Server hosting BotApiService on core.sock.
    let (event_tx, event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx);
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("Core IPC Server failed");
    });

    // Allow the server a moment to bind the socket.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(core_sock.exists(), "Core socket file must exist");

    // 3. Initialize Supervisor and spawn out-of-process demo-rust-plugin.
    let supervisor = Arc::new(Supervisor::new(
        Some(run_dir.clone()),
        Some(core_sock.clone()),
    ));
    let plugin_bin = find_demo_plugin_bin();
    let managed_host = supervisor
        .spawn_plugin("demo_rust", &plugin_bin, &[])
        .await
        .expect("Failed to spawn demo_rust plugin host");

    // Verify metadata handshake registered the /rustcalc command.
    assert_eq!(managed_host.metas().len(), 1);
    assert_eq!(managed_host.metas()[0].commands[0].name, "rustcalc");

    // 4. Register a built-in adapter for the fixture platform, then start the pipeline worker
    //    and the outbound dispatcher that routes replies through the adapter registry.
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<DeliverMessageRequest>(100);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared("test_im", outbound_tx))
        .await
        .expect("adapter registration");

    let engine = Arc::new(PipelineEngine::new(supervisor.clone()));
    let worker_handle = engine.clone().start_worker(event_rx);
    let dispatcher_handle = engine
        .start_outbound_dispatcher()
        .expect("outbound dispatcher starts");

    // 5. Connect gRPC client to Core IPC socket.
    let core_channel = connect_ipc(&core_sock)
        .await
        .expect("Failed to connect to core.sock");
    let mut core_client = BotApiServiceClient::new(core_channel);

    // =========================================================================
    // Scenario 1: PreFilter Pass -> /rustcalc Command Execution -> Outbound Reply
    // =========================================================================
    let calc_event = IngestEventRequest {
        platform: "test_im".to_string(),
        event: Some(PipelineEventRequest {
            event_id: "evt_calc_1".to_string(),
            platform: "test_im".to_string(),
            channel_id: "chan_math".to_string(),
            sender_id: "alice".to_string(),
            raw_text: "/rustcalc 10 + 32".to_string(),
            segments: vec![MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "/rustcalc 10 + 32".to_string(),
                })),
            }],
            metadata: None,
        }),
    };

    let fast_ack_1 = core_client
        .ingest_event(calc_event)
        .await
        .expect("IngestEvent RPC failed")
        .into_inner();

    assert!(
        fast_ack_1.accepted,
        "Fast-ACK must immediately accept event"
    );
    assert_eq!(fast_ack_1.event_id, "evt_calc_1");

    // Collect outbound response produced by PipelineEngine
    let outbound_reply = tokio::time::timeout(Duration::from_secs(3), outbound_rx.recv())
        .await
        .expect("Timed out waiting for outbound reply from pipeline")
        .expect("Outbound channel unexpectedly closed");

    assert_eq!(outbound_reply.platform, "test_im");
    assert_eq!(outbound_reply.channel_id, "chan_math");
    assert_eq!(outbound_reply.recipient_id, "alice");
    assert_eq!(outbound_reply.segments.len(), 1);

    if let Some(Segment::Text(text)) = &outbound_reply.segments[0].segment {
        assert!(
            text.content
                .contains("Rust calculation result for [10 + 32]: 42 (fast-path)"),
            "Unexpected reply content: {}",
            text.content
        );
    } else {
        panic!("Expected text message segment in outbound reply");
    }

    // =========================================================================
    // Scenario 2: PreFilter Interception (Block) -> Short-Circuit -> Block Reply
    // =========================================================================
    let blocked_event = IngestEventRequest {
        platform: "test_im".to_string(),
        event: Some(PipelineEventRequest {
            event_id: "evt_blocked_2".to_string(),
            platform: "test_im".to_string(),
            channel_id: "chan_security".to_string(),
            sender_id: "bob".to_string(),
            raw_text: "[block] /rustcalc 1 + 1".to_string(),
            segments: vec![MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "[block] /rustcalc 1 + 1".to_string(),
                })),
            }],
            metadata: None,
        }),
    };

    let fast_ack_2 = core_client
        .ingest_event(blocked_event)
        .await
        .expect("IngestEvent RPC failed")
        .into_inner();

    assert!(fast_ack_2.accepted);

    let outbound_block_reply = tokio::time::timeout(Duration::from_secs(3), outbound_rx.recv())
        .await
        .expect("Timed out waiting for pre-filter block reply")
        .expect("Outbound channel unexpectedly closed");

    assert_eq!(outbound_block_reply.channel_id, "chan_security");
    assert_eq!(outbound_block_reply.recipient_id, "bob");
    assert_eq!(outbound_block_reply.segments.len(), 1);

    if let Some(Segment::Text(text)) = &outbound_block_reply.segments[0].segment {
        assert!(
            text.content
                .contains("blocked by Demo Rust Plugin pre-filter"),
            "Expected pre-filter block notice, got: {}",
            text.content
        );
    } else {
        panic!("Expected text segment in block reply");
    }

    // =========================================================================
    // Scenario 3: Conversational Message (No Command) -> Passes through without replies
    // =========================================================================
    let convo_event = IngestEventRequest {
        platform: "test_im".to_string(),
        event: Some(PipelineEventRequest {
            event_id: "evt_chat_3".to_string(),
            platform: "test_im".to_string(),
            channel_id: "chan_general".to_string(),
            sender_id: "charlie".to_string(),
            raw_text: "Good morning, everyone!".to_string(),
            segments: vec![],
            metadata: None,
        }),
    };

    let fast_ack_3 = core_client
        .ingest_event(convo_event)
        .await
        .expect("IngestEvent RPC failed")
        .into_inner();

    assert!(fast_ack_3.accepted);

    // Verify that plain conversational messages do not generate command replies.
    let unexpected = tokio::time::timeout(Duration::from_millis(200), outbound_rx.recv()).await;
    assert!(
        unexpected.is_err(),
        "Plain text conversation should not produce command delivery message"
    );

    // =========================================================================
    // Clean Shutdown and Resource Teardown
    // =========================================================================
    worker_handle.abort();
    dispatcher_handle.abort();
    supervisor
        .stop_host("demo_rust")
        .await
        .expect("Failed to terminate host process");

    let _ = shutdown_tx.send(());
    let _ = server_task.await;

    assert!(!core_sock.exists(), "Core socket must be removed on exit");
}
