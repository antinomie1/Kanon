//! Coverage for the Rust SDK's adapter-facing primitives.
//!
//! Both directions of the adapter contract are exercised against real gRPC endpoints:
//! - outbound: the host must dispatch `OnDeliverMessage` into the plugin's overridden hook;
//! - inbound: `CoreHandle::ingest_event` must reach `BotApiService.IngestEvent` and relay the
//!   Fast-ACK acknowledgement without inventing success.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kanon_proto::v1::bot_api_service_server::{BotApiService, BotApiServiceServer};
use kanon_proto::v1::message_pipeline_service_client::MessagePipelineServiceClient;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DeliverMessageRequest, DeliverMessageResponse,
    GetPluginMetaRequest, GetStorageRequest, GetStorageResponse, IngestEventRequest,
    IngestEventResponse, LlmChunk, LlmRequest, PingRequest, PingResponse, PipelineEventRequest,
    PluginMeta, RegisterHostRequest, RegisterHostResponse, SendMessageRequest, SendMessageResponse,
    SetStorageRequest, SetStorageResponse, TextSegment,
};
use kanon_sdk::KanonHost;
use kanon_sdk::context::{CoreHandle, PluginContext};
use kanon_sdk::plugin::{Plugin, PluginResult};
use kanon_transport::{IpcListener, connect_ipc};
use tokio::sync::{Mutex, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// Plugin that records the messages the host delivers to it.
#[derive(Default)]
struct AdapterPlugin {
    delivered: Arc<Mutex<Vec<DeliverMessageRequest>>>,
}

#[async_trait::async_trait]
impl Plugin for AdapterPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "org.kanon.plugin.sdk_adapter_fixture".to_string(),
            name: "SDK Adapter Fixture".to_string(),
            version: "1.0.0".to_string(),
            ..PluginMeta::default()
        }
    }

    async fn on_deliver_message(
        &self,
        req: DeliverMessageRequest,
    ) -> PluginResult<DeliverMessageResponse> {
        let message_id = format!("sdk-{}", req.channel_id);
        self.delivered.lock().await.push(req);

        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }
}

/// Plugin that does not implement outbound delivery at all.
#[derive(Default)]
struct NonAdapterPlugin;

#[async_trait::async_trait]
impl Plugin for NonAdapterPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "org.kanon.plugin.sdk_non_adapter".to_string(),
            name: "SDK Non Adapter".to_string(),
            version: "1.0.0".to_string(),
            ..PluginMeta::default()
        }
    }
}

/// Core stub recording inbound events and registrations.
#[derive(Default)]
struct CoreStub {
    ingested: Mutex<Vec<IngestEventRequest>>,
    registrations: Mutex<Vec<RegisterHostRequest>>,
}

#[tonic::async_trait]
impl BotApiService for CoreStub {
    /// Liveness probe: hosts use it to notice that their core is gone.
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            timestamp: request.into_inner().timestamp,
        }))
    }

    async fn register_host(
        &self,
        request: Request<RegisterHostRequest>,
    ) -> Result<Response<RegisterHostResponse>, Status> {
        let req = request.into_inner();
        self.registrations.lock().await.push(req);
        Ok(Response::new(RegisterHostResponse {
            success: true,
            message: "registered".to_string(),
            core_metadata: None,
        }))
    }

    async fn ingest_event(
        &self,
        request: Request<IngestEventRequest>,
    ) -> Result<Response<IngestEventResponse>, Status> {
        let req = request.into_inner();
        let event_id = req
            .event
            .as_ref()
            .map(|event| event.event_id.clone())
            .unwrap_or_default();
        self.ingested.lock().await.push(req);

        Ok(Response::new(IngestEventResponse {
            accepted: true,
            event_id,
        }))
    }

    async fn reply_message(
        &self,
        _request: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn send_message(
        &self,
        _request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    type RequestLLMStream = ReceiverStream<Result<LlmChunk, Status>>;

    /// Echoes the parameters back as the result, so a test sees both directions of the
    /// JSON conversion.
    async fn call_platform_api(
        &self,
        request: tonic::Request<kanon_proto::v1::PlatformApiRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PlatformApiResponse>, tonic::Status> {
        let request = request.into_inner();
        Ok(tonic::Response::new(kanon_proto::v1::PlatformApiResponse {
            result: request.params.map(|params| prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(params)),
            }),
        }))
    }

    async fn request_llm(
        &self,
        _request: Request<LlmRequest>,
    ) -> Result<Response<Self::RequestLLMStream>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn set_storage(
        &self,
        _request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn get_storage(
        &self,
        _request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn get_conversation_history(
        &self,
        _request: Request<kanon_sdk::proto::v1::ConversationHistoryRequest>,
    ) -> Result<Response<kanon_sdk::proto::v1::ConversationHistoryResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }
}

/// Starts a Core stub serving `BotApiService` on `socket_path`.
async fn start_core_stub(socket_path: &PathBuf) -> Arc<CoreStub> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }

    let stub = Arc::new(CoreStub::default());
    let served = stub.clone();
    let listener = IpcListener::bind(socket_path).expect("core stub binds");
    let incoming = listener.incoming();

    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(BotApiServiceServer::new(CoreStubServer { inner: served }))
            .serve_with_incoming(incoming)
            .await;
    });

    stub
}

/// Tonic adapter delegating to [`CoreStub`].
struct CoreStubServer {
    inner: Arc<CoreStub>,
}

#[tonic::async_trait]
impl BotApiService for CoreStubServer {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        self.inner.ping(request).await
    }

    async fn register_host(
        &self,
        request: Request<RegisterHostRequest>,
    ) -> Result<Response<RegisterHostResponse>, Status> {
        self.inner.register_host(request).await
    }

    async fn ingest_event(
        &self,
        request: Request<IngestEventRequest>,
    ) -> Result<Response<IngestEventResponse>, Status> {
        self.inner.ingest_event(request).await
    }

    async fn reply_message(
        &self,
        _request: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        self.inner.send_message(request).await
    }

    type RequestLLMStream = ReceiverStream<Result<LlmChunk, Status>>;

    async fn call_platform_api(
        &self,
        request: tonic::Request<kanon_proto::v1::PlatformApiRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PlatformApiResponse>, tonic::Status> {
        self.inner.call_platform_api(request).await
    }

    async fn request_llm(
        &self,
        request: Request<LlmRequest>,
    ) -> Result<Response<Self::RequestLLMStream>, Status> {
        self.inner.request_llm(request).await
    }

    async fn set_storage(
        &self,
        request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        self.inner.set_storage(request).await
    }

    async fn get_storage(
        &self,
        request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        self.inner.get_storage(request).await
    }

    async fn get_conversation_history(
        &self,
        _request: Request<kanon_sdk::proto::v1::ConversationHistoryRequest>,
    ) -> Result<Response<kanon_sdk::proto::v1::ConversationHistoryResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }
}

/// Locates a temporary socket path that does not collide across tests.
fn temp_socket(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("kanon-sdk-tests");
    std::fs::create_dir_all(&dir).expect("temp socket dir");
    dir.join(format!("{name}-{}.sock", std::process::id()))
}

/// Waits until a freshly bound gRPC socket answers the given connection attempt.
async fn connect_with_retry(path: &Path) -> tonic::transport::Channel {
    for _ in 0..50 {
        if let Ok(channel) = connect_ipc(path.to_path_buf()).await {
            return channel;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("endpoint {} never became reachable", path.display());
}

/// `CoreHandle::ingest_event` delivers the exact payload and relays the acknowledgement.
#[tokio::test]
async fn core_handle_ingests_events_into_the_core() {
    let socket = temp_socket("core-ingest");
    let stub = start_core_stub(&socket).await;

    let channel = connect_with_retry(&socket).await;
    let handle = CoreHandle::new(channel);

    let response = handle
        .ingest_event(IngestEventRequest {
            platform: "sdk_test".to_string(),
            event: Some(PipelineEventRequest {
                event_id: "evt-sdk-1".to_string(),
                platform: "sdk_test".to_string(),
                channel_id: "chan-1".to_string(),
                sender_id: "alice".to_string(),
                raw_text: "hello from the sdk".to_string(),
                segments: vec![],
                metadata: None,
            }),
        })
        .await
        .expect("ingest succeeds");

    assert!(response.accepted);
    assert_eq!(response.event_id, "evt-sdk-1");

    let ingested = stub.ingested.lock().await;
    assert_eq!(ingested.len(), 1);
    let event = ingested[0].event.as_ref().expect("inner event");
    assert_eq!(event.platform, "sdk_test");
    assert_eq!(event.channel_id, "chan-1");
    assert_eq!(event.sender_id, "alice");
    assert_eq!(event.raw_text, "hello from the sdk");

    // The convenience helper builds the same shape from individual fields.
    drop(ingested);
    handle
        .ingest_text("sdk_test", "chan-2", "bob", "second message")
        .await
        .expect("ingest_text succeeds");

    let ingested = stub.ingested.lock().await;
    assert_eq!(ingested.len(), 2);
    assert_eq!(
        ingested[1]
            .event
            .as_ref()
            .map(|event| event.channel_id.as_str()),
        Some("chan-2")
    );

    let _ = std::fs::remove_file(&socket);
}

/// Registration is the reachability proof used by the host runner.
#[tokio::test]
async fn core_handle_calls_the_platform_api_with_json() {
    let socket = temp_socket("core-platform-api");
    let _stub = start_core_stub(&socket).await;
    let handle = CoreHandle::new(connect_with_retry(&socket).await);

    let result = handle
        .call_platform_api(
            "onebot",
            "set_group_ban",
            serde_json::json!({ "group_id": 1, "users": ["a"], "notify": true }),
        )
        .await
        .expect("platform API call");
    // Numbers travel as doubles, so integers come back as floats.
    assert_eq!(
        result,
        serde_json::json!({ "group_id": 1.0, "users": ["a"], "notify": true })
    );

    let rejected = handle
        .call_platform_api("onebot", "x", serde_json::json!([1]))
        .await
        .expect_err("non-object parameters");
    assert_eq!(rejected.code(), tonic::Code::InvalidArgument);
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn core_handle_registers_the_host() {
    let socket = temp_socket("core-register");
    let stub = start_core_stub(&socket).await;

    let handle = CoreHandle::new(connect_with_retry(&socket).await);
    let response = handle
        .register_host(RegisterHostRequest {
            host_id: "host_sdk_fixture".to_string(),
            runtime: "rust".to_string(),
            endpoint: "/tmp/host_sdk_fixture.sock".to_string(),
            loaded_plugin_ids: vec!["org.kanon.plugin.sdk_adapter_fixture".to_string()],
        })
        .await
        .expect("registration succeeds");

    assert!(response.success);
    let registrations = stub.registrations.lock().await;
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].host_id, "host_sdk_fixture");

    let _ = std::fs::remove_file(&socket);
}

/// The host dispatches `OnDeliverMessage` into the plugin's overridden hook.
#[tokio::test]
async fn host_dispatches_outbound_delivery_to_plugin() {
    let host_socket = temp_socket("plugin-host-deliver");
    if host_socket.exists() {
        let _ = std::fs::remove_file(&host_socket);
    }

    let plugin = Arc::new(AdapterPlugin::default());
    let delivered = plugin.delivered.clone();
    let host = KanonHost::new(AdapterPlugin {
        delivered: delivered.clone(),
    })
    .with_socket_path(host_socket.clone());

    // The host must not be started with a core socket here: standalone mode is the point.
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = host
            .run_with_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let channel = connect_with_retry(&host_socket).await;
    let mut client = MessagePipelineServiceClient::new(channel);

    let response = client
        .on_deliver_message(DeliverMessageRequest {
            platform: "sdk_platform".to_string(),
            channel_id: "chan-7".to_string(),
            recipient_id: "alice".to_string(),
            segments: vec![kanon_proto::v1::MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "outbound text".to_string(),
                })),
            }],
            event_id: "evt-sdk-1".to_string(),
        })
        .await
        .expect("delivery RPC succeeds")
        .into_inner();

    assert!(response.success);
    assert_eq!(response.message_id, "sdk-chan-7");

    let recorded = delivered.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].platform, "sdk_platform");
    assert_eq!(recorded[0].recipient_id, "alice");

    let _ = shutdown_tx.send(());
    let _ = std::fs::remove_file(&host_socket);
}

/// A plugin that is not an adapter refuses delivery instead of reporting false success.
#[tokio::test]
async fn non_adapter_plugin_refuses_delivery() {
    let host_socket = temp_socket("plugin-host-refuse");
    if host_socket.exists() {
        let _ = std::fs::remove_file(&host_socket);
    }

    let host = KanonHost::new(NonAdapterPlugin).with_socket_path(host_socket.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = host
            .run_with_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let channel = connect_with_retry(&host_socket).await;
    let mut client = MessagePipelineServiceClient::new(channel);

    let response = client
        .on_deliver_message(DeliverMessageRequest {
            platform: "sdk_platform".to_string(),
            channel_id: "chan-7".to_string(),
            recipient_id: "alice".to_string(),
            segments: vec![],
            event_id: "evt-sdk-refuse".to_string(),
        })
        .await
        .expect("RPC completes with a failure payload")
        .into_inner();

    assert!(!response.success);
    assert!(
        response
            .error_message
            .contains("does not implement on_deliver_message"),
        "unexpected message: {}",
        response.error_message
    );

    let _ = shutdown_tx.send(());
    let _ = std::fs::remove_file(&host_socket);
}

/// Standalone mode leaves `ctx.core` empty instead of fabricating a handle.
#[tokio::test]
async fn context_without_core_is_explicit() {
    let mut ctx = PluginContext::new(PathBuf::from("/tmp/kanon-sdk-context"), None);
    assert!(ctx.core.is_none());

    // A plugin may still opt in explicitly when embedded by an application.
    let socket = temp_socket("core-context");
    start_core_stub(&socket).await;
    let handle = CoreHandle::new(connect_with_retry(&socket).await);
    ctx = ctx.with_core(handle);
    assert!(ctx.core.is_some());

    let _ = std::fs::remove_file(&socket);
}

/// Meta reporting stays available for tooling that inspects a plugin without running it.
#[tokio::test]
async fn plugin_meta_is_reported() {
    let host_socket = temp_socket("plugin-host-meta");
    if host_socket.exists() {
        let _ = std::fs::remove_file(&host_socket);
    }

    let host = KanonHost::new(NonAdapterPlugin).with_socket_path(host_socket.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = host
            .run_with_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let channel = connect_with_retry(&host_socket).await;
    let mut client =
        kanon_proto::v1::plugin_host_service_client::PluginHostServiceClient::new(channel);
    let response = client
        .get_plugin_meta(GetPluginMetaRequest {})
        .await
        .expect("meta RPC succeeds")
        .into_inner();

    assert_eq!(response.plugins.len(), 1);
    assert_eq!(response.plugins[0].id, "org.kanon.plugin.sdk_non_adapter");

    let _ = shutdown_tx.send(());
    let _ = std::fs::remove_file(&host_socket);
}

/// Unused command hook keeps the default contract for completeness of the fixture.
#[allow(dead_code)]
async fn default_command_response(req: CommandExecuteRequest) -> CommandExecuteResponse {
    CommandExecuteResponse {
        success: true,
        replies: vec![],
        error_message: format!("Command '{}' executed by default stub handler", req.command),
        ..Default::default()
    }
}
