//! Integration tests verifying the convergence pass improvements:
//! 1. Unified host registry between CoreApiService and Supervisor, including hosts that register
//!    while the supervisor is still launching them
//! 2. SendMessage routed into PipelineEngine outbound queue
//! 3. Explicit unimplemented status for storage gRPC endpoints

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{Mutex, mpsc, oneshot};
use tonic::Code;

use kanon_core::adapter::{AdapterError, PlatformAdapter};
use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::{Supervisor, SupervisorError};
use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::message_pipeline_service_server::{
    MessagePipelineService, MessagePipelineServiceServer,
};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DeliverMessageRequest, DeliverMessageResponse,
    EventAck, EventNotification, GetPluginMetaRequest, GetPluginMetaResponse, GetStorageRequest,
    MessageSegment, PingRequest, PingResponse, PipelineEventRequest, PluginActionRequest,
    PluginActionResponse, PluginMeta, PreFilterResult, RegisterHostRequest,
    ReloadPluginConfigRequest, ReloadPluginConfigResponse, SendMessageRequest, SetStorageRequest,
    TextSegment, ToolCallRequest, ToolCallResponse,
};
use kanon_transport::{IpcListener, connect_ipc, host_socket_path};

/// Mock platform adapter capturing outbound deliveries.
struct MockAdapter {
    platform: String,
    deliveries: Arc<Mutex<Vec<DeliverMessageRequest>>>,
}

#[tonic::async_trait]
impl PlatformAdapter for MockAdapter {
    fn platform(&self) -> &str {
        &self.platform
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        self.deliveries.lock().await.push(request);
        Ok(DeliverMessageResponse {
            success: true,
            message_id: "mock_delivered_id".to_string(),
            error_message: String::new(),
        })
    }
}

/// Minimal mock host providing PluginHostService and MessagePipelineService.
struct MockHostService;

#[tonic::async_trait]
impl PluginHostService for MockHostService {
    /// This fixture declares no management actions.
    async fn invoke_action(
        &self,
        _request: tonic::Request<PluginActionRequest>,
    ) -> Result<tonic::Response<PluginActionResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("fixture host has no actions"))
    }

    async fn ping(
        &self,
        request: tonic::Request<PingRequest>,
    ) -> Result<tonic::Response<PingResponse>, tonic::Status> {
        Ok(tonic::Response::new(PingResponse {
            timestamp: request.into_inner().timestamp,
        }))
    }

    async fn reload_plugin_config(
        &self,
        _request: tonic::Request<ReloadPluginConfigRequest>,
    ) -> Result<tonic::Response<ReloadPluginConfigResponse>, tonic::Status> {
        Ok(tonic::Response::new(ReloadPluginConfigResponse {
            success: true,
            error_message: String::new(),
            applied_version: 1,
        }))
    }

    async fn get_plugin_meta(
        &self,
        _request: tonic::Request<GetPluginMetaRequest>,
    ) -> Result<tonic::Response<GetPluginMetaResponse>, tonic::Status> {
        Ok(tonic::Response::new(GetPluginMetaResponse {
            plugins: vec![PluginMeta {
                id: "test.plugin".to_string(),
                name: "Test Plugin".to_string(),
                version: "1.0.0".to_string(),
                author: "Test".to_string(),
                description: "Test".to_string(),
                commands: vec![],
                tools: vec![],
                ..Default::default()
            }],
        }))
    }
}

#[tonic::async_trait]
impl MessagePipelineService for MockHostService {
    async fn on_llm_request(
        &self,
        _request: tonic::Request<kanon_proto::v1::LlmRequestHookRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::LlmRequestHookResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_llm_request"))
    }

    async fn on_http_request(
        &self,
        _request: tonic::Request<kanon_proto::v1::HttpRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::HttpResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_http_request"))
    }
    async fn on_pre_filter(
        &self,
        _request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        Ok(tonic::Response::new(PreFilterResult {
            action: 0,
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
            ..Default::default()
        }))
    }

    async fn on_call_tool(
        &self,
        _request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        Ok(tonic::Response::new(ToolCallResponse {
            call_id: "call_1".to_string(),
            success: true,
            error_message: String::new(),
            payload: None,
            attachments: Vec::new(),
        }))
    }

    async fn on_event(
        &self,
        _request: tonic::Request<EventNotification>,
    ) -> Result<tonic::Response<EventAck>, tonic::Status> {
        Ok(tonic::Response::new(EventAck { received: true }))
    }

    async fn on_decorate_reply(
        &self,
        _request: tonic::Request<kanon_proto::v1::DecorateReplyRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DecorateReplyResult>, tonic::Status> {
        Ok(tonic::Response::new(
            kanon_proto::v1::DecorateReplyResult::default(),
        ))
    }

    async fn on_prepare_turn(
        &self,
        _request: tonic::Request<kanon_proto::v1::PrepareTurnRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PrepareTurnResult>, tonic::Status> {
        Ok(tonic::Response::new(
            kanon_proto::v1::PrepareTurnResult::default(),
        ))
    }

    async fn on_deliver_message(
        &self,
        _request: tonic::Request<DeliverMessageRequest>,
    ) -> Result<tonic::Response<DeliverMessageResponse>, tonic::Status> {
        Ok(tonic::Response::new(DeliverMessageResponse {
            success: true,
            message_id: "host_msg_1".to_string(),
            error_message: String::new(),
        }))
    }
}

#[tokio::test]
async fn test_unified_host_registration_into_supervisor() {
    let tmp = tempdir().expect("tempdir");
    let core_sock = tmp.path().join("core.sock");
    let host_sock = tmp.path().join("host_external.sock");

    // 1. Start mock host server on host_sock
    let host_listener = IpcListener::bind(&host_sock).expect("bind host socket");
    let (host_shutdown_tx, host_shutdown_rx) = oneshot::channel();
    let host_task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(MockHostService))
            .add_service(MessagePipelineServiceServer::new(MockHostService))
            .serve_with_incoming_shutdown(host_listener.incoming(), async move {
                let _ = host_shutdown_rx.await;
            })
            .await
            .expect("Host server failed");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Start Core server wired with Supervisor
    let supervisor = Arc::new(Supervisor::new(
        Some(tmp.path().to_path_buf()),
        Some(core_sock.clone()),
    ));
    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx).with_supervisor(supervisor.clone());
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let core_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
            .expect("Core server failed");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Connect as plugin host and call RegisterHost
    let channel = connect_ipc(&core_sock).await.expect("connect to core");
    let mut client = BotApiServiceClient::new(channel);

    let reg_req = RegisterHostRequest {
        host_id: "ext_host_1".to_string(),
        runtime: "rust".to_string(),
        endpoint: host_sock.to_string_lossy().to_string(),
        loaded_plugin_ids: vec!["test.plugin".to_string()],
    };

    let response = client
        .register_host(reg_req)
        .await
        .expect("RegisterHost RPC succeeded")
        .into_inner();

    assert!(response.success, "Registration must succeed");

    // 4. Verify host is now in Supervisor's unified registry
    let managed_host = supervisor
        .get_host("ext_host_1")
        .await
        .expect("Host must exist in Supervisor unified registry");

    assert_eq!(managed_host.host_id, "ext_host_1");
    assert_eq!(managed_host.metas().len(), 1);
    assert_eq!(managed_host.metas()[0].id, "test.plugin");

    // Clean up
    let _ = host_shutdown_tx.send(());
    let _ = core_shutdown_tx.send(());
    let _ = host_task.await;
    let _ = core_task.await;
}

/// Waits until `path` exists, failing the test after five seconds.
async fn wait_for_path(path: &Path) {
    for _ in 0..500 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} never appeared", path.display());
}

/// A launched host registers before it serves (the Rust SDK registers before it even binds its
/// socket, because plugins need the core handle while loading). The core used to dial the
/// missing socket back and log "Transport error connecting to host"; it must acknowledge the
/// registration and leave the handshake to the launch.
#[cfg(unix)]
#[tokio::test]
async fn a_host_registering_during_its_own_launch_is_acknowledged() {
    let tmp = tempdir().expect("tempdir");
    let core_sock = tmp.path().join("core.sock");
    let supervisor = Arc::new(Supervisor::new(
        Some(tmp.path().to_path_buf()),
        Some(core_sock.clone()),
    ));
    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx).with_supervisor(supervisor.clone());
    let core_server = CoreIpcServer::new(&core_sock, core_api);
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let core_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
            .expect("Core server failed");
    });
    wait_for_path(&core_sock).await;

    // A host that announces it started but never binds its socket, then exits: its launch stays
    // in flight for about a second and then fails.
    let launch = tokio::spawn({
        let supervisor = supervisor.clone();
        async move {
            supervisor
                .spawn_plugin(
                    "slow_host",
                    "sh",
                    &["-c", "touch \"$KANON_HOST_SOCK.started\"; sleep 1"],
                )
                .await
        }
    });
    let host_sock = host_socket_path("slow_host", Some(tmp.path()));
    wait_for_path(&PathBuf::from(format!("{}.started", host_sock.display()))).await;

    let mut client = BotApiServiceClient::new(connect_ipc(&core_sock).await.expect("dial core"));
    let registration = RegisterHostRequest {
        host_id: "slow_host".to_string(),
        runtime: "rust".to_string(),
        endpoint: host_sock.to_string_lossy().to_string(),
        loaded_plugin_ids: vec!["test.plugin".to_string()],
    };
    let response = client
        .register_host(registration.clone())
        .await
        .expect("RegisterHost RPC")
        .into_inner();
    assert!(response.success, "{}", response.message);
    assert!(
        supervisor.get_host("slow_host").await.is_none(),
        "the launch, not the registration, adds a launched host once it serves"
    );

    // Once the launch has failed it no longer claims the id, so the same registration is an
    // external one again and fails on the socket nobody bound.
    let launched = launch.await.expect("launch task");
    assert!(
        matches!(launched, Err(SupervisorError::PrematureExit { .. })),
        "{launched:?}"
    );
    let response = client
        .register_host(registration)
        .await
        .expect("RegisterHost RPC")
        .into_inner();
    assert!(!response.success, "{}", response.message);

    let _ = core_shutdown_tx.send(());
    let _ = core_task.await;
}

#[tokio::test]
async fn test_send_message_dispatches_to_platform_adapter() {
    let tmp = tempdir().expect("tempdir");
    let core_sock = tmp.path().join("core.sock");

    let supervisor = Arc::new(Supervisor::new(
        Some(tmp.path().to_path_buf()),
        Some(core_sock.clone()),
    ));

    // Register a mock platform adapter
    let deliveries = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(MockAdapter {
        platform: "mock_platform".to_string(),
        deliveries: deliveries.clone(),
    });
    supervisor
        .adapters()
        .register(adapter)
        .await
        .expect("register adapter");

    let engine = Arc::new(PipelineEngine::new(supervisor.clone()));
    let _dispatcher = engine.clone().start_outbound_dispatcher();

    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx)
        .with_supervisor(supervisor.clone())
        .with_outbound_sender(engine.outbound_sender());
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let core_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
            .expect("Core server failed");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and call SendMessage
    let channel = connect_ipc(&core_sock).await.expect("connect to core");
    let mut client = BotApiServiceClient::new(channel);

    let send_req = SendMessageRequest {
        platform: "mock_platform".to_string(),
        channel_id: "channel_abc".to_string(),
        recipient_id: "user_xyz".to_string(),
        segments: vec![MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: "Hello from SendMessage".to_string(),
            })),
        }],
    };

    let send_resp = client
        .send_message(send_req)
        .await
        .expect("SendMessage RPC succeeded")
        .into_inner();

    assert!(send_resp.success, "SendMessage must report success");
    assert!(
        send_resp.accepted,
        "SendMessage must explicitly report accepted=true for outbound queue admission"
    );
    assert!(!send_resp.message_id.is_empty());

    // Wait for the outbound dispatcher and platform worker to process the delivery
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(15)).await;
        if !deliveries.lock().await.is_empty() {
            break;
        }
    }

    let recorded = deliveries.lock().await;
    assert_eq!(recorded.len(), 1, "Message must have reached MockAdapter");
    assert_eq!(recorded[0].platform, "mock_platform");
    assert_eq!(recorded[0].channel_id, "channel_abc");
    assert_eq!(recorded[0].recipient_id, "user_xyz");
    assert_eq!(recorded[0].segments.len(), 1);
    drop(recorded);

    // ReplyMessage uses the same dispatcher but returns the platform receipt and
    // preserves the original message ID required for QQ passive replies.
    let result = client
        .reply_message(DeliverMessageRequest {
            platform: "mock_platform".into(),
            channel_id: "channel_abc".into(),
            recipient_id: "user_xyz".into(),
            event_id: "original-qq-message".into(),
            segments: vec![],
        })
        .await
        .expect("ReplyMessage RPC succeeded")
        .into_inner();
    assert!(result.success);
    let recorded = deliveries.lock().await;
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1].event_id, "original-qq-message");
    drop(recorded);

    // Clean up
    let _ = core_shutdown_tx.send(());
    let _ = core_task.await;
}

#[tokio::test]
async fn test_storage_endpoints_return_unimplemented() {
    let tmp = tempdir().expect("tempdir");
    let core_sock = tmp.path().join("core.sock");

    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx);
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let core_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
            .expect("Core server failed");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let channel = connect_ipc(&core_sock).await.expect("connect to core");
    let mut client = BotApiServiceClient::new(channel);

    // Test SetStorage
    let set_err = client
        .set_storage(SetStorageRequest {
            plugin_id: "test".to_string(),
            key: "foo".to_string(),
            value: vec![1, 2, 3],
            ttl_seconds: 0,
        })
        .await
        .expect_err("SetStorage must fail");

    assert_eq!(set_err.code(), Code::Unimplemented);
    assert!(set_err.message().contains("unsupported"));

    // Test GetStorage
    let get_err = client
        .get_storage(GetStorageRequest {
            plugin_id: "test".to_string(),
            key: "foo".to_string(),
        })
        .await
        .expect_err("GetStorage must fail");

    assert_eq!(get_err.code(), Code::Unimplemented);
    assert!(get_err.message().contains("unsupported"));

    // Clean up
    let _ = core_shutdown_tx.send(());
    let _ = core_task.await;
}
