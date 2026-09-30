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

pub use api::Endpoints;
pub use config::{DISPLAY_NAME, PLATFORM, QqOfficialConfig};

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use async_trait::async_trait;
use kanon_core::{AdapterError, EventIngress, PlatformAdapter};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, audio_segment, image_segment,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use api::{Api, MediaKind, MediaSource};
use mapping::{Quote, QuoteStore};

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
                // The passive reply's `msg_id` already threads the answer to the user's message.
                Some(Segment::Reply(_)) => {}
                Some(Segment::Image(image)) => media.push(Media {
                    kind: MediaKind::Image,
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
                    source: self
                        .media_source(audio.source.as_ref().map(|source| match source {
                            audio_segment::Source::Url(url) => Source::Url(url),
                            audio_segment::Source::FilePath(path) => Source::Path(path),
                            audio_segment::Source::RawBytes(bytes) => Source::Bytes(bytes),
                        }))
                        .await?,
                }),
                Some(Segment::Custom(custom)) => {
                    return Err(self.delivery_error(format!(
                        "QQ Official cannot send custom segment '{}'",
                        custom.type_name
                    )));
                }
                None => return Err(self.delivery_error("message contains an empty segment")),
            }
        }
        let text = text.trim().to_owned();
        if text.is_empty() && media.is_empty() {
            return Err(self.delivery_error("cannot send an empty QQ Official message"));
        }

        let msg_id = Some(request.event_id.as_str()).filter(|id| !id.is_empty());
        let mut sent = Vec::new();
        if !text.is_empty() {
            let response = self
                .send_text(&api, scene, target, &text, markdown, msg_id)
                .await
                .map_err(|err| self.delivery_error(err))?;
            self.remember(&response, &text);
            sent.push(response);
        }
        for item in media {
            let placeholder = match item.kind {
                MediaKind::Image => "[image]",
                MediaKind::Voice => "[voice]",
            };
            let response = self
                .send_media(&api, scene, target, item, msg_id)
                .await
                .map_err(|err| self.delivery_error(err))?;
            self.remember(&response, placeholder);
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
            Some(Source::Path(path)) => tokio::fs::read(path)
                .await
                .map(MediaSource::Bytes)
                .map_err(|err| {
                    self.delivery_error(format!("cannot read attachment {path}: {err}"))
                }),
            Some(Source::Bytes(bytes)) => Ok(MediaSource::Bytes(bytes.to_vec())),
            None => Err(self.delivery_error("media segment has no source")),
        }
    }

    async fn send_text(
        &self,
        api: &Api,
        scene: &str,
        target: &str,
        text: &str,
        markdown: bool,
        msg_id: Option<&str>,
    ) -> Result<Value, String> {
        match scene {
            "group" | "c2c" => {
                let mut body = if markdown {
                    json!({"msg_type": 2, "markdown": {"content": text}})
                } else {
                    json!({"msg_type": 0, "content": text})
                };
                reply_fields(&mut body, msg_id, true);
                api.send_v2(scene, target, &body).await
            }
            _ => {
                let mut body = json!({"content": text});
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
                let file_info = api.upload(scene, target, item.kind, item.source).await?;
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
                    (MediaKind::Voice, _) => {
                        return Err("QQ guild channels cannot receive voice messages".into());
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
fn reply_fields(body: &mut Value, msg_id: Option<&str>, with_seq: bool) {
    if let Some(msg_id) = msg_id {
        body["msg_id"] = json!(msg_id);
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
