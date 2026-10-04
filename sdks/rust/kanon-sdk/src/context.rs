//! Plugin execution context, environment metadata, and the core IPC handle.
//!
//! Exposes access to isolated persistent directories, active plugin configurations, and the
//! [`CoreHandle`] through which a plugin calls back into the microkernel: sending messages,
//! asking the model or the agent, key-value storage, conversations, personas and rendering.

use std::path::PathBuf;
use std::time::Duration;

use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AppendConversationRequest, ConversationHistoryRequest, ConversationHistoryResponse,
    ConversationInfo, ConversationsRequest, DeletePersonaRequest, DeleteStorageRequest,
    DeliverMessageRequest, DeliverMessageResponse, GetStorageRequest, HistoryMessage, ImageSegment,
    IngestEventRequest, IngestEventResponse, ListPersonasRequest, ListStorageRequest, LlmChunk,
    LlmMessage, LlmRequest, LlmRole, MessageSegment, Persona, PipelineEventRequest,
    PlatformApiRequest, RefreshPluginMetaRequest, RegisterHostRequest, RegisterHostResponse,
    RenderImageRequest, SelectConversationRequest, SendMessageRequest, SendMessageResponse,
    SetStorageRequest, image_segment, render_image_request,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tonic::Streaming;
use tonic::transport::Channel;

use crate::agent::AgentRequest;
use crate::error::CoreError;
use crate::event::{CommandEvent, MessageEvent};
use crate::segment::IntoReply;

/// Largest value the core stores under one key, in bytes of encoded JSON.
pub const MAX_KV_VALUE_BYTES: usize = 1024 * 1024;

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

/// Anything that names an inbound message: the raw [`PipelineEventRequest`], a
/// [`MessageEvent`] or a [`CommandEvent`].
///
/// Core calls about "this chat" (conversations, history, an agent run in the chat) take one,
/// because the core resolves the chat exactly as it does when answering that message.
pub trait AsEvent {
    /// The underlying pipeline event.
    fn as_event(&self) -> &PipelineEventRequest;
}

impl AsEvent for PipelineEventRequest {
    fn as_event(&self) -> &PipelineEventRequest {
        self
    }
}

impl AsEvent for MessageEvent {
    fn as_event(&self) -> &PipelineEventRequest {
        self.raw()
    }
}

impl AsEvent for CommandEvent {
    fn as_event(&self) -> &PipelineEventRequest {
        self.raw()
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
///
/// Calls scoped to the plugin (key-value storage, agent runs, rendering, metadata refresh) need
/// the plugin's identity; the host runner sets it. A handle made with [`CoreHandle::new`] alone
/// rejects those calls with [`CoreError::InvalidArgument`] instead of sending an anonymous
/// request.
#[derive(Debug, Clone)]
pub struct CoreHandle {
    client: BotApiServiceClient<kanon_transport::AuthenticatedChannel>,
    plugin_id: String,
    host_id: String,
}

impl CoreHandle {
    /// Wraps an established core channel, without a plugin identity.
    pub fn new(channel: Channel) -> Self {
        Self {
            client: BotApiServiceClient::with_interceptor(
                channel,
                kanon_transport::ClientAuthInterceptor(
                    std::env::var("KANON_IPC_TOKEN").unwrap_or_default(),
                ),
            ),
            plugin_id: String::new(),
            host_id: String::new(),
        }
    }

    /// Sets the plugin this handle acts for and the host it runs in (`KANON_HOST_ID`).
    pub fn with_identity(
        mut self,
        plugin_id: impl Into<String>,
        host_id: impl Into<String>,
    ) -> Self {
        self.plugin_id = plugin_id.into();
        self.host_id = host_id.into();
        self
    }

    /// The plugin this handle acts for; empty without an identity.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// The host this handle's plugin runs in; empty without an identity.
    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    /// The plugin id, or an error naming the call that needs it.
    pub(crate) fn require_plugin_id(&self, call: &str) -> Result<String, CoreError> {
        if self.plugin_id.is_empty() {
            return Err(CoreError::invalid(format!(
                "{call} needs the plugin's identity; build the handle with CoreHandle::with_identity"
            )));
        }
        Ok(self.plugin_id.clone())
    }

    fn client(&self) -> BotApiServiceClient<kanon_transport::AuthenticatedChannel> {
        self.client.clone()
    }

    /// Pushes an inbound event into the core pipeline and returns the Fast-ACK acknowledgement.
    ///
    /// The returned [`IngestEventResponse`] carries `accepted`: when the core's ingest queue is
    /// saturated it answers `accepted = false` instead of blocking, and the adapter decides
    /// whether to retry or drop. Transport failures surface as errors — this method never
    /// reports success for a message the core did not accept.
    pub async fn ingest_event(
        &self,
        request: IngestEventRequest,
    ) -> Result<IngestEventResponse, CoreError> {
        Ok(self.client().ingest_event(request).await?.into_inner())
    }

    /// Answers a liveness probe from the core.
    ///
    /// Deliberately trivial: it opens no business path, so a host can distinguish "core is gone"
    /// from "core is busy". Used by [`crate::watchdog::watch_core`].
    pub async fn ping(&self) -> Result<(), CoreError> {
        let request = kanon_proto::v1::PingRequest {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default(),
        };
        self.client().ping(request).await?;
        Ok(())
    }

    /// Registers the hosting process with the core (`BotApiService.RegisterHost`).
    ///
    /// Used by the host runner during startup; exposed because registration is also the cheapest
    /// proof that the core endpoint is actually serving.
    pub async fn register_host(
        &self,
        request: RegisterHostRequest,
    ) -> Result<RegisterHostResponse, CoreError> {
        Ok(self.client().register_host(request).await?.into_inner())
    }

    /// Replies to an inbound event and waits for the platform's delivery result
    /// (`BotApiService.ReplyMessage`).
    ///
    /// Success here is the adapter's delivery outcome, not mere queue admission. An RPC failure
    /// or timeout ([`CoreError::DeadlineExceeded`]) is ambiguous — the message may have gone
    /// out — so never retry a reply automatically.
    pub async fn reply_to(
        &self,
        event: &impl AsEvent,
        content: impl IntoReply,
    ) -> Result<DeliverMessageResponse, CoreError> {
        let event = event.as_event();
        let mut request = tonic::Request::new(DeliverMessageRequest {
            platform: event.platform.clone(),
            channel_id: event.channel_id.clone(),
            recipient_id: event.sender_id.clone(),
            event_id: event.event_id.clone(),
            segments: content.into_segments(),
        });
        // Slightly above Core's own delivery budget, so Core reports the outcome first.
        request.set_timeout(Duration::from_secs(35));
        Ok(self.client().reply_message(request).await?.into_inner())
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
    ) -> Result<SendMessageResponse, CoreError> {
        let request = SendMessageRequest {
            platform: platform.into(),
            channel_id: channel_id.into(),
            recipient_id: String::new(),
            segments: content.into_segments(),
        };
        Ok(self.client().send_message(request).await?.into_inner())
    }

    /// Asks the node's model one question and returns the complete answer.
    ///
    /// The call is independent of every chat conversation: nothing is read from or written to
    /// any session's memory. Build richer requests (system prompt, history, images, sampling)
    /// with [`LlmRequest`] and [`llm_message`], and pass them to [`stream_llm`](Self::stream_llm).
    /// For tools, or to answer inside a chat's conversation, use [`agent`](Self::agent).
    ///
    /// Fails with [`CoreError::Unavailable`] when the node has no model configured and
    /// [`CoreError::InvalidArgument`] for messages the model cannot take.
    pub async fn request_llm(&self, prompt: impl Into<String>) -> Result<String, CoreError> {
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
    ///
    /// Images may come from any source, raw bytes included (with their `mime_type`); images
    /// without a source, or raw bytes without a MIME type, are rejected before sending.
    pub async fn stream_llm(&self, request: LlmRequest) -> Result<Streaming<LlmChunk>, CoreError> {
        for message in &request.messages {
            message.images.iter().try_for_each(validate_image)?;
        }
        Ok(self.client().request_llm(request).await?.into_inner())
    }

    /// Calls one action of a built-in adapter's platform API and returns its result
    /// (`BotApiService.CallPlatformApi`).
    ///
    /// This reaches what the generic contract does not model, e.g. OneBot's
    /// `get_group_member_list`. `params` must be a JSON object (or `null` for none).
    /// Fails with [`CoreError::Unexpected`] for a non-finite result number,
    /// [`CoreError::NotFound`] for an unknown platform,
    /// [`CoreError::Unimplemented`] when the adapter offers no API and
    /// [`CoreError::Unavailable`] when the platform refused the call.
    pub async fn call_platform_api(
        &self,
        platform: impl Into<String>,
        action: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CoreError> {
        let params = match params {
            serde_json::Value::Object(fields) => Some(crate::json::to_struct(fields)),
            serde_json::Value::Null => None,
            _ => {
                return Err(CoreError::invalid(
                    "platform API parameters must be a JSON object",
                ));
            }
        };
        let request = PlatformApiRequest {
            platform: platform.into(),
            action: action.into(),
            params,
        };
        let response = self.client().call_platform_api(request).await?;
        Ok(response
            .into_inner()
            .result
            .map(crate::json::from_value)
            .transpose()
            .map_err(|error| CoreError::Unexpected(error.to_string()))?
            .unwrap_or(serde_json::Value::Null))
    }

    /// Reads the model conversation `event` belongs to (`BotApiService.GetConversationHistory`):
    /// the same session the model would continue when answering it.
    ///
    /// `limit` keeps only the most recent messages (0 keeps all). Only user and assistant turns
    /// are returned; tool calls, tool results and the model's reasoning are left out. History
    /// is read-only. Fails with [`CoreError::NotFound`] when no bot instance answers on the
    /// platform and [`CoreError::Unavailable`] when no model is configured.
    pub async fn conversation_history(
        &self,
        event: &impl AsEvent,
        limit: u32,
    ) -> Result<ConversationHistoryResponse, CoreError> {
        let request = ConversationHistoryRequest {
            context: Some(event.as_event().clone()),
            limit,
        };
        Ok(self
            .client()
            .get_conversation_history(request)
            .await?
            .into_inner())
    }

    /// Convenience wrapper building a text-only event for `platform`.
    ///
    /// The event is ingested without a caller-assigned identifier; build an
    /// [`IngestEventRequest`] and call [`ingest_event`](Self::ingest_event) to set one.
    pub async fn ingest_text(
        &self,
        platform: impl Into<String>,
        channel_id: impl Into<String>,
        sender_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<IngestEventResponse, CoreError> {
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

    // --- Key-value storage -----------------------------------------------------------------

    /// Reads `key` from the plugin's namespace of the core's key-value store, decoding the JSON
    /// it was stored as. `None` when the key is absent or expired.
    ///
    /// A stored value that does not decode as `T` is a [`CoreError::Json`], never `None`.
    pub async fn kv_get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, CoreError> {
        let request = GetStorageRequest {
            plugin_id: self.require_plugin_id("kv_get")?,
            key: validate_key(key)?,
        };
        let response = self.client().get_storage(request).await?.into_inner();
        if !response.found {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&response.value)?))
    }

    /// Stores `value` as JSON under `key`, without expiry, replacing any previous value. A value
    /// over [`MAX_KV_VALUE_BYTES`] is rejected before sending.
    pub async fn kv_set<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), CoreError> {
        self.store(key, value, 0).await
    }

    /// Stores `value` as JSON under `key` until `ttl` has passed; afterwards the key reads as
    /// absent. `ttl` is counted in whole seconds and must be at least one second (a shorter
    /// one would round to "never expires").
    pub async fn kv_set_with_ttl<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), CoreError> {
        let seconds =
            i64::try_from(ttl.as_secs()).map_err(|_| CoreError::invalid("the TTL is too long"))?;
        if seconds == 0 {
            return Err(CoreError::invalid("the TTL must be at least one second"));
        }
        self.store(key, value, seconds).await
    }

    async fn store<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl_seconds: i64,
    ) -> Result<(), CoreError> {
        let value = serde_json::to_vec(value)?;
        // The core would refuse it too; checking here saves the round trip of a large value.
        if value.len() > MAX_KV_VALUE_BYTES {
            return Err(CoreError::invalid(format!(
                "the value of '{key}' is {} bytes; the limit is {MAX_KV_VALUE_BYTES}",
                value.len()
            )));
        }
        let request = SetStorageRequest {
            plugin_id: self.require_plugin_id("kv_set")?,
            key: validate_key(key)?,
            value,
            ttl_seconds,
        };
        let response = self.client().set_storage(request).await?.into_inner();
        if !response.success {
            return Err(CoreError::Unexpected(format!(
                "the core did not store key '{key}'"
            )));
        }
        Ok(())
    }

    /// Removes `key`; returns whether a live key was removed.
    pub async fn kv_delete(&self, key: &str) -> Result<bool, CoreError> {
        let request = DeleteStorageRequest {
            plugin_id: self.require_plugin_id("kv_delete")?,
            key: validate_key(key)?,
        };
        Ok(self
            .client()
            .delete_storage(request)
            .await?
            .into_inner()
            .deleted)
    }

    /// The plugin's live keys starting with `prefix` (`""` for all of them), sorted.
    pub async fn kv_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let request = ListStorageRequest {
            plugin_id: self.require_plugin_id("kv_keys")?,
            prefix: prefix.to_string(),
        };
        Ok(self.client().list_storage(request).await?.into_inner().keys)
    }

    // --- Agent -------------------------------------------------------------------------------

    /// Prepares a run of the node's agent (model plus tool loop) for `prompt`; configure it with
    /// the builder methods and `.await` it.
    ///
    /// ```ignore
    /// let reply = core.agent("Summarize the discussion").event(&event).use_tools().await?;
    /// ```
    pub fn agent(&self, prompt: impl Into<String>) -> AgentRequest {
        AgentRequest::new(Some(self.clone()), prompt.into())
    }

    // --- Conversations -----------------------------------------------------------------------

    /// The conversations of `event`'s chat, oldest first (what `/ls` lists).
    pub async fn list_conversations(
        &self,
        event: &impl AsEvent,
    ) -> Result<Vec<ConversationInfo>, CoreError> {
        let request = ConversationsRequest {
            context: Some(event.as_event().clone()),
        };
        Ok(self
            .client()
            .list_conversations(request)
            .await?
            .into_inner()
            .conversations)
    }

    /// Starts a new, empty conversation in `event`'s chat and makes it current (what `/new`
    /// does); returns the chat's conversations afterwards.
    pub async fn new_conversation(
        &self,
        event: &impl AsEvent,
    ) -> Result<Vec<ConversationInfo>, CoreError> {
        let request = ConversationsRequest {
            context: Some(event.as_event().clone()),
        };
        Ok(self
            .client()
            .new_conversation(request)
            .await?
            .into_inner()
            .conversations)
    }

    /// Makes the conversation `session_id` of `event`'s chat current; returns the chat's
    /// conversations afterwards. [`CoreError::NotFound`] for an unknown session.
    pub async fn switch_conversation(
        &self,
        event: &impl AsEvent,
        session_id: &str,
    ) -> Result<Vec<ConversationInfo>, CoreError> {
        let request = select_request(event, session_id)?;
        Ok(self
            .client()
            .switch_conversation(request)
            .await?
            .into_inner()
            .conversations)
    }

    /// Deletes the conversation `session_id` of `event`'s chat (history and records); deleting
    /// the current one leaves the chat on a new, empty conversation. Returns the chat's
    /// conversations afterwards. [`CoreError::NotFound`] for an unknown session.
    pub async fn delete_conversation(
        &self,
        event: &impl AsEvent,
        session_id: &str,
    ) -> Result<Vec<ConversationInfo>, CoreError> {
        let request = select_request(event, session_id)?;
        Ok(self
            .client()
            .delete_conversation(request)
            .await?
            .into_inner()
            .conversations)
    }

    /// Appends finished `(user, assistant)` turns to the current conversation of `event`'s chat,
    /// e.g. a command exchange the model should remember; returns the conversation's session id.
    ///
    /// Taking pairs makes the core's rule — turns alternate user/assistant, starting with the
    /// user and ending with the assistant — impossible to break. History is append-only:
    /// nothing already stored changes.
    pub async fn append_conversation<U, A>(
        &self,
        event: &impl AsEvent,
        turns: impl IntoIterator<Item = (U, A)>,
    ) -> Result<String, CoreError>
    where
        U: Into<String>,
        A: Into<String>,
    {
        let messages: Vec<HistoryMessage> = turns
            .into_iter()
            .flat_map(|(user, assistant)| {
                [
                    history_message(LlmRole::User, user.into()),
                    history_message(LlmRole::Assistant, assistant.into()),
                ]
            })
            .collect();
        if messages.is_empty() {
            return Err(CoreError::invalid(
                "append_conversation needs at least one turn",
            ));
        }
        let request = AppendConversationRequest {
            context: Some(event.as_event().clone()),
            messages,
        };
        Ok(self
            .client()
            .append_conversation(request)
            .await?
            .into_inner()
            .session_id)
    }

    // --- Personas ----------------------------------------------------------------------------

    /// The operator's persona catalog.
    pub async fn list_personas(&self) -> Result<Vec<Persona>, CoreError> {
        Ok(self
            .client()
            .list_personas(ListPersonasRequest {})
            .await?
            .into_inner()
            .personas)
    }

    /// Creates or replaces the persona `persona.id`; returns whether one was replaced.
    ///
    /// `id` and `prompt` must not be empty; the built-in persona cannot be changed
    /// ([`CoreError::FailedPrecondition`]).
    pub async fn upsert_persona(&self, persona: Persona) -> Result<bool, CoreError> {
        if persona.id.trim().is_empty() || persona.prompt.trim().is_empty() {
            return Err(CoreError::invalid("a persona needs an id and a prompt"));
        }
        Ok(self
            .client()
            .upsert_persona(persona)
            .await?
            .into_inner()
            .replaced)
    }

    /// Deletes the persona `id`; returns whether it existed.
    pub async fn delete_persona(&self, id: &str) -> Result<bool, CoreError> {
        if id.trim().is_empty() {
            return Err(CoreError::invalid("a persona id must not be empty"));
        }
        let request = DeletePersonaRequest { id: id.to_string() };
        Ok(self
            .client()
            .delete_persona(request)
            .await?
            .into_inner()
            .deleted)
    }

    // --- Rendering ---------------------------------------------------------------------------

    /// Renders `text` as a card image (720 px wide) and returns it as an image segment, ready to
    /// send. Blank lines separate paragraphs and a line starting with `# ` is a heading.
    pub async fn render_text(&self, text: impl Into<String>) -> Result<MessageSegment, CoreError> {
        self.render(render_image_request::Source::Text(text.into()), 0)
            .await
    }

    /// Like [`render_text`](Self::render_text), `width` pixels wide (200 to 2000).
    pub async fn render_text_width(
        &self,
        text: impl Into<String>,
        width: u32,
    ) -> Result<MessageSegment, CoreError> {
        if !(200..=2000).contains(&width) {
            return Err(CoreError::invalid(format!(
                "render width {width} is outside 200..=2000"
            )));
        }
        self.render(render_image_request::Source::Text(text.into()), width)
            .await
    }

    /// Renders a complete SVG document to PNG and returns it as an image segment.
    pub async fn render_svg(&self, svg: impl Into<String>) -> Result<MessageSegment, CoreError> {
        self.render(render_image_request::Source::Svg(svg.into()), 0)
            .await
    }

    async fn render(
        &self,
        source: render_image_request::Source,
        width: u32,
    ) -> Result<MessageSegment, CoreError> {
        let (render_image_request::Source::Text(input) | render_image_request::Source::Svg(input)) =
            &source;
        if input.trim().is_empty() {
            return Err(CoreError::invalid("nothing to render: the input is empty"));
        }
        let request = RenderImageRequest {
            plugin_id: self.require_plugin_id("render")?,
            source: Some(source),
            width,
        };
        let response = self.client().render_image(request).await?.into_inner();
        if response.file_path.is_empty() {
            return Err(CoreError::Unexpected(
                "the core rendered no file".to_string(),
            ));
        }
        Ok(MessageSegment {
            segment: Some(Segment::Image(ImageSegment {
                source: Some(image_segment::Source::FilePath(response.file_path)),
                mime_type: Some("image/png".to_string()),
                filename: None,
            })),
        })
    }

    // --- Runtime metadata --------------------------------------------------------------------

    /// Asks the core to fetch this host's plugin metadata again, so tools, commands and
    /// triggers changed at runtime take effect for the next turn; returns the plugins whose
    /// metadata the core now holds.
    ///
    /// [`ContextSlot::add_tool`](crate::router::ContextSlot::add_tool) calls this for you.
    pub async fn refresh_plugin_meta(&self) -> Result<Vec<String>, CoreError> {
        if self.host_id.is_empty() {
            return Err(CoreError::invalid(
                "refresh_plugin_meta needs the host's identity; build the handle with CoreHandle::with_identity",
            ));
        }
        let request = RefreshPluginMetaRequest {
            host_id: self.host_id.clone(),
        };
        Ok(self
            .client()
            .refresh_plugin_meta(request)
            .await?
            .into_inner()
            .plugin_ids)
    }

    /// Sends a prepared agent run.
    pub(crate) async fn run_agent(
        &self,
        request: kanon_proto::v1::RunAgentRequest,
    ) -> Result<kanon_proto::v1::RunAgentResponse, CoreError> {
        Ok(self.client().run_agent(request).await?.into_inner())
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

fn history_message(role: LlmRole, text: String) -> HistoryMessage {
    HistoryMessage {
        role: role as i32,
        text,
    }
}

fn select_request(
    event: &impl AsEvent,
    session_id: &str,
) -> Result<SelectConversationRequest, CoreError> {
    if session_id.is_empty() {
        return Err(CoreError::invalid("a session id must not be empty"));
    }
    Ok(SelectConversationRequest {
        context: Some(event.as_event().clone()),
        session_id: session_id.to_string(),
    })
}

fn validate_key(key: &str) -> Result<String, CoreError> {
    if key.is_empty() {
        return Err(CoreError::invalid("a storage key must not be empty"));
    }
    Ok(key.to_string())
}

/// Rejects an image the model could not read: one without a source, or raw bytes without the
/// MIME type that says how to decode them.
pub(crate) fn validate_image(image: &ImageSegment) -> Result<(), CoreError> {
    match &image.source {
        None => Err(CoreError::invalid("an image has no source")),
        Some(image_segment::Source::RawBytes(_))
            if image.mime_type.as_deref().is_none_or(str::is_empty) =>
        {
            Err(CoreError::invalid("raw image bytes need a mime_type"))
        }
        Some(_) => Ok(()),
    }
}
