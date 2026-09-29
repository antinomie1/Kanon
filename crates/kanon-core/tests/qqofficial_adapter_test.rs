//! Integration tests for QQ Official platform adapter plugin (`plugins/qqofficial`).

use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::manifest::{PluginManifest, PluginScanner};
use kanon_core::supervisor::Supervisor;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, MessageSegment, TextSegment};

fn find_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn test_qqofficial_manifest_and_scanner() {
    let root = find_workspace_root();
    let manifest_path = root.join("plugins/qqofficial/plugin.toml");
    assert!(
        manifest_path.exists(),
        "Manifest must exist at {}",
        manifest_path.display()
    );

    let manifest =
        PluginManifest::load_from_file(&manifest_path).expect("Manifest must parse cleanly");
    assert_eq!(manifest.plugin.id, "org.kanon.adapter.qqofficial");
    assert_eq!(manifest.plugin.name, "QQ Official Adapter");
    assert_eq!(manifest.plugin.runtime, "python");
    assert_eq!(manifest.plugin.entrypoint, "main.py");
    assert_eq!(manifest.plugin.isolated, Some(true));

    let adapter = manifest.adapter.expect("Adapter section must be defined");
    assert_eq!(adapter.platform, "qqofficial");
    assert_eq!(adapter.display_name.as_deref(), Some("QQ 官方机器人"));

    // Verify discovery via PluginScanner
    let discovered =
        PluginScanner::scan(root.join("plugins")).expect("Scanner must scan plugins dir");
    let qq_plugin = discovered
        .iter()
        .find(|p| p.manifest.plugin.id == "org.kanon.adapter.qqofficial");
    assert!(
        qq_plugin.is_some(),
        "PluginScanner must discover qqofficial"
    );
}

#[tokio::test]
async fn test_qqofficial_adapter_supervisor_lifecycle() {
    let root = find_workspace_root();
    let manifest_path = root.join("plugins/qqofficial/plugin.toml");

    // 1. Set up isolated temporary runtime directory for test sockets
    let temp_dir = tempdir().expect("Failed to create temporary directory");
    let run_dir = temp_dir.path().to_path_buf();
    let core_sock = run_dir.join("core.sock");

    // 2. Start Core IPC Server on core.sock
    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
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

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(core_sock.exists(), "Core socket must exist after startup");

    // 3. Initialize Supervisor with the isolated runtime directory
    let supervisor = Supervisor::new(Some(run_dir.clone()), Some(core_sock.clone()));

    // 4. Spawn QQ official adapter plugin via Supervisor
    let managed_host = supervisor
        .spawn_from_manifest(&manifest_path, None)
        .await
        .expect("Failed to spawn QQ Official host and complete handshake");

    // 5. Verify GetPluginMeta handshake and adapter status
    assert_eq!(managed_host.meta.len(), 1);
    assert_eq!(managed_host.meta[0].id, "org.kanon.adapter.qqofficial");
    assert_eq!(
        managed_host.adapter_platforms(),
        vec!["qqofficial".to_string()]
    );
    assert_eq!(
        managed_host.adapter_display_name().as_deref(),
        Some("QQ 官方机器人")
    );

    // 6. Test outbound delivery hook while QQ client is disconnected
    let deliver_req = DeliverMessageRequest {
        platform: "qqofficial".to_string(),
        channel_id: "group:mock_group_123".to_string(),
        recipient_id: "mock_user".to_string(),
        segments: vec![MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: "Hello from Kanon Core".to_string(),
            })),
        }],
        event_id: "msg_out_001".to_string(),
    };

    let deliver_resp = managed_host
        .deliver_message(deliver_req)
        .await
        .expect("deliver_message RPC must succeed");

    // The QQ client is disconnected in test mode (missing credentials), so success is false with explicit error
    assert!(
        !deliver_resp.success,
        "Delivery without active QQ client must report failure"
    );
    assert!(
        deliver_resp.error_message.contains("disconnected")
            || deliver_resp.error_message.contains("not running"),
        "Error message should explain client disconnection, got: {}",
        deliver_resp.error_message
    );

    // 7. Clean up
    supervisor
        .stop_host(&managed_host.host_id)
        .await
        .expect("Failed to stop QQ host");

    assert!(
        !managed_host.socket_path.exists(),
        "Host socket file must be cleaned up"
    );

    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
