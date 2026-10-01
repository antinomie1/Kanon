//! Platform adapter contract, built-in adapter registry, and the Fast-ACK ingest handle.
//!
//! # Why adapters exist
//! The microkernel is platform agnostic: it knows how to filter, route and reason, but it never
//! speaks any IM protocol. A **platform adapter** owns exactly one platform identifier (the same
//! string carried in `PipelineEventRequest.platform`) and is responsible for two directions:
//!
//! - **inbound**: pushing user messages into the core through [`EventIngress`], which preserves
//!   the Fast-ACK guarantee (non-blocking enqueue, explicit backpressure instead of blocking);
//! - **outbound**: turning a [`DeliverMessageRequest`] into an actual platform API call.
//!
//! # Two implementation routes, one contract
//! - **Built-in (in-process)**: a Rust type implementing [`PlatformAdapter`], registered on the
//!   [`AdapterRegistry`]. Zero IPC overhead, ideal for HTTP-facing bridges.
//! - **Plugin (out-of-process)**: a plugin whose static manifest declares an `[adapter]` platform.
//!   The supervisor discovers it from the manifest and routes outbound messages to the owning host
//!   through `MessagePipelineService.OnDeliverMessage`; inbound messages come back through
//!   `BotApiService.IngestEvent`. Both primitives already exist in the gRPC contract, so plugins
//!   need no core-side registration call.
//!
//! Adapters are resolved by platform on the outbound path only; the pipeline itself stays unaware
//! of which route served a message.
//!
//! # Capabilities
//! Platforms differ: one reports group roles, another cannot receive a group message nobody
//! @-mentioned. Every feature that depends on such a difference goes through one generic
//! contract — metadata keys, reply segments, [`PlatformAdapter::acknowledge`],
//! [`PlatformAdapter::accept_request`] — and every adapter declares which of them it honours as a
//! [`Capability`]. The catalog reports the declarations, so a console can say, for any setting,
//! which adapters it affects, and a new adapter only has to declare what it implements.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest, PipelineEventRequest,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};

use crate::supervisor::circuit_breaker::CircuitState;

/// A platform-dependent feature an adapter implements through the generic adapter contract.
///
/// Serialized in `snake_case`; plugin adapters declare the same names under `[adapter]
/// capabilities` in `plugin.toml`. A capability that needs a callback into the adapter
/// ([`Capability::Acknowledge`], [`Capability::FriendRequests`], [`Capability::GroupInvites`]) is
/// only available to built-in adapters, because the plugin protocol has no such call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Events carry the sender's display name (`kanon.sender_name`).
    SenderName,
    /// Group events carry the sender's role (`kanon.sender_role`: owner, admin or member).
    SenderRole,
    /// Every group message is delivered, not only the ones that @-mention the bot.
    GroupMessages,
    /// A reply segment is sent as a native quote of the message it names.
    QuoteReply,
    /// Merged forwards arrive with their content (`messages` in the forward payload).
    ForwardContent,
    /// [`PlatformAdapter::acknowledge`] shows that an answer is being prepared.
    Acknowledge,
    /// Reports members joining a group (`member_join` notice).
    MemberJoin,
    /// Reports the bot being added to a group (`bot_join` notice).
    BotJoin,
    /// Reports the bot being added as a friend (`friend_add` notice).
    FriendAdd,
    /// Reports pokes of the bot (`poke` notice).
    Poke,
    /// Reports recalled messages (`recall` notice).
    Recall,
    /// Reports friend requests and accepts them through [`PlatformAdapter::accept_request`].
    FriendRequests,
    /// Reports group invitations and accepts them through [`PlatformAdapter::accept_request`].
    GroupInvites,
    /// Passes plugin calls to the platform's own API through [`PlatformAdapter::call_api`].
    PlatformApi,
    /// Delivers image segments.
    SendImage,
    /// Delivers audio segments as playable voice messages.
    SendVoice,
    /// Delivers video segments.
    SendVideo,
    /// Delivers file segments of any type under the name they carry.
    SendFile,
}

impl Capability {
    /// Whether the capability needs a call into the adapter, which only built-in adapters offer.
    pub fn needs_callback(self) -> bool {
        matches!(
            self,
            Self::Acknowledge | Self::FriendRequests | Self::GroupInvites | Self::PlatformApi
        )
    }
}

/// Errors raised by a platform adapter or by adapter routing.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// The platform refused the message or the transport to it failed.
    #[error("adapter '{platform}' delivery failed: {reason}")]
    Delivery {
        /// Platform identifier that failed.
        platform: String,
        /// Underlying reason reported by the adapter or plugin host.
        reason: String,
    },
    /// The adapter is registered but not usable (e.g. a missing callback URL).
    #[error("adapter '{platform}' is not configured: {reason}")]
    Configuration {
        /// Platform identifier that is misconfigured.
        platform: String,
        /// What is missing or invalid.
        reason: String,
    },
    /// No built-in adapter and no plugin declares the requested platform.
    #[error("no adapter is registered for platform '{0}'")]
    UnknownPlatform(String),
    /// A platform identifier was declared twice, which would make routing ambiguous.
    #[error("platform '{0}' is already served by another adapter")]
    DuplicatePlatform(String),
    /// The adapter does not implement a capability the core asked it to use.
    #[error("adapter '{platform}' does not support {capability:?}")]
    Unsupported {
        /// Platform identifier asked for the capability.
        platform: String,
        /// The capability it lacks.
        capability: Capability,
    },
    /// The platform rejected a raw API call, or the call could not reach it.
    #[error("adapter '{platform}' API call '{action}' failed: {reason}")]
    Api {
        /// Platform identifier the call was made on.
        platform: String,
        /// Platform action that failed.
        action: String,
        /// Underlying reason reported by the adapter.
        reason: String,
    },
    /// Inbound payload verification (e.g. HMAC signature or bearer token) failed.
    #[error("adapter '{platform}' authentication failed: {reason}")]
    Authentication {
        /// Platform identifier where authentication failed.
        platform: String,
        /// Underlying reason reported by the verification logic.
        reason: String,
    },
}

/// Outcome of an inbound ingest attempt.
///
/// Both variants are actionable: `QueueFull` means the core is saturated (the caller should shed
/// load or retry later) and `Closed` means the pipeline worker is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IngestError {
    /// The ingest queue reached its high-watermark.
    #[error("ingest queue is full; the core is saturated")]
    QueueFull,
    /// The pipeline worker stopped; the core is shutting down.
    #[error("ingest queue is closed; the core is shutting down")]
    Closed,
}

/// Cloneable handle to the core's Fast-ACK ingest queue.
///
/// This is the *only* sanctioned inbound entry point: it performs a non-blocking `try_send` so an
/// adapter's network loop can acknowledge a platform webhook in microseconds instead of waiting
/// for downstream LLM reasoning (the lockstep-prevention rule from the architecture spec).
#[derive(Debug, Clone)]
pub struct EventIngress {
    sender: mpsc::Sender<IngestEventRequest>,
}

impl EventIngress {
    /// Wraps an existing bounded ingest channel.
    pub fn new(sender: mpsc::Sender<IngestEventRequest>) -> Self {
        Self { sender }
    }

    /// Enqueues an inbound event without awaiting capacity.
    pub fn try_ingest(&self, request: IngestEventRequest) -> Result<(), IngestError> {
        self.sender.try_send(request).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => IngestError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => IngestError::Closed,
        })
    }

    /// Enqueues an inbound event, awaiting capacity when the queue is saturated.
    ///
    /// Pull-style adapters (long polling / streaming sockets) use this variant because they own a
    /// dedicated task per platform and can afford to apply backpressure to their own loop.
    pub async fn ingest(&self, request: IngestEventRequest) -> Result<(), IngestError> {
        self.sender
            .send(request)
            .await
            .map_err(|_| IngestError::Closed)
    }

    /// Configured high-watermark of the ingest queue, useful for diagnostics.
    pub fn capacity(&self) -> usize {
        self.sender.max_capacity()
    }

    /// Returns `true` when the pipeline worker has stopped consuming events.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

impl From<mpsc::Sender<IngestEventRequest>> for EventIngress {
    fn from(sender: mpsc::Sender<IngestEventRequest>) -> Self {
        Self::new(sender)
    }
}

/// Origin of an adapter registration, reported to the management console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// In-process Rust adapter registered on the [`AdapterRegistry`].
    Builtin,
    /// Out-of-process plugin host whose manifest declares the platform.
    Plugin,
}

/// Console-facing description of a platform adapter.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterDescriptor {
    /// Platform identifier used for routing.
    pub platform: String,
    /// Human-readable name.
    pub display_name: String,
    /// Whether the adapter lives in-process or in a plugin host.
    pub kind: AdapterKind,
    /// Whether the adapter can currently deliver messages.
    pub connected: bool,
    /// Adaptive circuit breaker state for this platform's outbound delivery queue.
    pub circuit_state: CircuitState,
    /// Owning plugin identifier (plugin adapters only).
    pub plugin_id: Option<String>,
    /// Owning host process identifier (plugin adapters only).
    pub host_id: Option<String>,
    /// Features this adapter implements, sorted.
    pub capabilities: Vec<Capability>,
}

/// Contract implemented by in-process platform adapters.
///
/// Implementations must be cheap to share (`&self` methods only) because a single instance serves
/// every concurrent delivery for its platform.
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Platform identifier this adapter owns (e.g. `webhook`, `telegram`).
    fn platform(&self) -> &str;

    /// Human-readable name shown in the management console.
    fn display_name(&self) -> &str {
        self.platform()
    }

    /// Features this adapter implements through the generic contract.
    ///
    /// Declaring a capability is a promise: the core relies on it (for example, it only offers
    /// group observation where every group message is delivered) and the console reports it next to
    /// each setting that depends on it. The default declares nothing.
    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    /// Whether the adapter can currently accept outbound messages.
    ///
    /// Returning `false` is reported to consoles; it does not prevent routing, so a delivery
    /// attempt still yields an explicit error instead of a silent drop.
    fn is_connected(&self) -> bool {
        true
    }

    /// Maximum delivery requests into which one model reply may be split.
    ///
    /// The adapter must reserve its own native-message costs, including media and any concurrent
    /// acknowledgement. The core merges excess lines into the last request without discarding
    /// content. Returning one disables extra deliveries; the default imposes no limit.
    fn reply_message_limit(&self, _request: &DeliverMessageRequest) -> usize {
        usize::MAX
    }

    /// Delivers one outbound message to the platform.
    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError>;

    /// Tells the user their message is being worked on, before the model starts answering it.
    ///
    /// The pipeline calls this — without awaiting it — once it has decided to answer `event` with
    /// the model, so an adapter can show a typing indicator or react to the message. Only the
    /// adapter knows whether its platform and configuration support that; the default does nothing.
    /// A failure is logged by the caller and never delays or cancels the answer.
    async fn acknowledge(&self, _event: &PipelineEventRequest) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Accepts the friend request or group invitation a `friend_request` / `group_invite` notice
    /// reported, using the opaque token the adapter put in its metadata.
    ///
    /// Called by the core when the node's event policy accepts such requests automatically. An
    /// adapter that declares [`Capability::FriendRequests`] or [`Capability::GroupInvites`] must
    /// implement it; the default reports the capability as unsupported.
    async fn accept_request(&self, _event: &PipelineEventRequest) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            platform: self.platform().to_string(),
            capability: Capability::FriendRequests,
        })
    }

    /// Calls one action of the platform's own API with JSON parameters and returns its result.
    ///
    /// This is the escape hatch for platform features the generic contract does not model (group
    /// management, profile lookups, ...). The action name has already been checked by the core to
    /// be a plain identifier. An adapter that declares [`Capability::PlatformApi`] must implement
    /// it; the default reports the capability as unsupported.
    async fn call_api(
        &self,
        _action: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Unsupported {
            platform: self.platform().to_string(),
            capability: Capability::PlatformApi,
        })
    }

    /// Verifies the authenticity of an inbound payload (e.g. HMAC signature or webhook token).
    ///
    /// The default implementation accepts all payloads (`Ok(())`). Adapters that enforce cryptographic
    /// signatures (such as Webhook HMAC-SHA256) override this to reject forged payloads before
    /// they enter the core pipeline.
    fn verify_inbound(
        &self,
        _signature: Option<&str>,
        _payload: &[u8],
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Starts the adapter's inbound loop for push-less platforms (long polling, sockets).
    ///
    /// Push-style adapters (HTTP webhooks) need nothing here. A failure must be returned, not
    /// logged and swallowed, so the caller can decide whether the node is degraded.
    async fn start(&self, _ingress: EventIngress) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Releases platform resources during graceful shutdown.
    async fn stop(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Thread-safe registry of built-in (in-process) adapters.
///
/// Plugin adapters are deliberately *not* registered here: they are derived from the supervisor's
/// manifest view on demand, which means a crashed host can never leave a stale routing entry.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>,
}

impl AdapterRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a built-in adapter.
    ///
    /// Duplicate platform identifiers are rejected: silently replacing an adapter would make
    /// message routing depend on startup order.
    pub async fn register(&self, adapter: Arc<dyn PlatformAdapter>) -> Result<(), AdapterError> {
        let platform = adapter.platform().to_string();
        let mut adapters = self.adapters.write().await;

        if adapters.contains_key(&platform) {
            return Err(AdapterError::DuplicatePlatform(platform));
        }

        adapters.insert(platform, adapter);
        Ok(())
    }

    /// Removes a built-in adapter, returning it when it was present.
    pub async fn unregister(&self, platform: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.adapters.write().await.remove(platform)
    }

    /// Looks up a built-in adapter by platform.
    pub async fn get(&self, platform: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.adapters.read().await.get(platform).cloned()
    }

    /// Lists every registered built-in adapter.
    pub async fn list(&self) -> Vec<Arc<dyn PlatformAdapter>> {
        self.adapters.read().await.values().cloned().collect()
    }

    /// Number of registered built-in adapters.
    pub async fn len(&self) -> usize {
        self.adapters.read().await.len()
    }

    /// Returns `true` when no built-in adapter is registered.
    pub async fn is_empty(&self) -> bool {
        self.adapters.read().await.is_empty()
    }

    /// Starts every adapter's inbound loop, returning the platforms that failed to start.
    ///
    /// Failures are returned rather than logged so the composition root can surface them at
    /// startup; adapters that started successfully keep running.
    pub async fn start_all(&self, ingress: EventIngress) -> Vec<(String, AdapterError)> {
        let adapters = self.list().await;
        let mut failures = Vec::new();

        for adapter in adapters {
            if let Err(err) = adapter.start(ingress.clone()).await {
                failures.push((adapter.platform().to_string(), err));
            }
        }

        failures
    }

    /// Stops every adapter, returning the platforms that failed to stop cleanly.
    pub async fn stop_all(&self) -> Vec<(String, AdapterError)> {
        let adapters = self.list().await;
        let mut failures = Vec::new();

        for adapter in adapters {
            if let Err(err) = adapter.stop().await {
                failures.push((adapter.platform().to_string(), err));
            }
        }

        failures
    }
}
