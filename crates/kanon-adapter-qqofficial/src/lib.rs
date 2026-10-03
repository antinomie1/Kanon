//! Built-in QQ Official Bot adapter (QQ Open Platform API v2).
//!
//! Inbound, one gateway task holds the bot's WebSocket session and pushes group, C2C, guild and
//! guild-DM messages into the core with Fast-ACK. Outbound, replies are sent over REST as passive
//! replies to the message that triggered them (`msg_id` = the event ID): QQ restricts messages a
//! bot starts on its own, so answering the user's message is what reliably gets delivered.
//!
//! Settings live in the `qqofficial` section of `data/system.json` and are hot-applied: saving
//! new credentials reconnects the gateway without a node restart.

pub mod api;
pub mod bind;
pub mod config;
mod gateway;
pub mod mapping;
pub mod voice;

pub use api::Endpoints;
pub use config::{DISPLAY_NAME, PLATFORM, QqOfficialConfig};

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use async_trait::async_trait;
use kanon_core::{AdapterError, Capability, EventIngress, PlatformAdapter};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, PipelineEventRequest, audio_segment,
    file_segment, image_segment, video_segment,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use api::{Api, MediaKind, MediaSource};
use mapping::{Quote, QuoteStore};

/// What this adapter implements through the generic adapter contract.
///
/// A public QQ bot only receives group messages that @-mention it, and group events carry openids
/// rather than names or roles, so it does not declare group observation, sender names or roles.
/// Media goes out through one upload of at most 20 MiB each: images, voice (converted to a format
/// QQ plays when needed, see [`voice`]), video, and named files (`file_type = 4`).
const CAPABILITIES: &[Capability] = &[
    Capability::QuoteReply,
    Capability::Acknowledge,
    Capability::BotJoin,
    Capability::FriendAdd,
    Capability::SendImage,
    Capability::SendVoice,
    Capability::SendVideo,
    Capability::SendFile,
];

/// How many recent messages are remembered for resolving quotes.
const QUOTE_MEMORY: usize = 2048;

/// Observable lifecycle of the gateway session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Disabled by configuration.
    Disabled,
    /// Fetching a token or opening the gateway session.
    Connecting,
    /// The session is ready; messages flow both ways.
    Connected,
    /// The session failed; the adapter retries unless QQ refused the bot's permissions.
    Disconnected,
    /// Stopped by the node.
    Stopped,
}

/// Live adapter state for the console, free of credentials.
#[derive(Debug, Clone, Serialize)]
pub struct QqOfficialStatus {
    /// Platform routing key.
    pub platform: String,
    /// Operator's requested enabled state.
    pub enabled: bool,
    /// Whether the gateway session is ready.
    pub connected: bool,
    /// Current lifecycle state.
    pub connection_state: ConnectionState,
    /// Whether an AppSecret is stored.
    pub secret_configured: bool,
    /// Bot name reported by the gateway on the current session.
    pub bot_name: Option<String>,
    /// Most recent actionable failure.
    pub last_error: Option<String>,
}

/// State shared by the adapter, its gateway task and deliveries.
pub(crate) struct Shared {
    config: QqOfficialConfig,
    status: QqOfficialStatus,
    /// REST client for the current credentials; `None` while disabled.
    api: Option<Arc<Api>>,
    /// The bot's own user ID from `READY`, used to drop its @-marker from guild messages.
    bot_id: String,
}

/// Serializes lifecycle changes; held across the await that stops an old gateway task.
#[derive(Default)]
struct Lifecycle {
    ingress: Option<EventIngress>,
    task: Option<JoinHandle<()>>,
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The built-in `qqofficial` platform adapter.
pub struct QqOfficialAdapter {
    /// Fixed endpoints for tests; `None` selects production or sandbox from the configuration.
    endpoints: Option<Endpoints>,
    state: Arc<RwLock<Shared>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    quotes: Arc<StdMutex<QuoteStore>>,
}

impl QqOfficialAdapter {
    /// Validates the configuration without any network I/O.
    pub fn new(config: QqOfficialConfig) -> Result<Self, AdapterError> {
        Self::build(config, None)
    }

    /// Like [`new`](Self::new), but talks to `endpoints` regardless of the sandbox setting.
    pub fn with_endpoints(
        config: QqOfficialConfig,
        endpoints: Endpoints,
    ) -> Result<Self, AdapterError> {
        Self::build(config, Some(endpoints))
    }

    fn build(config: QqOfficialConfig, endpoints: Option<Endpoints>) -> Result<Self, AdapterError> {
        let config = config.prepare().map_err(configuration_error)?;
        let status = QqOfficialStatus {
            platform: PLATFORM.into(),
            enabled: config.enabled,
            connected: false,
            connection_state: ConnectionState::Disabled,
            secret_configured: config.secret.is_some(),
            bot_name: None,
            last_error: None,
        };
        Ok(Self {
            endpoints,
            state: Arc::new(RwLock::new(Shared {
                config,
                status,
                api: None,
                bot_id: String::new(),
            })),
            lifecycle: Arc::new(Mutex::new(Lifecycle::default())),
            quotes: Arc::new(StdMutex::new(QuoteStore::new(QUOTE_MEMORY))),
        })
    }

    /// Current settings, including the secret, for persistence only.
    pub fn config(&self) -> QqOfficialConfig {
        self.read().config.clone()
    }

    /// Current credential-free status.
    pub fn status(&self) -> QqOfficialStatus {
        self.read().status.clone()
    }

    /// Validates and hot-applies settings, reconnecting the gateway with them.
    ///
    /// A rejected configuration leaves the running adapter untouched.
    pub async fn apply(&self, config: QqOfficialConfig) -> Result<(), AdapterError> {
        let config = config.prepare().map_err(configuration_error)?;
        let (state, lifecycle, endpoints, quotes) = self.parts();
        // Run to completion even if the HTTP caller goes away, so a half-applied change can never
        // leave the old gateway stopped without a new one started.
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            replace(&state, &mut lifecycle, endpoints, quotes, config).await;
        })
        .await
        .map_err(|_| configuration_error("QQ Official lifecycle task failed".into()))
    }

    fn parts(
        &self,
    ) -> (
        Arc<RwLock<Shared>>,
        Arc<Mutex<Lifecycle>>,
        Option<Endpoints>,
        Arc<StdMutex<QuoteStore>>,
    ) {
        (
            self.state.clone(),
            self.lifecycle.clone(),
            self.endpoints.clone(),
            self.quotes.clone(),
        )
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Shared> {
        self.state.read().expect("QQ Official state poisoned")
    }

    fn delivery_error(&self, reason: impl Into<String>) -> AdapterError {
        let reason = reason.into();
        self.state
            .write()
            .expect("QQ Official state poisoned")
            .status
            .last_error = Some(reason.clone());
        AdapterError::Delivery {
            platform: PLATFORM.into(),
            reason,
        }
    }
}

/// Stops the current gateway task and, when enabled and started, launches one for `config`.
async fn replace(
    state: &Arc<RwLock<Shared>>,
    lifecycle: &mut Lifecycle,
    endpoints: Option<Endpoints>,
    quotes: Arc<StdMutex<QuoteStore>>,
    config: QqOfficialConfig,
) {
    if let Some(task) = lifecycle.task.take() {
        // Await the abort: the old task must not publish status or ingest after this point.
        task.abort();
        let _ = task.await;
    }
    let api = match (&config.secret, config.enabled) {
        (Some(secret), true) => Some(Arc::new(Api::new(
            endpoints.unwrap_or_else(|| Endpoints::official(config.sandbox)),
            config.app_id.clone(),
            secret.clone(),
        ))),
        _ => None,
    };
    {
        let mut shared = state.write().expect("QQ Official state poisoned");
        shared.status.enabled = config.enabled;
        shared.status.secret_configured = config.secret.is_some();
        shared.status.connected = false;
        shared.status.bot_name = None;
        shared.status.last_error = None;
        shared.status.connection_state = if api.is_some() {
            ConnectionState::Connecting
        } else {
            ConnectionState::Disabled
        };
        shared.config = config;
        shared.api = api.clone();
        shared.bot_id.clear();
    }
    if let (Some(api), Some(ingress)) = (api, lifecycle.ingress.clone()) {
        lifecycle.task = Some(tokio::spawn(gateway::run(
            state.clone(),
            api,
            ingress,
            quotes,
        )));
    }
}

fn configuration_error(reason: String) -> AdapterError {
    AdapterError::Configuration {
        platform: PLATFORM.into(),
        reason,
    }
}

/// `msg_seq` must differ between replies to the same message; one process-wide counter is the
/// simplest source of distinct values.
fn next_msg_seq() -> u32 {
    static SEQ: AtomicU32 = AtomicU32::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed) % 65_535 + 1
}

/// One media item of an outbound message.
struct Media {
    kind: MediaKind,
    source: MediaSource,
    /// Name shown for a file; QQ requires one for `file_type = 4`.
    name: Option<String>,
}

impl Media {
    /// What an image segment really holds, judged by the MIME type it reports. A plugin may
    /// label a video or a document as an image segment; it is sent as what it is instead of
    /// failing as a broken picture.
    fn kind_for(mime_type: Option<&str>) -> MediaKind {
        match mime_type.map(str::to_ascii_lowercase) {
            None => MediaKind::Image,
            Some(mime) if mime.starts_with("image/") => MediaKind::Image,
            Some(mime) if mime.starts_with("video/") => MediaKind::Video,
            Some(_) => MediaKind::File,
        }
    }

    /// Text remembered for a quote of this item.
    fn placeholder(&self) -> String {
        match self.kind {
            MediaKind::Image => "[image]".into(),
            MediaKind::Voice => "[voice]".into(),
            MediaKind::Video => "[video]".into(),
            MediaKind::File => format!("[file:{}]", self.name.as_deref().unwrap_or("file")),
        }
    }
}

#[async_trait]
impl PlatformAdapter for QqOfficialAdapter {
    fn platform(&self) -> &str {
        PLATFORM
    }

    fn display_name(&self) -> &str {
        DISPLAY_NAME
    }

    fn is_connected(&self) -> bool {
        self.read().status.connected
    }

    async fn start(&self, ingress: EventIngress) -> Result<(), AdapterError> {
        let (state, lifecycle, endpoints, quotes) = self.parts();
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            lifecycle.ingress = Some(ingress);
            let config = state
                .read()
                .expect("QQ Official state poisoned")
                .config
                .clone();
            replace(&state, &mut lifecycle, endpoints, quotes, config).await;
        })
        .await
        .map_err(|_| configuration_error("QQ Official lifecycle task failed".into()))
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        let (state, lifecycle, _, _) = self.parts();
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            lifecycle.ingress = None;
            if let Some(task) = lifecycle.task.take() {
                task.abort();
                let _ = task.await;
            }
            let mut shared = state.write().expect("QQ Official state poisoned");
            shared.status.connected = false;
            shared.status.connection_state = ConnectionState::Stopped;
        })
        .await
        .map_err(|_| configuration_error("QQ Official lifecycle task failed".into()))
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    fn reply_message_limit(&self, request: &DeliverMessageRequest) -> usize {
        // QQ permits five passive replies per C2C/group message. Each attachment is sent as
        // another native message. Reserve a C2C typing reply even if its asynchronous call has
        // not finished yet; consulting completed calls here would race the acknowledgement.
        // https://github.com/tencent-connect/bot-docs/blob/main/docs/develop/api-v2/server-inter/message/send-receive/send.md
        let budget: usize = if request.event_id.is_empty() {
            1
        } else if request.channel_id.starts_with("group:") {
            5
        } else if request.channel_id.starts_with("c2c:") {
            if request.event_id.starts_with(mapping::EVENT_ID_PREFIX) {
                5
            } else {
                4
            }
        } else {
            // Guild channels have a per-second limit instead. Keep their existing single text
            // delivery rather than creating an unpaced burst; guild DMs remain conservative too.
            1
        };
        let media = request
            .segments
            .iter()
            .filter(|segment| {
                matches!(
                    segment.segment.as_ref(),
                    Some(
                        Segment::Image(_)
                            | Segment::Audio(_)
                            | Segment::Video(_)
                            | Segment::File(_)
                    ),
                )
            })
            .count();
        budget.saturating_sub(media).max(1)
    }

    /// Shows "typing…" in a C2C chat; QQ has no indicator for other chats.
    ///
    /// QQ counts the indicator as one of the few passive replies a message allows, which is why
    /// the reply policy leaves acknowledgements off unless an operator turns them on.
    async fn acknowledge(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let api = {
            let shared = self.read();
            match &shared.api {
                Some(api) => api.clone(),
                _ => return Ok(()),
            }
        };
        let Some(openid) = event.channel_id.strip_prefix("c2c:") else {
            return Ok(());
        };
        if event.event_id.is_empty() || event.event_id.starts_with(mapping::EVENT_ID_PREFIX) {
            return Ok(());
        }
        let mut body =
            json!({"msg_type": 6, "input_notify": {"input_type": 1, "input_second": 60}});
        reply_fields(&mut body, Some(&event.event_id), true);
        api.send_v2("c2c", openid, &body)
            .await
            .map(|_| ())
            .map_err(|err| self.delivery_error(format!("typing indicator failed: {err}")))
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        if request.platform != PLATFORM {
            return Err(self.delivery_error("delivery platform does not match this adapter"));
        }
        // Pin the client first: a reconfiguration during this delivery must not switch bots
        // halfway through a multi-part reply.
        let (api, markdown) = {
            let shared = self.read();
            match &shared.api {
                Some(api) => (api.clone(), shared.config.markdown),
                None => {
                    return Err(configuration_error(
                        "the QQ Official adapter is disabled".into(),
                    ));
                }
            }
        };
        let (scene, target) = request
            .channel_id
            .split_once(':')
            .filter(|(scene, target)| {
                matches!(*scene, "group" | "c2c" | "guild" | "guild_dm") && !target.is_empty()
            })
            .ok_or_else(|| {
                self.delivery_error(format!(
                    "unsupported QQ Official channel '{}'",
                    request.channel_id
                ))
            })?;

        let mut text = String::new();
        let mut media = Vec::new();
        let mut quote: Option<String> = None;
        for segment in &request.segments {
            match segment.segment.as_ref() {
                Some(Segment::Text(part)) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&part.content);
                }
                Some(Segment::Mention(mention)) => {
                    text.push('@');
                    text.push_str(if mention.display_name.is_empty() {
                        &mention.target_user_id
                    } else {
                        &mention.display_name
                    });
                }
                // Quoted natively on the text message; a gateway event is not a quotable message.
                Some(Segment::Reply(reply)) => {
                    quote = Some(reply.target_message_id.clone())
                        .filter(|id| !id.is_empty() && !id.starts_with(mapping::EVENT_ID_PREFIX));
                }
                Some(Segment::Image(image)) => media.push(Media {
                    kind: Media::kind_for(image.mime_type.as_deref()),
                    name: image
                        .filename
                        .clone()
                        .or_else(|| match image.source.as_ref() {
                            Some(image_segment::Source::FilePath(path)) => {
                                std::path::Path::new(path)
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                            }
                            _ => None,
                        }),
                    source: self
                        .media_source(image.source.as_ref().map(|source| match source {
                            image_segment::Source::Url(url) => Source::Url(url),
                            image_segment::Source::FilePath(path) => Source::Path(path),
                            image_segment::Source::RawBytes(bytes) => Source::Bytes(bytes),
                        }))
                        .await?,
                }),
                Some(Segment::Audio(audio)) => media.push(Media {
                    kind: MediaKind::Voice,
                    name: None,
                    source: self
                        .voice_source(
                            &api,
                            audio.source.as_ref().map(|source| match source {
                                audio_segment::Source::Url(url) => Source::Url(url),
                                audio_segment::Source::FilePath(path) => Source::Path(path),
                                audio_segment::Source::RawBytes(bytes) => Source::Bytes(bytes),
                            }),
                        )
                        .await?,
                }),
                Some(Segment::Video(video)) => media.push(Media {
                    kind: MediaKind::Video,
                    name: video.filename.clone(),
                    source: self
                        .media_source(video.source.as_ref().map(|source| match source {
                            video_segment::Source::Url(url) => Source::Url(url),
                            video_segment::Source::FilePath(path) => Source::Path(path),
                            video_segment::Source::RawBytes(bytes) => Source::Bytes(bytes),
                        }))
                        .await?,
                }),
                Some(Segment::File(file)) => {
                    if file.name.is_empty() {
                        return Err(self.delivery_error("file segment has no name"));
                    }
                    media.push(Media {
                        kind: MediaKind::File,
                        name: Some(file.name.clone()),
                        source: self
                            .media_source(file.source.as_ref().map(|source| match source {
                                file_segment::Source::Url(url) => Source::Url(url),
                                file_segment::Source::FilePath(path) => Source::Path(path),
                                file_segment::Source::RawBytes(bytes) => Source::Bytes(bytes),
                            }))
                            .await?,
                    });
                }
                // The open API has no plain emoji segment; failing beats sending the message
                // without it.
                Some(Segment::Face(_)) => {
                    return Err(self.delivery_error("QQ Official cannot send face segments"));
                }
                Some(Segment::Custom(custom)) => {
                    return Err(self.delivery_error(format!(
                        "QQ Official cannot send custom segment '{}'",
                        custom.type_name
                    )));
                }
                None => return Err(self.delivery_error("message contains an empty segment")),
            }
        }
        // Strip only surrounding blank lines. Spaces and tabs are content: a split reply line
        // may be indented code, and trimming them would flatten it.
        let text = text.trim_matches(['\r', '\n']).to_owned();
        let text = if text.trim().is_empty() {
            String::new()
        } else {
            text
        };
        if text.is_empty() && media.is_empty() {
            return Err(self.delivery_error("cannot send an empty QQ Official message"));
        }

        let msg_id = Some(request.event_id.as_str()).filter(|id| !id.is_empty());
        let mut sent = Vec::new();
        if !text.is_empty() {
            let response = self
                .send_text(
                    &api,
                    scene,
                    target,
                    &text,
                    markdown,
                    msg_id,
                    quote.as_deref(),
                )
                .await
                .map_err(|err| self.delivery_error(err))?;
            self.remember(&response, &text);
            sent.push(response);
        }
        for item in media {
            let placeholder = item.placeholder();
            let response = self
                .send_media(&api, scene, target, item, msg_id)
                .await
                .map_err(|err| self.delivery_error(err))?;
            self.remember(&response, &placeholder);
            sent.push(response);
        }

        let message_id = sent
            .last()
            .and_then(|response| response["id"].as_str())
            .unwrap_or_default()
            .to_owned();
        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }
}

/// A borrowed media source from an outbound segment.
enum Source<'a> {
    Url(&'a str),
    Path(&'a str),
    Bytes(&'a [u8]),
}

impl QqOfficialAdapter {
    /// Resolves a media source: public URLs pass through, local files are read here because the
    /// path means nothing to QQ's servers.
    async fn media_source(&self, source: Option<Source<'_>>) -> Result<MediaSource, AdapterError> {
        match source {
            Some(Source::Url(url)) => Ok(MediaSource::Url(url.to_owned())),
            Some(Source::Path(path)) => {
                let failed =
                    |err| self.delivery_error(format!("cannot read attachment {path}: {err}"));
                let file = tokio::fs::File::open(path).await.map_err(failed)?;
                let mut bytes = Vec::new();
                // Read one extra byte to distinguish an exact-limit file from an oversized one.
                // Limiting the reader also covers files that grow after opening or report no size.
                file.take((api::MAX_UPLOAD_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(failed)?;
                api::validate_upload_size(bytes.len()).map_err(|err| self.delivery_error(err))?;
                Ok(MediaSource::Bytes(bytes))
            }
            Some(Source::Bytes(bytes)) => {
                api::validate_upload_size(bytes.len()).map_err(|err| self.delivery_error(err))?;
                Ok(MediaSource::Bytes(bytes.to_vec()))
            }
            None => Err(self.delivery_error("media segment has no source")),
        }
    }

    /// Resolves a voice source to bytes QQ plays as a voice message.
    ///
    /// Unlike other media, a voice URL is downloaded here instead of being handed to QQ: whether
    /// QQ can play it depends on the bytes, and an Opus URL passed through would fail inside QQ
    /// without a reason. Conversion runs on a blocking thread because decoding is CPU-bound.
    async fn voice_source(
        &self,
        api: &Api,
        source: Option<Source<'_>>,
    ) -> Result<MediaSource, AdapterError> {
        let audio = match self.media_source(source).await? {
            MediaSource::Url(url) => api
                .download(&url, api::MAX_UPLOAD_BYTES)
                .await
                .map_err(|err| self.delivery_error(err))?,
            MediaSource::Bytes(bytes) => bytes,
        };
        tokio::task::spawn_blocking(move || voice::playable(audio))
            .await
            .map_err(|err| self.delivery_error(format!("voice conversion stopped: {err}")))?
            .map(MediaSource::Bytes)
            .map_err(|err| self.delivery_error(err))
    }

    async fn send_text(
        &self,
        api: &Api,
        scene: &str,
        target: &str,
        text: &str,
        markdown: bool,
        msg_id: Option<&str>,
        quote: Option<&str>,
    ) -> Result<Value, String> {
        // QQ cannot quote a message from a Markdown message, so a quote only rides on plain text.
        let reference = quote
            .filter(|_| !markdown || !matches!(scene, "group" | "c2c"))
            .map(|id| json!({"message_id": id}));
        match scene {
            "group" | "c2c" => {
                let mut body = if markdown {
                    json!({"msg_type": 2, "markdown": {"content": text}})
                } else {
                    json!({"msg_type": 0, "content": text})
                };
                if let Some(reference) = reference {
                    body["message_reference"] = reference;
                }
                reply_fields(&mut body, msg_id, true);
                api.send_v2(scene, target, &body).await
            }
            _ => {
                let mut body = json!({"content": text});
                if let Some(reference) = reference {
                    body["message_reference"] = reference;
                }
                reply_fields(&mut body, msg_id, false);
                guild_send(api, scene, target, &body).await
            }
        }
    }

    async fn send_media(
        &self,
        api: &Api,
        scene: &str,
        target: &str,
        item: Media,
        msg_id: Option<&str>,
    ) -> Result<Value, String> {
        match scene {
            "group" | "c2c" => {
                let file_info = api
                    .upload(scene, target, item.kind, item.source, item.name.as_deref())
                    .await?;
                let mut body = json!({"msg_type": 7, "media": {"file_info": file_info}});
                reply_fields(&mut body, msg_id, true);
                api.send_v2(scene, target, &body).await
            }
            _ => {
                // Guild messages take images by URL only; uploads exist for groups and C2C.
                let url = match (item.kind, item.source) {
                    (MediaKind::Image, MediaSource::Url(url)) => url,
                    (MediaKind::Image, MediaSource::Bytes(_)) => {
                        return Err(
                            "QQ guild channels only accept image URLs, not local files".into()
                        );
                    }
                    (kind, _) => {
                        return Err(format!(
                            "QQ guild channels only accept images, not {kind:?} attachments"
                        ));
                    }
                };
                let mut body = json!({"image": url});
                reply_fields(&mut body, msg_id, false);
                guild_send(api, scene, target, &body).await
            }
        }
    }

    /// Remembers a sent message under the key a user's quote of it will carry.
    fn remember(&self, response: &Value, text: &str) {
        let key = response["ext_info"]["ref_idx"]
            .as_str()
            .or_else(|| response["id"].as_str())
            .unwrap_or_default();
        self.quotes
            .lock()
            .expect("QQ Official quote store poisoned")
            .insert(
                key.to_owned(),
                Quote {
                    text: text.to_owned(),
                    images: Vec::new(),
                },
            );
    }
}

/// Adds the passive-reply fields; `msg_seq` only exists on the v2 group and C2C endpoints.
///
/// A reply to a message quotes it as `msg_id`; a reply to a gateway event (bot added, friend
/// added) quotes the event as `event_id`, which is the only way QQ accepts such a greeting.
fn reply_fields(body: &mut Value, msg_id: Option<&str>, with_seq: bool) {
    match msg_id.map(|id| id.strip_prefix(mapping::EVENT_ID_PREFIX).ok_or(id)) {
        Some(Ok(event_id)) => body["event_id"] = json!(event_id),
        Some(Err(msg_id)) => body["msg_id"] = json!(msg_id),
        None => {}
    }
    if with_seq {
        body["msg_seq"] = json!(next_msg_seq());
    }
}

async fn guild_send(api: &Api, scene: &str, target: &str, body: &Value) -> Result<Value, String> {
    if scene == "guild" {
        api.send_channel(target, body).await
    } else {
        api.send_dm(target, body).await
    }
}
