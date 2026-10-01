//! Plugin execution context, environment metadata, and the core IPC handle.
//!
//! Exposes access to isolated persistent directories, active plugin configurations, and — for
//! plugins acting as platform adapters — a [`CoreHandle`] that pushes inbound messages back into
//! the microkernel pipeline.

use std::path::PathBuf;

use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest, IngestEventResponse,
    LlmChunk, LlmMessage, LlmRequest, LlmRole, PipelineEventRequest, PlatformApiRequest,
    RegisterHostRequest, RegisterHostResponse, SendMessageRequest, SendMessageResponse,
};
use tonic::Streaming;
use tonic::transport::Channel;

use crate::segment::IntoReply;

/// Runtime context passed to plugins during initialization and invocation.
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// Dedicated directory for this plugin's local database and file storage (`./data/plugins/<id>/`).
    pub data_dir: PathBuf,
    /// Active dynamic configuration parameters provided by Core.
    pub config: Option<prost_types::Struct>,
    /// Handle to the Core microkernel, absent when the host runs standalone.
    ///
    /// Adapter plugins must capture this handle during `on_load` (the only hook receiving the
    /// context) and reuse it from their polling loops:
    /// ```ignore
    /// async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
    ///     self.core = ctx.core.clone();
    ///     Ok(())
    /// }
    /// ```
    pub core: Option<CoreHandle>,
}

impl PluginContext {
    /// Creates a new `PluginContext` for the given data directory.
    pub fn new(data_dir: PathBuf, config: Option<prost_types::Struct>) -> Self {
        Self {
            data_dir,
            config,
            core: None,
        }
    }

    /// Attaches a Core IPC handle, enabling inbound ingestion from this plugin.
    pub fn with_core(mut self, core: CoreHandle) -> Self {
        self.core = Some(core);
        self
    }
}

/// Cloneable handle for calling back into the Core microkernel (`BotApiService`).
///
/// A single gRPC channel is shared across every clone instead of dialing per call: plugin hosts
/// issue these calls from long-running loops (a chat platform poller, a socket reader), and
/// re-establishing a connection per message would dominate the cost of a text-only event.
///
/// Each call works on its own clone of the client. Cloning a tonic client only clones the
/// channel handle, and HTTP/2 multiplexes the calls, so a long model stream never blocks an
/// adapter's inbound ingestion.
#[derive(Debug, Clone)]
pub struct CoreHandle {
    client: BotApiServiceClient<Channel>,
}

/// `tonic::Status` is inherently large (it carries metadata and a boxed source), and boxing it
/// here would force every plugin author to unwrap a second layer for no benefit. The rest of the
/// workspace follows the same convention.
#[allow(clippy::result_large_err)]
impl CoreHandle {
    /// Wraps an established core channel.
    pub fn new(channel: Channel) -> Self {
        Self {
            client: BotApiServiceClient::new(channel),
        }
    }

    /// Pushes an inbound event into the core pipeline and returns the Fast-ACK acknowledgement.
    ///
    /// The returned [`IngestEventResponse`] carries `accepted`: when the core's ingest queue is
    /// saturated it answers `accepted = false` instead of blocking, and the adapter decides
    /// whether to retry or drop. Transport failures surface as `tonic::Status` errors — this
    /// method never reports success for a message the core did not accept.
    pub async fn ingest_event(
        &self,
        request: IngestEventRequest,
    ) -> Result<IngestEventResponse, tonic::Status> {
        let mut client = self.client.clone();
        let response = client.ingest_event(request).await?;
        Ok(response.into_inner())
    }

    /// Answers a liveness probe from the core.
    ///
    /// Deliberately trivial: it opens no business path, so a host can distinguish "core is gone"
    /// from "core is busy". Used by [`crate::watchdog::watch_core`].
    pub async fn ping(&self) -> Result<(), tonic::Status> {
        let mut client = self.client.clone();
        let request = kanon_proto::v1::PingRequest {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default(),
        };
        client.ping(request).await?;
        Ok(())
    }

    /// Registers the hosting process with the core (`BotApiService.RegisterHost`).
    ///
    /// Used by the host runner during startup; exposed because registration is also the cheapest
    /// proof that the core endpoint is actually serving.
    pub async fn register_host(
        &self,
        request: RegisterHostRequest,
    ) -> Result<RegisterHostResponse, tonic::Status> {
        let mut client = self.client.clone();
        let response = client.register_host(request).await?;
        Ok(response.into_inner())
    }

    /// Replies to an inbound event and waits for the platform's delivery result
    /// (`BotApiService.ReplyMessage`).
    ///
    /// Success here is the adapter's delivery outcome, not mere queue admission. An RPC failure
    /// or timeout is ambiguous — the message may have gone out — so never retry a reply
    /// automatically.
    pub async fn reply_to(
        &self,
        event: &PipelineEventRequest,
        content: impl IntoReply,
    ) -> Result<DeliverMessageResponse, tonic::Status> {
        let mut request = tonic::Request::new(DeliverMessageRequest {
            platform: event.platform.clone(),
            channel_id: event.channel_id.clone(),
            recipient_id: event.sender_id.clone(),
            event_id: event.event_id.clone(),
            segments: content.into_segments(),
        });
        // Slightly above Core's own delivery budget, so Core reports the outcome first.
        request.set_timeout(std::time::Duration::from_secs(35));
        Ok(self
            .client
            .clone()
            .reply_message(request)
            .await?
            .into_inner())
    }

    /// Sends a message on the bot's own initiative: reminders, broadcasts, subscriptions
    /// (`BotApiService.SendMessage`).
    ///
    /// Success means Core accepted the message into its outbound queue, not that the platform
    /// delivered it; use [`reply_to`](Self::reply_to) when the delivery outcome matters.
    /// `channel_id` is the conversation as events report it (`"group:123"`).
    pub async fn send_message(
        &self,
        platform: impl Into<String>,
        channel_id: impl Into<String>,
        content: impl IntoReply,
    ) -> Result<SendMessageResponse, tonic::Status> {
        let request = SendMessageRequest {
            platform: platform.into(),
            channel_id: channel_id.into(),
            recipient_id: String::new(),
            segments: content.into_segments(),
        };
        Ok(self
            .client
            .clone()
            .send_message(request)
            .await?
            .into_inner())
    }

    /// Asks the node's model one question and returns the complete answer.
    ///
    /// The call is independent of every chat conversation: nothing is read from or written to
    /// any session's memory. Build richer requests (system prompt, history, images, sampling)
    /// with [`LlmRequest`] and [`llm_message`], and pass them to [`stream_llm`](Self::stream_llm).
    ///
    /// Fails with `UNAVAILABLE` when the node has no model configured and `INVALID_ARGUMENT`
    /// for messages the model cannot take.
    pub async fn request_llm(&self, prompt: impl Into<String>) -> Result<String, tonic::Status> {
        let request = LlmRequest {
            messages: vec![llm_message(LlmRole::User, prompt)],
            ..Default::default()
        };
        let mut stream = self.stream_llm(request).await?;
        let mut answer = String::new();
        while let Some(chunk) = stream.message().await? {
            answer.push_str(&chunk.delta_text);
        }
        Ok(answer)
    }

    /// Starts a model call and returns its answer as a stream of text deltas.
    pub async fn stream_llm(
        &self,
        request: LlmRequest,
    ) -> Result<Streaming<LlmChunk>, tonic::Status> {
        Ok(self.client.clone().request_llm(request).await?.into_inner())
    }

    /// Calls one action of a built-in adapter's platform API and returns its result
    /// (`BotApiService.CallPlatformApi`).
    ///
    /// This reaches what the generic contract does not model, e.g. OneBot's
    /// `get_group_member_list`. `params` must be a JSON object (or `null` for none); numbers in
    /// the result come back as floats. Fails with `NOT_FOUND` for an unknown platform,
    /// `UNIMPLEMENTED` when the adapter offers no API and `UNAVAILABLE` when the platform
    /// refused the call.
    pub async fn call_platform_api(
        &self,
        platform: impl Into<String>,
        action: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, tonic::Status> {
        let params = match params {
            serde_json::Value::Object(fields) => Some(crate::json::to_struct(fields)),
            serde_json::Value::Null => None,
            _ => {
                return Err(tonic::Status::invalid_argument(
                    "platform API parameters must be a JSON object",
                ));
            }
        };
        let request = PlatformApiRequest {
            platform: platform.into(),
            action: action.into(),
            params,
        };
        let response = self.client.clone().call_platform_api(request).await?;
        Ok(response
            .into_inner()
            .result
            .map(crate::json::from_value)
            .unwrap_or(serde_json::Value::Null))
    }

    /// Convenience wrapper building a text-only event for `platform`.
    ///
    /// `event_id` is supplied by the caller because platform message identifiers (update ids,
    /// message ids) are the most useful trace keys; pass an empty string to let the core accept
    /// the event without a caller-assigned identifier.
    pub async fn ingest_text(
        &self,
        platform: impl Into<String>,
        channel_id: impl Into<String>,
        sender_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<IngestEventResponse, tonic::Status> {
        let platform = platform.into();
        let request = IngestEventRequest {
            platform: platform.clone(),
            event: Some(PipelineEventRequest {
                event_id: String::new(),
                platform,
                channel_id: channel_id.into(),
                sender_id: sender_id.into(),
                raw_text: text.into(),
                segments: Vec::new(),
                metadata: None,
            }),
        };

        self.ingest_event(request).await
    }
}

/// Builds one turn of an [`LlmRequest`]; attach images (user turns only) through `images`.
pub fn llm_message(role: LlmRole, text: impl Into<String>) -> LlmMessage {
    LlmMessage {
        role: role as i32,
        text: text.into(),
        images: Vec::new(),
    }
}
