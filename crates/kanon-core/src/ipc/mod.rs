//! Kanon Core IPC server implementation.
//!
//! Provides the central gRPC endpoint (`core.sock`) through which plugin hosts
//! communicate with the Core microkernel via [`BotApiService`].

use crate::pipeline::engine::OutboundMessage;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tonic::{Request, Response, Status};

use kanon_proto::v1::bot_api_service_server::{BotApiService, BotApiServiceServer};
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, GetStorageRequest, GetStorageResponse,
    IngestEventRequest, IngestEventResponse, LlmChunk, LlmRequest, RegisterHostRequest,
    RegisterHostResponse, SendMessageRequest, SendMessageResponse, SetStorageRequest,
    SetStorageResponse,
};
use kanon_transport::{IpcListener, core_socket_path};

use crate::adapter::{EventIngress, IngestError};
use crate::supervisor::{HostRegistration, Supervisor};
use kanon_llm::{AgentSlot, ChatMessage, ChatRequest, LlmGateway};
use tokio_stream::StreamExt;

/// Default capacity for the inbound asynchronous event ingest queue.
///
/// Under high throughput, this queue serves as a backpressure boundary separating
/// external IM adapters from downstream LLM reasoning and plugin pipelines.
pub const DEFAULT_INGEST_QUEUE_CAPACITY: usize = 10_000;

/// Core implementation of the [`BotApiService`] gRPC service.
#[derive(Debug, Clone)]
pub struct CoreApiService {
    /// Shared Fast-ACK ingest handle; also handed to platform adapters.
    ingress: EventIngress,
    /// Reference to the central Supervisor managing host lifecycles and registration.
    supervisor: Option<Arc<Supervisor>>,
    /// Producer channel connected to the central pipeline outbound dispatcher.
    outbound_sender: Option<mpsc::Sender<OutboundMessage>>,
    /// Shared agent slot resolving the node's live model provider for `RequestLLM`.
    ///
    /// A slot (not a captured gateway) so that a provider configured, replaced or cleared
    /// through the control plane is observed by the next request without a restart.
    llm: Option<Arc<AgentSlot>>,
}

impl CoreApiService {
    /// Creates a new `CoreApiService` with the specified event queue sender.
    ///
    /// Accepts anything convertible into an [`EventIngress`] so callers may keep handing over a
    /// raw `mpsc::Sender` while adapters share the very same ingress handle.
    pub fn new(ingress: impl Into<EventIngress>) -> Self {
        Self {
            ingress: ingress.into(),
            supervisor: None,
            outbound_sender: None,
            llm: None,
        }
    }

    /// Returns the shared ingest handle, so adapters can push inbound events.
    pub fn ingress(&self) -> &EventIngress {
        &self.ingress
    }

    /// Configures the attached supervisor instance for unified host registration.
    pub fn with_supervisor(mut self, supervisor: Arc<Supervisor>) -> Self {
        self.supervisor = Some(supervisor);
        self
    }

    /// Configures the outbound message queue sender for dispatching external messages.
    pub fn with_outbound_sender(mut self, sender: mpsc::Sender<OutboundMessage>) -> Self {
        self.outbound_sender = Some(sender);
        self
    }

    /// Shares the node's agent slot, enabling `RequestLLM` for plugin hosts.
    pub fn with_agent_slot(mut self, agent: Arc<AgentSlot>) -> Self {
        self.llm = Some(agent);
        self
    }

    /// Configures a fixed LLM gateway instance for embedded deployments and tests.
    ///
    /// The gateway is captured into a slot holding exactly one agent, so a caller that never
    /// rewires the slot keeps the previous static behaviour.
    pub fn with_gateway(mut self, gateway: Arc<LlmGateway>) -> Self {
        // The gateway carries no session memory of its own: `RequestLLM` is a stateless
        // pass-through, so a private in-memory store is sufficient and never shared.
        let agent = kanon_llm::Agent::builder("core-gateway", gateway.provider().clone())
            .model(gateway.default_model())
            .build();
        self.llm = Some(Arc::new(AgentSlot::with_agent(Arc::new(agent))));
        self
    }

    /// Resolves the provider that should serve a `RequestLLM` call right now.
    ///
    /// Returns `None` when the node has no model provider configured; callers must surface that
    /// as an explicit `unavailable` status rather than fabricating a completion.
    fn current_gateway(&self) -> Option<LlmGateway> {
        let agent = self.llm.as_ref()?.current()?;
        Some(LlmGateway::new(
            agent.provider().clone(),
            agent.config().default_model.clone(),
        ))
    }
}

#[tonic::async_trait]
impl BotApiService for CoreApiService {
    /// Registers a newly initialized plugin host with the Core microkernel,
    /// converging directly into the Supervisor's unified host registry.
    async fn register_host(
        &self,
        request: Request<RegisterHostRequest>,
    ) -> Result<Response<RegisterHostResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            host_id = %req.host_id,
            runtime = %req.runtime,
            endpoint = %req.endpoint,
            "Received Host registration request"
        );

        let supervisor = match &self.supervisor {
            Some(s) => s,
            None => {
                return Err(Status::unavailable(
                    "Supervisor is not configured on CoreApiService; cannot register host",
                ));
            }
        };

        match supervisor
            .register_host_endpoint(
                &req.host_id,
                &req.runtime,
                &req.endpoint,
                &req.loaded_plugin_ids,
            )
            .await
        {
            Ok(HostRegistration::Registered(_)) => Ok(Response::new(RegisterHostResponse {
                success: true,
                message: format!("Host '{}' registered successfully", req.host_id),
                core_metadata: None,
            })),
            // The host was launched by this core, which finishes its handshake as soon as the
            // host serves; the host must not wait for that, since it may register before serving.
            Ok(HostRegistration::Launching) => Ok(Response::new(RegisterHostResponse {
                success: true,
                message: format!(
                    "Host '{}' acknowledged; the core completes its handshake once it serves",
                    req.host_id
                ),
                core_metadata: None,
            })),
            Err(e) => {
                tracing::error!(
                    host_id = %req.host_id,
                    error = %e,
                    "Failed to register host into unified supervisor registry"
                );
                Ok(Response::new(RegisterHostResponse {
                    success: false,
                    message: format!("Failed to register host: {e}"),
                    core_metadata: None,
                }))
            }
        }
    }

    /// Ingests an inbound event into the core pipeline with millisecond Fast-ACK.
    ///
    /// # Fast-ACK Concurrency Guarantee
    /// To ensure external IM heartbeat keep-alive connections (e.g. WebSocket pings)
    /// never block on slow downstream LLM generation or plugin I/O, this method
    /// uses non-blocking `try_send` into a bounded Tokio MPSC channel.
    /// Execution returns in < 50µs, severing the backpressure chain from IM adapters.
    async fn ingest_event(
        &self,
        request: Request<IngestEventRequest>,
    ) -> Result<Response<IngestEventResponse>, Status> {
        let req = request.into_inner();

        let event_id = req
            .event
            .as_ref()
            .map(|e| e.event_id.clone())
            .unwrap_or_default();

        // Non-blocking enqueue to guarantee Fast-ACK (< 50µs).
        match self.ingress.try_ingest(req) {
            Ok(()) => Ok(Response::new(IngestEventResponse {
                accepted: true,
                event_id,
            })),
            Err(IngestError::QueueFull) => {
                // High watermark reached: report backpressure but acknowledge failure quickly.
                tracing::warn!(
                    event_id = %event_id,
                    "Inbound event queue full; dropping event to protect microkernel stability"
                );
                Ok(Response::new(IngestEventResponse {
                    accepted: false,
                    event_id,
                }))
            }
            Err(IngestError::Closed) => Err(Status::unavailable(
                "Microkernel event queue has been closed",
            )),
        }
    }

    /// Dispatches an outbound message to the target platform adapter via the pipeline queue.
    /// Preserves the original event ID and waits for the platform delivery result.
    async fn reply_message(
        &self,
        request: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        let request = request.into_inner();
        if request.event_id.is_empty()
            || request.platform.is_empty()
            || request.channel_id.is_empty()
        {
            return Err(Status::invalid_argument(
                "reply requires original event_id, platform and channel_id",
            ));
        }
        let sender = self
            .outbound_sender
            .as_ref()
            .ok_or_else(|| Status::unavailable("Outbound delivery dispatcher is not configured"))?;
        let (receipt, result) = oneshot::channel();
        sender
            .try_send(OutboundMessage {
                request,
                split_lines: false,
                receipt: Some(receipt),
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    Status::resource_exhausted("Outbound queue full; reply not queued")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    Status::unavailable("Outbound dispatcher stopped; reply not queued")
                }
            })?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(30), result)
            .await
            .map_err(|_| {
                Status::deadline_exceeded(
                    "Reply delivery outcome unknown; do not automatically retry",
                )
            })?
            .map_err(|_| {
                Status::unavailable("Outbound dispatcher stopped before delivery was confirmed")
            })?;
        Ok(Response::new(response))
    }

    /// Admits a proactive message to the queue without waiting for platform delivery.
    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let req = request.into_inner();
        tracing::debug!(
            platform = %req.platform,
            channel_id = %req.channel_id,
            "Core received SendMessage request"
        );

        let sender = match &self.outbound_sender {
            Some(s) => s,
            None => {
                return Err(Status::unavailable(
                    "Outbound delivery dispatcher is not configured on CoreApiService",
                ));
            }
        };

        static MSG_SEQ: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let event_id = format!(
            "send-{:x}-{:x}",
            now_ms,
            MSG_SEQ.fetch_add(1, Ordering::Relaxed)
        );

        let deliver_req = DeliverMessageRequest {
            platform: req.platform.clone(),
            channel_id: req.channel_id.clone(),
            recipient_id: req.recipient_id.clone(),
            segments: req.segments,
            event_id: event_id.clone(),
        };

        match sender.try_send(deliver_req.into()) {
            Ok(()) => Ok(Response::new(SendMessageResponse {
                accepted: true,
                success: true,
                message_id: event_id,
                error_message: String::new(),
            })),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    platform = %req.platform,
                    channel_id = %req.channel_id,
                    "Outbound queue is full; SendMessage request rejected"
                );
                Ok(Response::new(SendMessageResponse {
                    accepted: false,
                    success: false,
                    message_id: String::new(),
                    error_message: "Outbound queue full; delivery dropped".to_string(),
                }))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(Status::unavailable("Outbound message queue is closed"))
            }
        }
    }

    /// Answers a liveness probe from a plugin host.
    ///
    /// Deliberately trivial: it must not touch the pipeline, the supervisor or the ingest queue,
    /// so that a host can distinguish "core is unreachable" from "core is busy".
    async fn ping(
        &self,
        request: Request<kanon_proto::v1::PingRequest>,
    ) -> Result<Response<kanon_proto::v1::PingResponse>, Status> {
        Ok(Response::new(kanon_proto::v1::PingResponse {
            timestamp: request.into_inner().timestamp,
        }))
    }

    /// Server streaming response type for LLM token generation chunks.
    type RequestLLMStream = tokio_stream::wrappers::ReceiverStream<Result<LlmChunk, Status>>;

    /// Invokes the LLM gateway for streaming text completions.
    async fn request_llm(
        &self,
        request: Request<LlmRequest>,
    ) -> Result<Response<Self::RequestLLMStream>, Status> {
        let req = request.into_inner();
        // No provider ⇒ explicit failure. Returning a synthetic completion here would let a
        // plugin mistake a misconfigured node for a working model backend.
        let Some(gateway) = self.current_gateway() else {
            return Err(Status::unavailable(
                "No LLM provider is configured on this core; RequestLLM is disabled",
            ));
        };

        let mut messages = Vec::new();
        let mut temperature = None;
        let mut max_tokens = None;

        if let Some(ref params) = req.parameters {
            if let Some(kanon_proto::prost_types::value::Kind::StringValue(s)) = params
                .fields
                .get("system_prompt")
                .and_then(|v| v.kind.as_ref())
            {
                messages.push(ChatMessage::system(s));
            }
            if let Some(kanon_proto::prost_types::value::Kind::NumberValue(n)) = params
                .fields
                .get("temperature")
                .and_then(|v| v.kind.as_ref())
            {
                temperature = Some(*n as f32);
            }
            if let Some(kanon_proto::prost_types::value::Kind::NumberValue(n)) = params
                .fields
                .get("max_tokens")
                .and_then(|v| v.kind.as_ref())
            {
                max_tokens = Some(*n as u32);
            }
        }

        messages.push(ChatMessage::user(req.prompt));

        let chat_req = ChatRequest {
            model: req.model,
            messages,
            tools: Vec::new(),
            temperature,
            max_tokens,
        };

        let stream = gateway
            .chat_stream(&chat_req)
            .await
            .map_err(|e| Status::internal(format!("LLM Gateway error: {e}")))?;

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut stream = stream;
            while let Some(chunk_res) = stream.next().await {
                match chunk_res {
                    Ok(chunk) => {
                        let proto_chunk = LlmChunk {
                            delta_text: chunk.delta_text,
                            is_finished: chunk.is_finished,
                        };
                        if tx.send(Ok(proto_chunk)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(Status::internal(format!("Stream chunk error: {e}"))))
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    /// Sets an embedded KV key-value pair.
    ///
    /// Centralized KV storage via gRPC is unsupported. Plugins should persist state
    /// locally within their dedicated `./data/plugins/<id>/` directory (e.g. SQLite / DuckDB)
    /// to avoid RPC data amplification.
    async fn set_storage(
        &self,
        _request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        Err(Status::unimplemented(
            "Centralized KV storage via gRPC is unsupported; plugins must persist locally in their dedicated data directory",
        ))
    }

    /// Retrieves an embedded KV key-value pair.
    ///
    /// Centralized KV storage via gRPC is unsupported. Plugins should persist state
    /// locally within their dedicated `./data/plugins/<id>/` directory (e.g. SQLite / DuckDB)
    /// to avoid RPC data amplification.
    async fn get_storage(
        &self,
        _request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        Err(Status::unimplemented(
            "Centralized KV storage via gRPC is unsupported; plugins must persist locally in their dedicated data directory",
        ))
    }
}

/// IPC Server hosting the Core microkernel services on `./run/core.sock`.
pub struct CoreIpcServer {
    /// Path to the Unix domain socket or loopback address file.
    socket_path: PathBuf,
    /// Core API service implementation.
    service: CoreApiService,
}

impl CoreIpcServer {
    /// Creates a new `CoreIpcServer` with the specified socket path and service instance.
    pub fn new(socket_path: impl Into<PathBuf>, service: CoreApiService) -> Self {
        Self {
            socket_path: socket_path.into(),
            service,
        }
    }

    /// Creates a `CoreIpcServer` bound to the default socket path (`./run/core.sock`).
    pub fn with_default_path(service: CoreApiService) -> Self {
        Self::new(core_socket_path(None), service)
    }

    /// Returns a reference to the bound socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Runs the Core IPC server until the provided shutdown signal resolves.
    ///
    /// Binds to the designated socket path via [`IpcListener`] and registers
    /// the [`BotApiServiceServer`]. On graceful termination, the socket file is cleaned up.
    pub async fn run<F>(
        self,
        shutdown_signal: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let socket_path = self.socket_path.clone();
        let listener = IpcListener::bind(&socket_path)?;
        tracing::info!(socket = %socket_path.display(), "Core IPC server listening");

        let incoming = listener.incoming();
        let service = BotApiServiceServer::new(self.service);

        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(incoming, shutdown_signal)
            .await?;

        // Clean up socket file upon server exit.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
            tracing::debug!(socket = %socket_path.display(), "Cleaned up Core socket file");
        }

        Ok(())
    }
}
