//! Tests for the core-liveness watchdog.
//!
//! The watchdog is what keeps a plugin host from outliving its core: an orphan keeps serving its
//! platform, and once a new core starts both processes handle the same messages. These tests pin
//! the two outcomes that matter — a reachable core never triggers a stop, and a core that
//! disappears triggers exactly one.

use std::time::Duration;

use async_trait::async_trait;
use kanon_proto::v1::bot_api_service_server::{BotApiService, BotApiServiceServer};
use kanon_proto::v1::{
    GetStorageRequest, GetStorageResponse, IngestEventRequest, IngestEventResponse, LlmRequest,
    PingRequest, PingResponse, RegisterHostRequest, RegisterHostResponse, SendMessageRequest,
    SendMessageResponse, SetStorageRequest, SetStorageResponse,
};
use kanon_sdk::CoreHandle;
use kanon_sdk::watchdog::{CoreWatchdogConfig, StopReason, watch_core};
use tokio::sync::mpsc;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

/// Stub core that answers liveness probes and nothing else.
struct StubCore;

#[async_trait]
impl BotApiService for StubCore {
    async fn delete_storage(
        &self,
        _request: tonic::Request<kanon_proto::v1::DeleteStorageRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DeleteStorageResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("delete_storage"))
    }

    async fn list_storage(
        &self,
        _request: tonic::Request<kanon_proto::v1::ListStorageRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ListStorageResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("list_storage"))
    }

    async fn list_conversations(
        &self,
        _request: tonic::Request<kanon_proto::v1::ConversationsRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ConversationList>, tonic::Status> {
        Err(tonic::Status::unimplemented("list_conversations"))
    }

    async fn new_conversation(
        &self,
        _request: tonic::Request<kanon_proto::v1::ConversationsRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ConversationList>, tonic::Status> {
        Err(tonic::Status::unimplemented("new_conversation"))
    }

    async fn switch_conversation(
        &self,
        _request: tonic::Request<kanon_proto::v1::SelectConversationRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ConversationList>, tonic::Status> {
        Err(tonic::Status::unimplemented("switch_conversation"))
    }

    async fn delete_conversation(
        &self,
        _request: tonic::Request<kanon_proto::v1::SelectConversationRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ConversationList>, tonic::Status> {
        Err(tonic::Status::unimplemented("delete_conversation"))
    }

    async fn append_conversation(
        &self,
        _request: tonic::Request<kanon_proto::v1::AppendConversationRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::AppendConversationResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("append_conversation"))
    }

    async fn list_personas(
        &self,
        _request: tonic::Request<kanon_proto::v1::ListPersonasRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ListPersonasResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("list_personas"))
    }

    async fn upsert_persona(
        &self,
        _request: tonic::Request<kanon_proto::v1::Persona>,
    ) -> Result<tonic::Response<kanon_proto::v1::UpsertPersonaResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("upsert_persona"))
    }

    async fn delete_persona(
        &self,
        _request: tonic::Request<kanon_proto::v1::DeletePersonaRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DeletePersonaResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("delete_persona"))
    }

    async fn run_agent(
        &self,
        _request: tonic::Request<kanon_proto::v1::RunAgentRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::RunAgentResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("run_agent"))
    }

    async fn refresh_plugin_meta(
        &self,
        _request: tonic::Request<kanon_proto::v1::RefreshPluginMetaRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::RefreshPluginMetaResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("refresh_plugin_meta"))
    }

    async fn render_image(
        &self,
        _request: tonic::Request<kanon_proto::v1::RenderImageRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::RenderImageResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("render_image"))
    }
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            timestamp: request.into_inner().timestamp,
        }))
    }

    async fn register_host(
        &self,
        _request: Request<RegisterHostRequest>,
    ) -> Result<Response<RegisterHostResponse>, Status> {
        Ok(Response::new(RegisterHostResponse {
            success: true,
            message: "ok".to_string(),
            core_metadata: None,
        }))
    }

    async fn ingest_event(
        &self,
        _request: Request<IngestEventRequest>,
    ) -> Result<Response<IngestEventResponse>, Status> {
        Ok(Response::new(IngestEventResponse {
            accepted: true,
            event_id: String::new(),
        }))
    }

    async fn reply_message(
        &self,
        _request: Request<kanon_proto::v1::DeliverMessageRequest>,
    ) -> Result<Response<kanon_proto::v1::DeliverMessageResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn send_message(
        &self,
        _request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        Ok(Response::new(SendMessageResponse {
            success: true,
            message_id: String::new(),
            error_message: String::new(),
            accepted: true,
        }))
    }

    type RequestLLMStream =
        tokio_stream::wrappers::ReceiverStream<Result<kanon_proto::v1::LlmChunk, Status>>;

    async fn call_platform_api(
        &self,
        _request: tonic::Request<kanon_proto::v1::PlatformApiRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PlatformApiResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("not used by this test"))
    }

    async fn request_llm(
        &self,
        _request: Request<LlmRequest>,
    ) -> Result<Response<Self::RequestLLMStream>, Status> {
        Err(Status::unimplemented(
            "stub core does not serve LLM requests",
        ))
    }

    async fn set_storage(
        &self,
        _request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        Err(Status::unimplemented("stub core has no storage"))
    }

    async fn get_storage(
        &self,
        _request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        Err(Status::unimplemented("stub core has no storage"))
    }

    async fn get_conversation_history(
        &self,
        _request: Request<kanon_sdk::proto::v1::ConversationHistoryRequest>,
    ) -> Result<Response<kanon_sdk::proto::v1::ConversationHistoryResponse>, Status> {
        Err(Status::unimplemented("stub core has no conversations"))
    }
}

/// Starts a stub core on an ephemeral port and returns a handle plus its server task.
async fn spawn_stub_core() -> (CoreHandle, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub core");
    let addr = listener.local_addr().expect("stub core addr");
    let incoming = TcpListenerStream::new(listener);

    let server = tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(BotApiServiceServer::new(StubCore))
            .serve_with_incoming(incoming)
            .await;
    });

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .expect("channel uri")
        .connect()
        .await
        .expect("connect to stub core");

    (CoreHandle::new(channel), server)
}

/// Aggressive cadence so the tests finish quickly.
fn fast_config() -> CoreWatchdogConfig {
    CoreWatchdogConfig {
        interval: Duration::from_millis(20),
        timeout: Duration::from_millis(50),
        max_failures: 2,
    }
}

#[tokio::test]
async fn a_reachable_core_never_triggers_a_stop() {
    let (handle, _server) = spawn_stub_core().await;
    let (_stop_tx, stop_rx) = mpsc::channel(1);

    let watcher = tokio::spawn(watch_core(handle, fast_config(), stop_rx));

    // Many probe intervals pass while the core answers: the watchdog must stay armed, not fire.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !watcher.is_finished(),
        "a healthy core must not stop the host"
    );

    watcher.abort();
}

#[tokio::test]
async fn a_core_that_disappears_stops_the_host() {
    let (handle, server) = spawn_stub_core().await;
    let (_stop_tx, stop_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(watch_core(handle, fast_config(), stop_rx));

    // The core exits (crash, SIGKILL, or a replaced process).
    server.abort();

    let reason = tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .expect("watchdog must resolve once the core is gone")
        .expect("watchdog task");
    assert_eq!(reason, StopReason::CoreLost);
}

#[tokio::test]
async fn an_explicit_shutdown_wins_over_probing() {
    let (handle, _server) = spawn_stub_core().await;
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(watch_core(handle, fast_config(), stop_rx));

    stop_tx.send(()).await.expect("signal stop");

    let reason = tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .expect("watchdog must resolve when the host stops itself")
        .expect("watchdog task");
    assert_eq!(reason, StopReason::Requested);
}
