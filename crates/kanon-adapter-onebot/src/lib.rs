//! OneBot v11 adapter with forward and reverse universal WebSocket connections.
//!
//! A single socket task owns API correlation and event reception. Ingest never waits on the
//! pipeline; API calls have deadlines and are failed on disconnect rather than replayed.

pub mod client;
pub mod config;
pub mod mapping;
pub mod protocol;
mod transport;

pub use client::{OneBotClient, OneBotError};
pub use config::{DEFAULT_PLATFORM, OneBotConfig, TransportKind};

use async_trait::async_trait;
use kanon_core::{AdapterError, Capability, EventIngress, PlatformAdapter};
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, PipelineEventRequest, audio_segment,
    file_segment, image_segment, message_segment::Segment, video_segment,
};
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

/// What this adapter implements through the generic adapter contract.
///
/// OneBot v11 has no standard way to show a typing indicator or react to a message, so it does
/// not acknowledge. Files go out as the `file` extension segment (NapCat, LLOneBot, Lagrange); an
/// implementation without it rejects the message, which surfaces as a delivery error.
const CAPABILITIES: &[Capability] = &[
    Capability::SenderName,
    Capability::SenderRole,
    Capability::GroupMessages,
    Capability::QuoteReply,
    Capability::ForwardContent,
    Capability::MemberJoin,
    Capability::BotJoin,
    Capability::FriendAdd,
    Capability::Poke,
    Capability::Recall,
    Capability::FriendRequests,
    Capability::GroupInvites,
    Capability::PlatformApi,
    Capability::SendImage,
    Capability::SendVoice,
    Capability::SendVideo,
    Capability::SendFile,
];

/// Observable lifecycle of the universal connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Disabled by configuration.
    Disabled,
    /// Connecting to a forward endpoint.
    Connecting,
    /// Reverse listener is waiting for an implementation.
    Listening,
    /// A universal socket is available for deliveries.
    Connected,
    /// A connection failed; forward mode will retry.
    Disconnected,
    /// Explicitly stopped by the node.
    Stopped,
}

/// Live connection details, without credentials or raw peer payloads.
#[derive(Debug, Clone, Serialize)]
pub struct OneBotStatus {
    /// Stable routing key.
    pub platform: String,
    /// Operator's requested enabled state.
    pub enabled: bool,
    /// Whether a live universal connection is available.
    pub connected: bool,
    /// Current lifecycle state.
    pub connection_state: ConnectionState,
    /// Whether a bearer token is configured.
    pub token_configured: bool,
    /// Account observed on the current connection.
    pub self_id: Option<String>,
    /// Most recent actionable failure.
    pub last_error: Option<String>,
}

/// Shared snapshots and the current session's bounded command queue.
struct State {
    config: OneBotConfig,
    status: OneBotStatus,
    sender: Option<mpsc::Sender<transport::Command>>,
}

/// Serializes lifecycle mutations independently of synchronous status and delivery reads.
#[derive(Default)]
struct Lifecycle {
    ingress: Option<EventIngress>,
    task: Option<JoinHandle<()>>,
    listener: Option<Arc<TcpListener>>,
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// One platform registration serving one OneBot account at a time.
pub struct OneBotAdapter {
    platform: String,
    display_name: String,
    state: Arc<RwLock<State>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
}

impl OneBotAdapter {
    /// Returns a typed API handle sharing this adapter's current and future connections.
    pub fn client(&self) -> OneBotClient {
        OneBotClient::new(self.state.clone())
    }

    /// Validates settings without opening network resources.
    pub fn new(config: OneBotConfig) -> Result<Self, AdapterError> {
        let config = config
            .clone()
            .prepare()
            .map_err(|reason| AdapterError::Configuration {
                platform: config.platform,
                reason,
            })?;
        let status = OneBotStatus {
            platform: config.platform.clone(),
            enabled: config.enabled,
            connected: false,
            connection_state: ConnectionState::Disabled,
            token_configured: config.access_token.is_some(),
            self_id: None,
            last_error: None,
        };
        Ok(Self {
            platform: config.platform.clone(),
            display_name: config.effective_display_name(),
            state: Arc::new(RwLock::new(State {
                config,
                status,
                sender: None,
            })),
            lifecycle: Arc::new(Mutex::new(Lifecycle::default())),
        })
    }

    /// Stable platform identifier.
    pub fn identity(&self) -> &str {
        &self.platform
    }

    /// Current settings, including the credential for persistence only.
    pub fn config(&self) -> OneBotConfig {
        self.state
            .read()
            .expect("OneBot state poisoned")
            .config
            .clone()
    }

    /// Current credential-free runtime snapshot.
    pub fn status(&self) -> OneBotStatus {
        self.state
            .read()
            .expect("OneBot state poisoned")
            .status
            .clone()
    }

    /// Hot-applies settings, retaining the live configuration if a new bind fails.
    pub async fn apply(&self, config: OneBotConfig) -> Result<(), AdapterError> {
        self.update_config(move |_| config, |_| Ok(())).await
    }

    /// Changes the current configuration, saving it before replacing the live connection.
    ///
    /// The lifecycle lock covers reading retained credentials, preparing a listener, persistence
    /// and publication. Once started, the operation finishes even if its caller disconnects.
    pub async fn update_config(
        &self,
        change: impl FnOnce(OneBotConfig) -> OneBotConfig + Send + 'static,
        persist: impl FnOnce(&OneBotConfig) -> Result<(), String> + Send + 'static,
    ) -> Result<(), AdapterError> {
        let lifecycle = self.lifecycle.clone();
        let state = self.state.clone();
        let platform = self.platform.clone();
        let display_name = self.display_name.clone();
        // The short mutation owns its state until it finishes, even if the HTTP caller leaves.
        // Lifecycle still owns and aborts the long-running transport when the adapter is dropped.
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            let current = state.read().expect("OneBot state poisoned").config.clone();
            let config =
                change(current)
                    .prepare()
                    .map_err(|reason| AdapterError::Configuration {
                        platform: platform.clone(),
                        reason,
                    })?;
            if config.platform != platform || config.effective_display_name() != display_name {
                return Err(AdapterError::Configuration {
                    platform,
                    reason: "platform and display_name changes require a restart".into(),
                });
            }
            Self::replace(&state, &mut lifecycle, config, persist).await
        })
        .await
        .map_err(|_| self.configuration_error("OneBot lifecycle task failed"))?
    }

    /// Acquires a new listener before stopping the old task; an unchanged bind reuses its socket.
    async fn replace(
        state: &Arc<RwLock<State>>,
        lifecycle: &mut Lifecycle,
        config: OneBotConfig,
        persist: impl FnOnce(&OneBotConfig) -> Result<(), String>,
    ) -> Result<(), AdapterError> {
        let configuration_error = |reason: String| AdapterError::Configuration {
            platform: config.platform.clone(),
            reason,
        };
        let listener = if config.enabled
            && lifecycle.ingress.is_some()
            && config.transport == TransportKind::ReverseWebsocket
        {
            let addr = config.listen_addr().map_err(&configuration_error)?;
            match &lifecycle.listener {
                Some(listener) if listener.local_addr().ok() == Some(addr) => {
                    Some(listener.clone())
                }
                _ => Some(Arc::new(TcpListener::bind(addr).await.map_err(|e| {
                    configuration_error(format!("cannot bind reverse WebSocket listener: {e}"))
                })?)),
            }
        } else {
            None
        };
        // Binding and saving may fail. Neither failure is allowed to stop the old connection.
        persist(&config).map_err(|reason| AdapterError::Persistence {
            platform: config.platform.clone(),
            reason,
        })?;
        if let Some(task) = lifecycle.task.take() {
            // Await cancellation before publishing a new generation; an old task can no longer
            // overwrite status, retain the listener, or deliver stale replies after this point.
            task.abort();
            let _ = task.await;
        }
        lifecycle.listener = listener.clone();
        {
            let mut state = state.write().expect("OneBot state poisoned");
            state.sender = None;
            state.config = config.clone();
            state.status.enabled = config.enabled;
            state.status.token_configured = config.access_token.is_some();
            state.status.connected = false;
            state.status.self_id = None;
            state.status.last_error = None;
            state.status.connection_state = if !config.enabled {
                ConnectionState::Disabled
            } else if lifecycle.ingress.is_none() {
                ConnectionState::Stopped
            } else if listener.is_some() {
                ConnectionState::Listening
            } else {
                ConnectionState::Connecting
            };
        }
        if config.enabled {
            if let Some(ingress) = lifecycle.ingress.clone() {
                lifecycle.task = Some(tokio::spawn(transport::run(
                    state.clone(),
                    config,
                    ingress,
                    listener,
                )));
            }
        }
        Ok(())
    }

    fn configuration_error(&self, reason: impl Into<String>) -> AdapterError {
        AdapterError::Configuration {
            platform: self.platform.clone(),
            reason: reason.into(),
        }
    }

    fn delivery_error(&self, reason: impl Into<String>) -> AdapterError {
        let reason = reason.into();
        self.state
            .write()
            .expect("OneBot state poisoned")
            .status
            .last_error = Some(reason.clone());
        AdapterError::Delivery {
            platform: self.platform.clone(),
            reason,
        }
    }
}

#[async_trait]
impl PlatformAdapter for OneBotAdapter {
    fn platform(&self) -> &str {
        &self.platform
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    fn is_connected(&self) -> bool {
        self.status().connected
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    /// Accepts a friend request or group invitation with the `flag` its notice carried.
    async fn accept_request(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let (action, params) =
            mapping::accept_request_call(event).map_err(|e| self.delivery_error(e))?;
        self.client()
            .call_void(action, &params)
            .await
            .map_err(|error| self.delivery_error(error.to_string()))
    }

    /// Passes the call to the OneBot action of the same name.
    ///
    /// Failures are returned to the calling plugin and not recorded as the adapter's last error:
    /// a plugin asking for an action the implementation lacks says nothing about the connection.
    async fn call_api(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        self.client()
            .call_raw(action, &params)
            .await
            .map_err(|error| AdapterError::Api {
                platform: self.platform.clone(),
                action: action.to_string(),
                reason: error.to_string(),
            })
    }

    async fn start(&self, ingress: EventIngress) -> Result<(), AdapterError> {
        let lifecycle = self.lifecycle.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            lifecycle.ingress = Some(ingress);
            let config = state.read().expect("OneBot state poisoned").config.clone();
            Self::replace(&state, &mut lifecycle, config, |_| Ok(())).await
        })
        .await
        .map_err(|_| self.configuration_error("OneBot lifecycle task failed"))?
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        let lifecycle = self.lifecycle.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            let mut lifecycle = lifecycle.lock().await;
            lifecycle.ingress = None;
            if let Some(task) = lifecycle.task.take() {
                task.abort();
                let _ = task.await;
            }
            lifecycle.listener = None;
            let mut state = state.write().expect("OneBot state poisoned");
            state.sender = None;
            state.status.connected = false;
            state.status.self_id = None;
            state.status.connection_state = ConnectionState::Stopped;
        })
        .await
        .map_err(|_| self.configuration_error("OneBot lifecycle task failed"))
    }

    async fn deliver(
        &self,
        mut request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        if request.platform != self.platform {
            return Err(self.delivery_error("delivery platform does not match this adapter"));
        }
        // Pin the current session before file I/O: a reconnect or account switch during
        // attachment loading must fail this delivery rather than send it from another account.
        let sender = self
            .client()
            .session()
            .map_err(|error| self.configuration_error(error.to_string()))?;
        // FilePath belongs to the Kanon host, which may differ from the OneBot host. Read
        // it here and use the protocol's base64 media form instead of forwarding a local path.
        for segment in &mut request.segments {
            match segment.segment.as_mut() {
                Some(Segment::Image(image)) => {
                    if let Some(image_segment::Source::FilePath(path)) = image.source.as_ref() {
                        let bytes = tokio::fs::read(path).await.map_err(|error| {
                            self.delivery_error(format!(
                                "cannot read OneBot image attachment: {error}"
                            ))
                        })?;
                        image.source = Some(image_segment::Source::RawBytes(bytes));
                    }
                }
                Some(Segment::Audio(audio)) => {
                    if let Some(audio_segment::Source::FilePath(path)) = audio.source.as_ref() {
                        let bytes = tokio::fs::read(path).await.map_err(|error| {
                            self.delivery_error(format!(
                                "cannot read OneBot audio attachment: {error}"
                            ))
                        })?;
                        audio.source = Some(audio_segment::Source::RawBytes(bytes));
                    }
                }
                Some(Segment::Video(video)) => {
                    if let Some(video_segment::Source::FilePath(path)) = video.source.as_ref() {
                        let bytes = tokio::fs::read(path).await.map_err(|error| {
                            self.delivery_error(format!(
                                "cannot read OneBot video attachment: {error}"
                            ))
                        })?;
                        video.source = Some(video_segment::Source::RawBytes(bytes));
                    }
                }
                Some(Segment::File(file)) => {
                    if let Some(file_segment::Source::FilePath(path)) = file.source.as_ref() {
                        let bytes = tokio::fs::read(path).await.map_err(|error| {
                            self.delivery_error(format!(
                                "cannot read OneBot file attachment: {error}"
                            ))
                        })?;
                        file.source = Some(file_segment::Source::RawBytes(bytes));
                    }
                }
                _ => {}
            }
        }
        let (action, params) = mapping::delivery(&request).map_err(|e| self.delivery_error(e))?;
        let result: protocol::SendMessageOutput = OneBotClient::call_on(sender, &action, &params)
            .await
            .map_err(|error| self.delivery_error(error.to_string()))?;
        let message_id = result.message_id.to_string();
        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }
}
