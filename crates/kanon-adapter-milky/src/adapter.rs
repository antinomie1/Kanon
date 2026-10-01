//! The Milky platform adapter: Kanon's in-process bridge to a Milky protocol implementation.
//!
//! # Inbound
//! [`PlatformAdapter::start`] receives the core's Fast-ACK ingest handle and spawns one event
//! stream (SSE or WebSocket, per configuration) whose decoded events are translated by
//! [`crate::mapping`] and pushed into that queue with a non-blocking `try_send`. The adapter never
//! awaits downstream reasoning, so a slow model cannot make the QQ connection look dead.
//!
//! # Outbound
//! [`PlatformAdapter::deliver`] resolves the Kanon `channel_id` back into a Milky conversation and
//! calls `send_group_message` or `send_private_message`. Every failure path — a disabled adapter,
//! an undecodable channel identifier, a segment Kanon can express but Milky cannot — returns an
//! [`AdapterError`] instead of reporting a delivery that never happened.
//!
//! # Why the runtime lives behind one lock
//! Configuration is hot-reloadable, so the client and the stream task are replaced while requests
//! may be in flight. Both are held in a single [`RwLock`] guarded state that is only ever locked
//! for the duration of a clone or a field assignment: the guard is never held across an `await`,
//! which is what keeps `is_connected` synchronous for the registry while `apply` stays async.
//!
//! # Non-message events
//! Milky pushes 21 event types; only `message_receive` is a conversational turn. A member joining,
//! a nudge of the bot and a recall are ingested as core notices (see `kanon_core::notice`), whose
//! node-wide event policy decides whether the bot reacts; friend requests and group invitations
//! are accepted when the configuration says so. Everything else is counted and reflected in the
//! status only.

use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kanon_core::{AdapterError, Capability, EventIngress, PlatformAdapter};
use kanon_proto::v1::{DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest};
use serde::Serialize;

use crate::client::{MilkyClient, MilkyError};
use crate::config::{ConfigError, MilkyConfig, TransportKind};
use crate::event_source::{EventSource, EventSourceHandle, StreamEvent};
use crate::mapping::{self, ChannelScene};
use crate::protocol::{
    AcceptFriendRequestInput, AcceptGroupInvitationInput, Event, GetForwardedMessagesInput,
    GetGroupMemberInfoInput, GetUserProfileInput, SendGroupMessageInput,
    SendGroupMessageReactionInput, SendPrivateMessageInput, UploadGroupFileInput,
    UploadPrivateFileInput,
};
use kanon_proto::v1::PipelineEventRequest;

/// What this adapter implements through the generic adapter contract.
const CAPABILITIES: &[Capability] = &[
    Capability::SenderName,
    Capability::SenderRole,
    Capability::GroupMessages,
    Capability::QuoteReply,
    Capability::ForwardContent,
    Capability::Acknowledge,
    Capability::MemberJoin,
    Capability::BotJoin,
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

/// Reaction used to acknowledge a message: QQ face 76, the thumbs-up.
const ACK_REACTION: &str = "76";

/// Connection state of the adapter, as shown in the management console.
///
/// Reported as a state machine rather than a boolean because "disabled by an operator" and "cannot
/// reach the protocol implementation" demand completely different reactions, and collapsing them
/// into `connected: false` would hide which one is happening.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// No configuration is enabled; the adapter holds no connection by design.
    #[default]
    Disabled,
    /// A stream is being established or re-established.
    Connecting,
    /// The event stream is up and events are arriving.
    Connected,
    /// The last attempt failed; the transport keeps retrying with backoff.
    Error,
}

/// Login information of the account the protocol implementation is signed in as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilkyLogin {
    /// Logged-in QQ number.
    pub uin: i64,
    /// Logged-in nickname.
    pub nickname: String,
}

/// Description of the protocol implementation behind the endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilkyImplementation {
    /// Protocol implementation name (for example `Lagrange.Milky`).
    pub impl_name: String,
    /// Protocol implementation version.
    pub impl_version: String,
    /// QQ protocol version the implementation reports.
    pub qq_protocol_version: String,
    /// QQ protocol platform the implementation reports.
    pub qq_protocol_type: String,
    /// Milky protocol version the implementation implements.
    pub milky_version: String,
}

/// Point-in-time status of the adapter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MilkyStatus {
    /// Platform identifier owned by the adapter.
    pub platform: String,
    /// Console-facing adapter name.
    pub display_name: String,
    /// Whether the stored configuration asks for a connection.
    pub enabled: bool,
    /// Base URL of the protocol implementation.
    pub base_url: String,
    /// Configured inbound transport.
    pub transport: TransportKind,
    /// Whether an `access_token` is stored (the value itself is never reported).
    pub token_configured: bool,
    /// Current connection state.
    pub state: ConnectionState,
    /// Convenience flag derived from [`MilkyStatus::state`].
    pub connected: bool,
    /// Reason of the most recent failure, cleared once a connection succeeds.
    pub last_error: Option<String>,
    /// Number of protocol events seen since the node started.
    pub events_received: u64,
    /// Number of messages accepted into the core pipeline.
    pub messages_ingested: u64,
    /// Number of messages the core refused because its ingest queue was saturated or closed.
    pub messages_rejected: u64,
    /// Number of messages successfully delivered to the platform.
    pub messages_delivered: u64,
    /// Unix timestamp in milliseconds of the most recent protocol event.
    pub last_event_at_unix_ms: Option<u64>,
    /// Cached login information, once known.
    pub login: Option<MilkyLogin>,
    /// Cached protocol implementation description, once known.
    pub implementation: Option<MilkyImplementation>,
}

/// Result of an explicit connectivity test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilkyTestReport {
    /// Round-trip time of the two probe calls, in milliseconds.
    pub latency_ms: u64,
    /// Login information reported by the endpoint.
    pub login: MilkyLogin,
    /// Implementation description reported by the endpoint.
    pub implementation: MilkyImplementation,
}

/// Mutable connection-scoped state, protected by one lock.
struct State {
    /// Stored configuration (the credential included; never reported verbatim).
    config: MilkyConfig,
    /// Client used for outbound calls, present while a configuration is enabled and installed.
    client: Option<Arc<MilkyClient>>,
    /// Running event stream, present while a stream is installed.
    source: Option<EventSourceHandle>,
    /// Fast-ACK ingest handle, captured when the core starts the adapter.
    ingress: Option<EventIngress>,
    /// Live status.
    status: StatusData,
}

/// Counters and cached identity behind [`MilkyStatus`].
#[derive(Debug, Default)]
struct StatusData {
    /// Current connection state.
    state: ConnectionState,
    /// Most recent failure reason.
    last_error: Option<String>,
    /// Protocol events seen.
    events_received: u64,
    /// Messages accepted by the core.
    messages_ingested: u64,
    /// Messages refused by the core.
    messages_rejected: u64,
    /// Messages delivered to the platform.
    messages_delivered: u64,
    /// Unix timestamp in milliseconds of the most recent event.
    last_event_at_unix_ms: Option<u64>,
    /// Cached login information.
    login: Option<MilkyLogin>,
    /// Cached implementation description.
    implementation: Option<MilkyImplementation>,
}

/// Kanon platform adapter for the Milky protocol.
pub struct MilkyAdapter {
    /// Platform identifier owned by the adapter, fixed for the process lifetime.
    platform: String,
    /// Console-facing name, fixed for the process lifetime because the registry reports `&str`.
    display_name: String,
    /// Connection state shared with the event pump task.
    state: Arc<RwLock<State>>,
}

impl MilkyAdapter {
    /// Builds an adapter for a configuration.
    ///
    /// The configuration is validated here even when it is disabled, so a node can never start
    /// holding a configuration that would fail the moment an operator switches it on.
    pub fn new(config: MilkyConfig) -> Result<Self, AdapterError> {
        let platform = config.platform.trim().to_string();
        let config = config
            .prepare()
            .map_err(|err| configuration_error(&platform, err))?;
        let display_name = config.effective_display_name();

        Ok(Self {
            platform,
            display_name,
            state: Arc::new(RwLock::new(State {
                config,
                client: None,
                source: None,
                ingress: None,
                status: StatusData::default(),
            })),
        })
    }

    /// Returns a copy of the stored configuration, credential included.
    pub fn config(&self) -> MilkyConfig {
        self.state
            .read()
            .expect("adapter state poisoned")
            .config
            .clone()
    }

    /// Returns the platform identifier owned by this adapter.
    pub fn identity(&self) -> &str {
        &self.platform
    }

    /// Builds the current status report.
    pub fn status(&self) -> MilkyStatus {
        let state = self.state.read().expect("adapter state poisoned");
        let status = &state.status;

        MilkyStatus {
            platform: self.platform.clone(),
            display_name: self.display_name.clone(),
            enabled: state.config.enabled,
            base_url: state.config.base_url.clone(),
            transport: state.config.transport,
            token_configured: state.config.token_configured(),
            state: status.state,
            connected: status.state == ConnectionState::Connected,
            last_error: status.last_error.clone(),
            events_received: status.events_received,
            messages_ingested: status.messages_ingested,
            messages_rejected: status.messages_rejected,
            messages_delivered: status.messages_delivered,
            last_event_at_unix_ms: status.last_event_at_unix_ms,
            login: status.login.clone(),
            implementation: status.implementation.clone(),
        }
    }

    /// Installs a new configuration and makes it effective immediately.
    ///
    /// The old stream is stopped *before* the new configuration is stored, so an in-flight event
    /// from the previous connection can never be attributed to the new one. Identity fields are
    /// rejected rather than ignored: the registry routes by platform and reports the display name
    /// through `&str` accessors, so changing either would leave the catalog describing an adapter
    /// that no longer exists until the next restart.
    pub async fn apply(&self, config: MilkyConfig) -> Result<MilkyStatus, AdapterError> {
        let config = config
            .prepare()
            .map_err(|err| configuration_error(&self.platform, err))?;

        if config.platform != self.platform {
            return Err(AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: format!(
                    "the platform identifier is fixed once the adapter is registered ('{}'); restart the node to change it",
                    self.platform
                ),
            });
        }
        if config.effective_display_name() != self.display_name {
            return Err(AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: format!(
                    "the display name is fixed once the adapter is registered ('{}'); restart the node to change it",
                    self.display_name
                ),
            });
        }

        let previous = {
            let mut state = self.state.write().expect("adapter state poisoned");
            state.source.take()
        };
        if let Some(previous) = previous {
            previous.shutdown().await;
        }

        {
            let mut state = self.state.write().expect("adapter state poisoned");
            state.config = config;
            // Connection-scoped state is discarded; the counters are lifetime metrics of the
            // adapter and survive a reconfiguration on purpose.
            state.client = None;
            state.status.login = None;
            state.status.implementation = None;
            state.status.last_error = None;
            state.status.state = if state.config.enabled {
                ConnectionState::Connecting
            } else {
                ConnectionState::Disabled
            };
        }

        self.install_runtime();

        Ok(self.status())
    }

    /// Disables the adapter, releasing its connection.
    pub async fn disable(&self) -> Result<MilkyStatus, AdapterError> {
        let mut config = self.config();
        config.enabled = false;
        self.apply(config).await
    }

    /// Probes an endpoint and reports what answered.
    ///
    /// A configuration may be supplied so the console can test values *before* saving them; when it
    /// is absent the stored configuration is probed. The probe never installs anything, so a failed
    /// test cannot disturb a working adapter.
    pub async fn test_connection(
        &self,
        candidate: Option<MilkyConfig>,
    ) -> Result<MilkyTestReport, AdapterError> {
        let config = match candidate {
            Some(candidate) => candidate
                .prepare()
                .map_err(|err| configuration_error(&self.platform, err))?,
            None => self.config(),
        };

        let client = MilkyClient::new(config).map_err(|err| AdapterError::Configuration {
            platform: self.platform.clone(),
            reason: err.to_string(),
        })?;

        let started = Instant::now();
        let (login, implementation) = probe_identity(&client)
            .await
            .map_err(|err| err.into_adapter_error(&self.platform))?;

        Ok(MilkyTestReport {
            latency_ms: started.elapsed().as_millis() as u64,
            login,
            implementation,
        })
    }

    /// Creates the client and stream for the stored configuration when one is missing.
    ///
    /// Called by both [`PlatformAdapter::start`] and [`MilkyAdapter::apply`], so a configuration
    /// saved before the core handed over its ingest handle still comes up as soon as it does.
    fn install_runtime(&self) {
        let mut state = self.state.write().expect("adapter state poisoned");

        if !state.config.enabled || state.source.is_some() {
            return;
        }
        let Some(ingress) = state.ingress.clone() else {
            // Without an ingest queue the core cannot receive what the stream would deliver; the
            // adapter stays in `Connecting` until `start` supplies the handle.
            return;
        };

        let config = state.config.clone();

        let client = match MilkyClient::new(config.clone()) {
            Ok(client) => Arc::new(client),
            Err(err) => {
                state.status.state = ConnectionState::Error;
                state.status.last_error = Some(err.to_string());
                return;
            }
        };
        let source = match EventSource::new(config) {
            Ok(source) => source,
            Err(err) => {
                state.status.state = ConnectionState::Error;
                state.status.last_error = Some(err.to_string());
                return;
            }
        };

        let (handle, receiver) = source.spawn();
        state.client = Some(client);
        state.source = Some(handle);
        state.status.state = ConnectionState::Connecting;
        state.status.last_error = None;
        drop(state);

        tokio::spawn(pump(
            self.state.clone(),
            self.platform.clone(),
            ingress,
            receiver,
        ));
    }
}

#[async_trait]
impl PlatformAdapter for MilkyAdapter {
    /// Platform identifier owned by this adapter.
    fn platform(&self) -> &str {
        &self.platform
    }

    /// Console-facing name.
    fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Whether the event stream is currently up.
    fn is_connected(&self) -> bool {
        self.state
            .read()
            .expect("adapter state poisoned")
            .status
            .state
            == ConnectionState::Connected
    }

    /// Delivers one outbound message through the Milky API.
    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        let target = mapping::delivery_target(&request.channel_id).map_err(|err| {
            AdapterError::Delivery {
                platform: self.platform.clone(),
                reason: err.to_string(),
            }
        })?;

        let client = {
            self.state
                .read()
                .expect("adapter state poisoned")
                .client
                .clone()
        };
        let Some(client) = client else {
            return Err(AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled or not configured yet; enable it in the management console"
                    .to_string(),
            });
        };

        let delivery = mapping::outbound_delivery(&request.segments).map_err(|err| {
            AdapterError::Delivery {
                platform: self.platform.clone(),
                reason: err.to_string(),
            }
        })?;

        // Naming the segment kinds makes a delivery observable in the trace log without dumping
        // user content, which is what an operator needs when a platform rejects a payload.
        let segment_types: Vec<&str> = delivery
            .message
            .iter()
            .map(mapping::outbound_segment_type)
            .collect();
        let file_count = delivery.files.len();

        // The message goes first so text introducing a file arrives before it. A failure after it
        // was sent still fails the delivery: the operator must learn that a file never arrived.
        let mut message_id = String::new();
        if !delivery.message.is_empty() {
            let sent = match target.scene {
                ChannelScene::Group => {
                    let input = SendGroupMessageInput {
                        group_id: target.peer_id,
                        message: delivery.message,
                    };
                    client
                        .send_group_message(&input)
                        .await
                        .map(|out| out.message_seq)
                }
                ChannelScene::Friend => {
                    let input = SendPrivateMessageInput {
                        user_id: target.peer_id,
                        message: delivery.message,
                    };
                    client
                        .send_private_message(&input)
                        .await
                        .map(|out| out.message_seq)
                }
                // Rejected by `delivery_target` above; handled explicitly so a future scene cannot
                // silently fall through to a private message.
                ChannelScene::Temp => {
                    unreachable!("temporary conversations are rejected before send")
                }
            };
            match sent {
                Ok(message_seq) => message_id = message_seq.to_string(),
                Err(err) => {
                    return Err(record_failure(
                        &self.state,
                        err.into_adapter_error(&self.platform),
                    ));
                }
            }
        }

        // An upload yields a file id, not a message sequence, so it never becomes the message id.
        for file in delivery.files {
            let uploaded = match target.scene {
                ChannelScene::Group => client
                    .upload_group_file(&UploadGroupFileInput {
                        group_id: target.peer_id,
                        // The group's root folder, the protocol's own default.
                        parent_folder_id: "/".to_string(),
                        file_uri: file.uri,
                        file_name: file.name,
                    })
                    .await
                    .map(drop),
                ChannelScene::Friend => client
                    .upload_private_file(&UploadPrivateFileInput {
                        user_id: target.peer_id,
                        file_uri: file.uri,
                        file_name: file.name,
                    })
                    .await
                    .map(drop),
                ChannelScene::Temp => {
                    unreachable!("temporary conversations are rejected before send")
                }
            };
            if let Err(err) = uploaded {
                return Err(record_failure(
                    &self.state,
                    err.into_adapter_error(&self.platform),
                ));
            }
        }

        self.state
            .write()
            .expect("adapter state poisoned")
            .status
            .messages_delivered += 1;

        tracing::debug!(
            channel = %request.channel_id,
            segments = ?segment_types,
            files = file_count,
            message_id = %message_id,
            "Milky message delivered"
        );

        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    /// Passes the call to the Milky API endpoint of the same name.
    async fn call_api(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        let client = self
            .state
            .read()
            .expect("adapter state poisoned")
            .client
            .clone()
            .ok_or_else(|| AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled".to_string(),
            })?;
        client
            .call_raw(action, &params)
            .await
            .map_err(|error| AdapterError::Api {
                platform: self.platform.clone(),
                action: action.to_string(),
                reason: error.to_string(),
            })
    }

    /// Accepts a friend request or group invitation with the token its notice carried.
    async fn accept_request(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let client = self
            .state
            .read()
            .expect("adapter state poisoned")
            .client
            .clone()
            .ok_or_else(|| AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled".to_string(),
            })?;
        let input =
            mapping::accept_request_input(event).map_err(|reason| AdapterError::Delivery {
                platform: self.platform.clone(),
                reason,
            })?;
        let result = match input {
            mapping::AcceptRequest::Friend(initiator_uid) => {
                client
                    .accept_friend_request(&AcceptFriendRequestInput {
                        initiator_uid,
                        is_filtered: false,
                    })
                    .await
            }
            mapping::AcceptRequest::Group(group_id, invitation_seq) => {
                client
                    .accept_group_invitation(&AcceptGroupInvitationInput {
                        group_id,
                        invitation_seq,
                    })
                    .await
            }
        };
        result.map_err(|err| err.into_adapter_error(&self.platform))
    }

    /// Reacts to a group message the bot is about to answer.
    async fn acknowledge(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let client = {
            let state = self.state.read().expect("adapter state poisoned");
            match &state.client {
                Some(client) => client.clone(),
                _ => return Ok(()),
            }
        };
        let Ok(target) = mapping::parse_channel_id(&event.channel_id) else {
            return Ok(());
        };
        let message_seq = event
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.fields.get(mapping::META_MESSAGE_SEQ))
            .and_then(|value| match value.kind {
                Some(kanon_proto::prost_types::value::Kind::NumberValue(seq)) => Some(seq as i64),
                _ => None,
            });
        // Only a group message can carry a reaction; notices have no message to react to.
        let (ChannelScene::Group, Some(message_seq)) = (target.scene, message_seq) else {
            return Ok(());
        };
        client
            .send_group_message_reaction(&SendGroupMessageReactionInput {
                group_id: target.peer_id,
                message_seq,
                reaction: ACK_REACTION.to_string(),
                reaction_type: "face".to_string(),
                is_add: true,
            })
            .await
            .map_err(|err| err.into_adapter_error(&self.platform))
    }

    /// Captures the ingest handle and brings up the event stream.
    async fn start(&self, ingress: EventIngress) -> Result<(), AdapterError> {
        {
            let mut state = self.state.write().expect("adapter state poisoned");
            state.ingress = Some(ingress);
        }

        self.install_runtime();
        Ok(())
    }

    /// Releases the event stream.
    async fn stop(&self) -> Result<(), AdapterError> {
        let source = {
            let mut state = self.state.write().expect("adapter state poisoned");
            state.client = None;
            state.status.state = ConnectionState::Disabled;
            state.source.take()
        };

        if let Some(source) = source {
            source.shutdown().await;
        }
        Ok(())
    }
}

/// Forwards decoded stream events into the core pipeline until the stream goes away.
///
/// Runs as its own task so that the ingest path stays non-blocking: the adapter's `deliver` and
/// management calls never wait for event decoding, and a saturated core applies backpressure to
/// this task alone.
async fn pump(
    state: Arc<RwLock<State>>,
    platform: String,
    ingress: EventIngress,
    mut receiver: tokio::sync::mpsc::Receiver<StreamEvent>,
) {
    while let Some(item) = receiver.recv().await {
        match item {
            StreamEvent::Connected => {
                let client = {
                    let mut state = state.write().expect("adapter state poisoned");
                    state.status.state = ConnectionState::Connected;
                    state.status.last_error = None;
                    state.client.clone()
                };

                // Identity is refreshed on every (re)connection because a protocol implementation
                // may have been restarted with a different account in the meantime.
                if let Some(client) = client {
                    tokio::spawn(refresh_identity(state.clone(), client));
                }
            }
            StreamEvent::Disconnected(reason) => {
                let mut state = state.write().expect("adapter state poisoned");
                state.status.state = ConnectionState::Error;
                state.status.last_error = Some(reason);
            }
            StreamEvent::Event(event) => handle_event(&state, &platform, &ingress, &event),
        }
    }
}

/// Records one protocol event and ingests it when it is a conversational message.
fn handle_event(state: &Arc<RwLock<State>>, platform: &str, ingress: &EventIngress, event: &Event) {
    let event_type = event.event_type();
    let self_id = event.self_id();

    {
        let mut guard = state.write().expect("adapter state poisoned");
        guard.status.events_received += 1;
        guard.status.last_event_at_unix_ms = Some(now_unix_millis());
    }

    let client = state.read().expect("adapter state poisoned").client.clone();

    if let Some(notice) = mapping::map_notice(platform, event) {
        match client {
            Some(client) => {
                let (state, platform, ingress) =
                    (state.clone(), platform.to_string(), ingress.clone());
                tokio::spawn(async move {
                    let mut request = notice.event;
                    if let Some(actor) = notice.actor_id {
                        let name = display_name(&client, notice.group_id, actor)
                            .await
                            .unwrap_or_else(|| actor.to_string());
                        mapping::set_notice_actor(&mut request, &name);
                    }
                    ingest(&state, &platform, &ingress, request);
                });
            }
            None => ingest(state, platform, ingress, notice.event),
        }
        return;
    }

    if let Some(request) = mapping::map_request(platform, event) {
        ingest(state, platform, ingress, request);
        return;
    }

    let request = match mapping::inbound_message(platform, self_id, event) {
        Ok(Some(request)) => request,
        Ok(None) => {
            // Any other event: counted, deliberately not ingested.
            tracing::trace!(event_type, "Milky event observed but not ingested");
            return;
        }
        Err(err) => {
            let mut guard = state.write().expect("adapter state poisoned");
            guard.status.last_error = Some(err.to_string());
            tracing::warn!(event_type, error = %err, "Failed to translate Milky message");
            return;
        }
    };

    // A merged forward is fetched before the message goes in, so the model reads what was
    // forwarded instead of a title. Everything else is ingested at once, keeping its order.
    let forwards = mapping::forward_ids(&request);
    match client {
        Some(client) if !forwards.is_empty() => {
            let (state, platform, ingress) = (state.clone(), platform.to_string(), ingress.clone());
            tokio::spawn(async move {
                let mut request = request;
                for forward_id in forwards {
                    let fetched = client
                        .get_forwarded_messages(&GetForwardedMessagesInput {
                            forward_id: forward_id.clone(),
                        })
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|out| {
                            mapping::attach_forward(&mut request, &forward_id, &out.messages)
                        });
                    if let Err(err) = fetched {
                        // An expired forward must not drop the message that carried it.
                        tracing::warn!(error = %err, "Milky merged forward unavailable");
                    }
                }
                ingest(&state, &platform, &ingress, request);
            });
        }
        _ => ingest(state, platform, ingress, request),
    }
}

/// Group card, else nickname, of an account; `None` when the implementation cannot tell.
async fn display_name(client: &MilkyClient, group_id: Option<i64>, user_id: i64) -> Option<String> {
    let name = match group_id {
        Some(group_id) => {
            let member = client
                .get_group_member_info(&GetGroupMemberInfoInput {
                    group_id,
                    user_id,
                    no_cache: false,
                })
                .await
                .ok()?
                .member;
            if member.card.trim().is_empty() {
                member.nickname
            } else {
                member.card
            }
        }
        None => {
            client
                .get_user_profile(&GetUserProfileInput { user_id })
                .await
                .ok()?
                .nickname
        }
    };
    Some(name.trim().to_string()).filter(|name| !name.is_empty())
}

/// Pushes one event into the core without waiting for capacity.
fn ingest(
    state: &Arc<RwLock<State>>,
    platform: &str,
    ingress: &EventIngress,
    request: PipelineEventRequest,
) {
    let event_id = request.event_id.clone();
    let ingest = IngestEventRequest {
        platform: platform.to_string(),
        event: Some(request),
    };

    let mut guard = state.write().expect("adapter state poisoned");
    match ingress.try_ingest(ingest) {
        Ok(()) => {
            guard.status.messages_ingested += 1;
            tracing::debug!(event_id, "Milky message ingested");
        }
        Err(err) => {
            guard.status.messages_rejected += 1;
            // Backpressure is not a connection failure: the stream stays up and the platform's own
            // retry-free semantics mean the message is simply lost, so it is logged loudly.
            tracing::warn!(event_id, error = %err, "Core refused a Milky message");
        }
    }
}

/// Refreshes the cached login and implementation description after a connection.
async fn refresh_identity(state: Arc<RwLock<State>>, client: Arc<MilkyClient>) {
    match probe_identity(&client).await {
        Ok((login, implementation)) => {
            let mut guard = state.write().expect("adapter state poisoned");
            guard.status.login = Some(login);
            guard.status.implementation = Some(implementation);
        }
        Err(err) => {
            // The stream is up, so the endpoint is reachable; failing to describe it is a
            // diagnostic gap, not a connection failure, and must not flip the reported state.
            tracing::debug!(error = %err, "Failed to read Milky identity");
        }
    }
}

/// Reads the login information and implementation description from an endpoint.
async fn probe_identity(
    client: &MilkyClient,
) -> Result<(MilkyLogin, MilkyImplementation), MilkyError> {
    let login = client.get_login_info().await?;
    let implementation = client.get_impl_info().await?;

    Ok((
        MilkyLogin {
            uin: login.uin,
            nickname: login.nickname,
        },
        MilkyImplementation {
            impl_name: implementation.impl_name,
            impl_version: implementation.impl_version,
            qq_protocol_version: implementation.qq_protocol_version,
            qq_protocol_type: implementation.qq_protocol_type,
            milky_version: implementation.milky_version,
        },
    ))
}

/// Records a delivery failure in the status and returns the error unchanged.
///
/// Outbound failures are the one failure mode an operator cannot observe from the connection
/// state, so they are surfaced here as well as returned to the dispatcher.
fn record_failure(state: &Arc<RwLock<State>>, error: AdapterError) -> AdapterError {
    state
        .write()
        .expect("adapter state poisoned")
        .status
        .last_error = Some(error.to_string());
    error
}

/// Renders a configuration error in the adapter's error vocabulary.
fn configuration_error(platform: &str, error: ConfigError) -> AdapterError {
    AdapterError::Configuration {
        platform: platform.to_string(),
        reason: error.to_string(),
    }
}

/// Current Unix time in milliseconds, saturating at the epoch for a clock before 1970.
fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
