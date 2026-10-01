//! End-to-end integration test verifying the IPC loop between Kanon Core
//! and an out-of-process Rust plugin host (`demo-rust-plugin`).

use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::supervisor::Supervisor;
use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{CommandExecuteRequest, IngestEventRequest, PipelineEventRequest};
use kanon_transport::connect_ipc;

/// Locates the compiled `demo-rust-plugin` binary in the target directory.
fn find_demo_plugin_bin() -> PathBuf {
    // Standard target directory path relative to crates/kanon-core
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
async fn test_core_plugin_ipc_handshake_and_pipeline() {
    // 1. Set up isolated temporary runtime directory for test sockets
    let temp_dir = tempdir().expect("Failed to create temporary directory");
    let run_dir = temp_dir.path().to_path_buf();
    let core_sock = run_dir.join("core.sock");

    // 2. Start Core IPC Server on core.sock
    let (event_tx, mut event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
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

    // Wait briefly for Core IPC Server to bind and listen
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        core_sock.exists(),
        "Core socket file must exist after server startup"
    );

    // 3. Initialize Supervisor with the isolated runtime directory
    let supervisor = Supervisor::new(Some(run_dir.clone()), Some(core_sock.clone()));

    // 4. Spawn demo_rust_plugin sub-process through Supervisor
    let plugin_bin = find_demo_plugin_bin();
    let managed_host = supervisor
        .spawn_plugin("demo_rust", &plugin_bin, &[])
        .await
        .expect("Failed to spawn plugin and complete handshake");

    // 5. Verify GetPluginMeta handshake results
    assert_eq!(
        managed_host.metas().len(),
        1,
        "Expected exactly 1 plugin in metadata"
    );
    let meta = &managed_host.metas()[0].clone();
    assert_eq!(meta.id, "org.kanon.plugin.demo_rust");
    assert_eq!(meta.name, "Demo Rust Plugin");
    assert_eq!(meta.version, "0.1.0");
    assert_eq!(meta.commands.len(), 1);
    assert_eq!(meta.commands[0].name, "rustcalc");
    assert_eq!(meta.commands[0].usage, "/rustcalc <expr>");

    // 6. Test MessagePipelineService::OnExecuteCommand via IPC
    let cmd_request = CommandExecuteRequest {
        plugin_id: "org.kanon.plugin.demo_rust".to_string(),
        command: "rustcalc".to_string(),
        args: vec!["2 + 2".to_string()],
        context: None,
        ..Default::default()
    };
    let cmd_response = managed_host
        .execute_command(cmd_request)
        .await
        .expect("OnExecuteCommand RPC failed");

    assert!(cmd_response.success, "Command execution must succeed");
    assert_eq!(
        cmd_response.replies.len(),
        1,
        "Command must return 1 reply segment"
    );

    if let Some(Segment::Text(text_seg)) = &cmd_response.replies[0].segment {
        assert!(
            text_seg
                .content
                .contains("Rust calculation result for [2 + 2]: 42"),
            "Unexpected command reply content: {}",
            text_seg.content
        );
    } else {
        panic!("Expected text segment in command reply");
    }

    // 7. Test MessagePipelineService::OnPreFilter via IPC
    let pre_filter_req = PipelineEventRequest {
        event_id: "evt_100".to_string(),
        platform: "test".to_string(),
        channel_id: "chan_1".to_string(),
        sender_id: "user_1".to_string(),
        raw_text: "hello kanon".to_string(),
        segments: vec![],
        metadata: None,
    };
    let pre_filter_resp = managed_host
        .pre_filter(pre_filter_req)
        .await
        .expect("OnPreFilter RPC failed");
    assert_eq!(
        pre_filter_resp.action,
        kanon_proto::v1::pre_filter_result::Action::Pass as i32
    );

    // 8. Test Core BotApiService::IngestEvent Fast-ACK over core.sock
    let core_channel = connect_ipc(&core_sock)
        .await
        .expect("Failed to connect to core.sock");
    let mut core_client = BotApiServiceClient::new(core_channel);

    let ingest_req = IngestEventRequest {
        platform: "test".to_string(),
        event: Some(PipelineEventRequest {
            event_id: "evt_fast_ack_1".to_string(),
            platform: "test".to_string(),
            channel_id: "chan_1".to_string(),
            sender_id: "user_test".to_string(),
            raw_text: "fast-ack test".to_string(),
            segments: vec![],
            metadata: None,
        }),
    };
    let ingest_resp = core_client
        .ingest_event(ingest_req)
        .await
        .expect("IngestEvent RPC failed")
        .into_inner();

    assert!(ingest_resp.accepted, "Event must be accepted by Core");
    assert_eq!(ingest_resp.event_id, "evt_fast_ack_1");

    // Verify that event was placed onto the asynchronous ingest queue
    let received_event = event_rx.recv().await.expect("Expected event on queue");
    assert_eq!(received_event.event.unwrap().event_id, "evt_fast_ack_1");

    // 9. Graceful shutdown and cleanup verification
    supervisor
        .stop_host("demo_rust")
        .await
        .expect("Failed to stop demo_rust host");

    let host_sock = managed_host.socket_path.clone();
    assert!(
        !host_sock.exists(),
        "Host socket file must be removed after host stop"
    );

    // Shut down Core IPC Server
    let _ = shutdown_tx.send(());
    let _ = server_task.await;

    assert!(
        !core_sock.exists(),
        "Core socket file must be removed after server shutdown"
    );
}
