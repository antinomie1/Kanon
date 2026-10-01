//! End-to-end integration test verifying the IPC loop between Kanon Core
//! and the TypeScript plugin host (`sdks/typescript/dist/src/host/index.js`).

use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::supervisor::Supervisor;
use kanon_proto::prost_types;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, PipelineEventRequest, ToolCallRequest, pre_filter_result::Action,
};

fn find_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[tokio::test]
async fn test_typescript_plugin_lifecycle_and_pipeline() {
    let root = find_workspace_root();
    let manifest_path = root.join("sdks/typescript/plugins/demo_ts_plugin/plugin.toml");
    assert!(
        manifest_path.exists(),
        "TypeScript plugin manifest must exist at: {}",
        manifest_path.display()
    );

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

    // 4. Spawn TypeScript demo plugin via Supervisor manifest resolution
    let managed_host = supervisor
        .spawn_from_manifest(&manifest_path, None)
        .await
        .expect("Failed to spawn TypeScript host and complete handshake");

    // 5. Verify GetPluginMeta handshake results
    assert_eq!(
        managed_host.metas().len(),
        1,
        "Expected exactly 1 plugin in metadata"
    );
    let meta = &managed_host.metas()[0].clone();
    assert_eq!(meta.id, "org.kanon.plugin.demo_ts");
    assert_eq!(meta.name, "Demo TypeScript Plugin");
    assert_eq!(meta.version, "0.1.0");

    let cmd_meta = meta.commands.iter().find(|c| c.name == "tsgreet");
    assert!(cmd_meta.is_some(), "Expected tsgreet command in metadata");
    assert_eq!(cmd_meta.unwrap().usage, "/tsgreet <name>");

    let tool_meta = meta.tools.iter().find(|t| t.name == "ts_calc");
    assert!(tool_meta.is_some(), "Expected ts_calc tool in metadata");

    // 6. Test MessagePipelineService::OnExecuteCommand (/tsgreet Alice)
    let cmd_request = CommandExecuteRequest {
        plugin_id: "org.kanon.plugin.demo_ts".to_string(),
        command: "tsgreet".to_string(),
        args: vec!["Alice".to_string()],
        context: None,
        ..Default::default()
    };
    let cmd_response = managed_host
        .execute_command(cmd_request)
        .await
        .expect("OnExecuteCommand RPC failed");

    assert!(cmd_response.success, "Command execution must succeed");
    assert_eq!(cmd_response.replies.len(), 1);
    if let Some(Segment::Text(text_seg)) = &cmd_response.replies[0].segment {
        assert!(
            text_seg
                .content
                .contains("Hello from Kanon TypeScript plugin, Alice!"),
            "Unexpected reply: {}",
            text_seg.content
        );
    } else {
        panic!("Expected text segment in command reply");
    }

    // 7. Test MessagePipelineService::OnPreFilter (Normal event -> Pass)
    let normal_event = PipelineEventRequest {
        event_id: "evt_ts_1".to_string(),
        platform: "test".to_string(),
        channel_id: "chan_1".to_string(),
        sender_id: "user_ts".to_string(),
        raw_text: "hello from typescript test".to_string(),
        segments: vec![],
        metadata: None,
    };
    let pre_filter_resp = managed_host
        .pre_filter(normal_event)
        .await
        .expect("OnPreFilter RPC failed");
    assert_eq!(pre_filter_resp.action, Action::Pass as i32);

    // 8. Test MessagePipelineService::OnPreFilter (Block condition)
    let block_event = PipelineEventRequest {
        event_id: "evt_ts_2".to_string(),
        platform: "test".to_string(),
        channel_id: "chan_1".to_string(),
        sender_id: "user_ts".to_string(),
        raw_text: "test message with [block]".to_string(),
        segments: vec![],
        metadata: None,
    };
    let block_resp = managed_host
        .pre_filter(block_event)
        .await
        .expect("OnPreFilter RPC failed for blocking");
    assert_eq!(block_resp.action, Action::Block as i32);
    assert_eq!(block_resp.reply_messages.len(), 1);

    // 9. Test MessagePipelineService::OnCallTool (ts_calc)
    let tool_request = ToolCallRequest {
        call_id: "call_ts_101".to_string(),
        tool_name: "ts_calc".to_string(),
        session_id: "session_1".to_string(),
        payload: None,
    };
    let tool_response = managed_host
        .on_call_tool(tool_request)
        .await
        .expect("OnCallTool RPC failed");
    assert!(tool_response.success, "Tool execution must succeed");
    assert_eq!(tool_response.call_id, "call_ts_101");
    match tool_response.payload {
        Some(kanon_proto::v1::tool_call_response::Payload::StructuredResult(s)) => {
            let val = s.fields.get("result").expect("expected 'result' field");
            assert_eq!(
                val.kind,
                Some(prost_types::value::Kind::NumberValue(42.0)),
                "Tool result number must match 42.0"
            );
        }
        other => panic!("Expected StructuredResult payload, got: {:?}", other),
    }

    // 10. Graceful shutdown and socket cleanup verification
    supervisor
        .stop_host(&managed_host.host_id)
        .await
        .expect("Failed to stop typescript host");

    assert!(
        !managed_host.socket_path.exists(),
        "TypeScript host socket file must be removed after host stop"
    );

    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
