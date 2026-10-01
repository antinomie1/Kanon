//! Sub-process plugin host supervisor and lifecycle manager.
//!
//! Responsible for spawning child plugin processes, managing per-host IPC endpoints
//! (`./run/host_<id>.sock`), verifying socket readiness, conducting initial
//! `GetPluginMeta` handshakes, and ensuring graceful process termination and cleanup.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tonic::transport::Channel;

use crate::adapter::{AdapterDescriptor, AdapterKind, AdapterRegistry};
use crate::manifest::PluginManifest;
use crate::toggle::{PLUGIN_SECTION, ToggleStore};
use kanon_proto::v1::message_pipeline_service_client::MessagePipelineServiceClient;
use kanon_proto::v1::plugin_host_service_client::PluginHostServiceClient;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DecorateReplyRequest, DecorateReplyResult,
    DeliverMessageRequest, DeliverMessageResponse, EventNotification, GetPluginMetaRequest,
    PipelineEventRequest, PluginMeta, PreFilterResult, PrepareTurnRequest, PrepareTurnResult,
    ReloadPluginConfigRequest, ReloadPluginConfigResponse, ToolCallRequest, ToolCallResponse,
};
use kanon_transport::{connect_ipc, core_socket_path, default_run_dir, host_socket_path};

pub mod circuit_breaker;
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

/// Errors arising during supervisor operations.
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// Standard I/O failure when launching child processes or inspecting filesystem sockets.
    #[error("I/O error in supervisor: {0}")]
    Io(#[from] std::io::Error),
    /// gRPC transport error when dialing host endpoint.
    #[error("Transport error connecting to host: {0}")]
    Transport(#[from] tonic::transport::Error),
    /// gRPC status error returned during RPC execution.
    #[error("RPC error during host communication: {0}")]
    Rpc(Box<tonic::Status>),
    /// Host failed to become ready within the allocated deadline.
    #[error("Host '{0}' timed out waiting for socket readiness")]
    Timeout(String),
    /// Child process exited prematurely before completing handshake.
    #[error("Host process '{host_id}' exited prematurely with exit status: {status}")]
    PrematureExit { host_id: String, status: String },
    /// Manifest parsing failure when spawning from static file.
    #[error("Failed to load plugin manifest: {0}")]
    Manifest(String),
    /// Requested host was not found in the supervisor registry.
    #[error("Host '{0}' not found in supervisor")]
    HostNotFound(String),
    /// Requested plugin is not loaded by any active host.
    #[error("Plugin '{0}' is not loaded by any active host")]
    PluginNotFound(String),
    /// Host was registered without a launch specification (e.g. externally attached).
    ///
    /// Restart cannot be honoured because the microkernel never owned the process handle
    /// and therefore cannot faithfully reconstruct its command line.
    #[error("Host '{0}' has no recorded launch specification and cannot be restarted")]
    RestartUnavailable(String),
    /// Configuration payload was not a JSON object and cannot be mapped to `google.protobuf.Struct`.
    #[error("Plugin configuration payload must be a JSON object")]
    InvalidConfigPayload,
    /// Host explicitly rejected the configuration reload request.
    #[error("Host '{host_id}' rejected configuration reload for plugin '{plugin_id}': {reason}")]
    ConfigReloadRejected {
        host_id: String,
        plugin_id: String,
        reason: String,
    },
    /// Stale or out-of-order configuration update rejected by optimistic concurrency control.
    #[error(
        "Stale configuration version for plugin '{plugin_id}': current is {current_version}, requested {requested_version}"
    )]
    StaleConfigVersion {
        plugin_id: String,
        current_version: u64,
        requested_version: u64,
    },
    /// Runtime environment was not found or is unsupported.
    #[error("Runtime environment '{runtime}' is not available: {reason}")]
    RuntimeUnavailable { runtime: String, reason: String },
}

impl From<tonic::Status> for SupervisorError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

/// Recorded recipe describing how a host process was launched.
///
/// Retaining the launch recipe is what makes control-plane restarts truthful:
/// the supervisor can only respawn a process it knows how to reconstruct.
#[derive(Debug, Clone)]
pub enum LaunchSpec {
    /// Host was spawned from a raw executable with an explicit argument vector.
    Direct {
        /// Executable path passed to the OS.
        executable: PathBuf,
        /// Argument vector passed alongside the executable.
        args: Vec<String>,
        /// Pipeline execution priority retained across restarts.
        priority: i32,
    },
    /// Host was spawned from a `plugin.toml` manifest.
    ///
    /// On restart the runtime launcher (Python interpreter / Node binary / native
    /// executable) is re-resolved, so toolchain upgrades are picked up automatically.
    Manifest {
        /// Absolute or project-relative path to the manifest file.
        manifest_path: PathBuf,
        /// Optional pre-built artifact overriding the manifest entrypoint.
        executable_override: Option<PathBuf>,
        /// Pipeline execution priority retained across restarts.
        priority: i32,
    },
}

/// Represents a running child plugin host managed by the supervisor.
pub struct ManagedHost {
    /// Unique identifier for this host (e.g. `host_demo_rust`).
    pub host_id: String,
    /// Filesystem path to the dedicated IPC endpoint.
    pub socket_path: PathBuf,
    /// Sub-process child handle. Wrapped in a Mutex for exclusive wait/kill operations.
    child: Mutex<Option<Child>>,
    /// Client for host lifecycle operations (`PluginHostService`).
    ///
    /// Every call works on a clone: a tonic client over one HTTP/2 channel multiplexes concurrent
    /// requests, so serializing them behind a lock would only add head-of-line blocking — and a
    /// deadlock when a command handler on this host waits for a reply delivered by an adapter
    /// living on the same host.
    host_client: PluginHostServiceClient<Channel>,
    /// Client for event and message dispatching (`MessagePipelineService`), cloned per call.
    pipeline_client: MessagePipelineServiceClient<Channel>,
    /// Metadata reported by the host: obtained during the handshake and refreshed after every
    /// accepted configuration reload, so a plugin may offer different commands or tools depending
    /// on how the operator configured it.
    ///
    /// A synchronous lock: it is only held to clone or replace the vector, never across an await.
    meta: std::sync::RwLock<Vec<PluginMeta>>,
    /// Execution priority for pipeline scheduling (lower executes first, default 500).
    pub priority: i32,
    /// Retained launch recipe, absent for externally registered hosts.
    launch_spec: Option<LaunchSpec>,
    /// Static manifest that produced this host, retained for control-plane queries.
    pub manifest: Option<PluginManifest>,
    /// Adaptive circuit breaker tracking latency beacons and failures for this host.
    pub circuit_breaker: Arc<CircuitBreaker>,
    /// Runtime health reported to the control plane and updated by the host watchdog.
    health: Mutex<HostHealth>,
}

/// Represents a plugin whose launch was deferred or failed due to runtime unavailability.
#[derive(Debug, Clone)]
pub struct UnavailablePlugin {
    /// Parsed manifest of the plugin.
    pub manifest: PluginManifest,
    /// Path to the manifest file on disk.
    pub manifest_path: PathBuf,
    /// Reason explaining why the runtime could not be activated.
    pub reason: String,
    /// Lifecycle status, typically "RuntimeUnavailable".
    pub status: String,
}

impl ManagedHost {
    /// Creates a new `ManagedHost` with an established channel, primarily used in testing or direct registration.
    ///
    /// The host is registered without a launch recipe, meaning it cannot be restarted
    /// by the supervisor (see [`SupervisorError::RestartUnavailable`]).
    pub fn new(
        host_id: String,
        socket_path: PathBuf,
        channel: Channel,
        meta: Vec<PluginMeta>,
        priority: i32,
    ) -> Self {
        Self {
            host_id,
            socket_path,
            child: Mutex::new(None),
            host_client: PluginHostServiceClient::new(channel.clone()),
            pipeline_client: MessagePipelineServiceClient::new(channel),
            meta: std::sync::RwLock::new(meta),
            priority,
            launch_spec: None,
            manifest: None,
            circuit_breaker: Arc::new(CircuitBreaker::with_defaults()),
            health: Mutex::new(HostHealth::default()),
        }
    }

    /// Returns the OS process identifier (PID) of the managed child process, if running.
    pub async fn pid(&self) -> Option<u32> {
        self.child.lock().await.as_ref().and_then(|c| c.id())
    }

    /// Attaches an adaptive circuit breaker configuration to this host.
    pub fn with_circuit_breaker(mut self, breaker: Arc<CircuitBreaker>) -> Self {
        self.circuit_breaker = breaker;
        self
    }

    /// Returns a reference to the active circuit breaker for this host.
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit_breaker
    }

    /// Attaches a retained launch recipe to this host.
    pub fn with_launch_spec(mut self, spec: LaunchSpec) -> Self {
        self.launch_spec = Some(spec);
        self
    }

    /// Attaches a static manifest to this host for metadata and config-schema queries.
    pub fn with_manifest(mut self, manifest: PluginManifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Returns the retained launch recipe, if the supervisor spawned this process.
    pub fn launch_spec(&self) -> Option<&LaunchSpec> {
        self.launch_spec.as_ref()
    }

    /// Returns the static manifest retained for this host, if any.
    pub fn manifest(&self) -> Option<&PluginManifest> {
        self.manifest.as_ref()
    }

    /// Snapshot of the plugin metadata this host currently reports.
    pub fn metas(&self) -> Vec<PluginMeta> {
        self.meta
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Replaces the cached plugin metadata.
    pub fn set_metas(&self, metas: Vec<PluginMeta>) {
        *self
            .meta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = metas;
    }

    /// Returns `true` when this host declares the given plugin identifier.
    pub fn declares_plugin(&self, plugin_id: &str) -> bool {
        self.metas().iter().any(|m| m.id == plugin_id)
    }

    /// Returns the platform identifiers this host serves as an adapter, from its static manifest.
    ///
    /// Only manifest-declared platforms are returned: adapter ownership must be knowable before
    /// the first message arrives, so it cannot depend on a runtime handshake value.
    pub fn adapter_platforms(&self) -> Vec<String> {
        self.manifest
            .as_ref()
            .and_then(|manifest| manifest.adapter.as_ref())
            .map(|adapter| vec![adapter.platform.clone()])
            .unwrap_or_default()
    }

    /// Returns the console-facing adapter name declared by this host, when it is an adapter.
    pub fn adapter_display_name(&self) -> Option<String> {
        self.manifest
            .as_ref()
            .and_then(|manifest| manifest.adapter.as_ref())
            .map(|adapter| {
                adapter
                    .display_name
                    .clone()
                    .unwrap_or_else(|| adapter.platform.clone())
            })
    }

    /// Returns the capabilities this host's adapter declaration lists, sorted.
    pub fn adapter_capabilities(&self) -> Vec<crate::adapter::Capability> {
        let mut capabilities: Vec<_> = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.adapter.as_ref())
            .map(|adapter| adapter.capabilities.clone())
            .unwrap_or_default();
        capabilities.sort();
        capabilities.dedup();
        capabilities
    }

    /// Returns the plugin identifier of this host's adapter declaration, when present.
    ///
    /// The manifest's plugin id is authoritative here even before a handshake reports metadata.
    pub fn adapter_plugin_id(&self) -> Option<String> {
        self.manifest
            .as_ref()
            .filter(|manifest| manifest.adapter.is_some())
            .map(|manifest| manifest.plugin.id.clone())
    }
}

impl std::fmt::Debug for ManagedHost {
    /// Renders the control-plane view of the host.
    ///
    /// Child processes and gRPC clients are intentionally omitted: they have no meaningful
    /// textual representation and are guarded by mutexes that must not be locked for logging.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let metas = self.metas();
        let plugin_ids: Vec<&str> = metas.iter().map(|meta| meta.id.as_str()).collect();
        f.debug_struct("ManagedHost")
            .field("host_id", &self.host_id)
            .field("socket_path", &self.socket_path)
            .field("plugin_ids", &plugin_ids)
            .field("priority", &self.priority)
            .field("restartable", &self.launch_spec.is_some())
            .finish_non_exhaustive()
    }
}

#[allow(clippy::result_large_err)]
impl ManagedHost {
    /// Dispatches an event through this host's pre-filter pipeline.
    pub async fn pre_filter(
        &self,
        req: PipelineEventRequest,
    ) -> Result<PreFilterResult, tonic::Status> {
        let mut client = self.pipeline_client.clone();
        let response = client.on_pre_filter(req).await?;
        Ok(response.into_inner())
    }

    /// Dispatches a slash command to this host for execution.
    pub async fn execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let start = std::time::Instant::now();
        let mut client = self.pipeline_client.clone();
        match client.on_execute_command(req).await {
            Ok(response) => {
                self.circuit_breaker.record_success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                self.circuit_breaker
                    .record_failure(&format!("Command gRPC error: {}", status.code()));
                Err(status)
            }
        }
    }

    /// Delivers a lifecycle event to one of this host's plugins.
    ///
    /// Not counted by the circuit breaker: events are optional notifications, and a plugin that
    /// ignores them must not get its commands and tools fast-failed.
    pub async fn notify_event(&self, req: EventNotification) -> Result<(), tonic::Status> {
        let mut client = self.pipeline_client.clone();
        client.on_event(req).await?;
        Ok(())
    }

    /// Asks one of this host's plugins to rewrite a reply.
    pub async fn decorate_reply(
        &self,
        req: DecorateReplyRequest,
    ) -> Result<DecorateReplyResult, tonic::Status> {
        let mut client = self.pipeline_client.clone();
        Ok(client.on_decorate_reply(req).await?.into_inner())
    }

    /// Asks one of this host's plugins for context to add to the turn the model is about to answer.
    pub async fn prepare_turn(
        &self,
        req: PrepareTurnRequest,
    ) -> Result<PrepareTurnResult, tonic::Status> {
        let mut client = self.pipeline_client.clone();
        Ok(client.on_prepare_turn(req).await?.into_inner())
    }

    /// Asks a plugin to rewrite the system prompt of the turn about to be answered.
    pub async fn rewrite_system_prompt(
        &self,
        req: kanon_proto::v1::LlmRequestHookRequest,
    ) -> Result<kanon_proto::v1::LlmRequestHookResult, tonic::Status> {
        let mut client = self.pipeline_client.clone();
        Ok(client.on_llm_request(req).await?.into_inner())
    }

    /// Forwards an HTTP request to one of the plugin's web routes.
    pub async fn http_request(
        &self,
        req: kanon_proto::v1::HttpRequest,
    ) -> Result<kanon_proto::v1::HttpResponse, tonic::Status> {
        let mut client = self.pipeline_client.clone();
        Ok(client.on_http_request(req).await?.into_inner())
    }

    /// Queries the host for fresh plugin metadata.
    pub async fn get_plugin_meta(&self) -> Result<Vec<PluginMeta>, tonic::Status> {
        let mut client = self.host_client.clone();
        let response = client.get_plugin_meta(GetPluginMetaRequest {}).await?;
        Ok(response.into_inner().plugins)
    }

    /// Dispatches a tool call to this host for execution via gRPC IPC.
    ///
    /// If this host's circuit breaker is currently open, fast-fails immediately without
    /// waiting for gRPC timeouts, thereby preserving the LLM tool reasoning throughput.
    pub async fn on_call_tool(
        &self,
        req: ToolCallRequest,
    ) -> Result<ToolCallResponse, tonic::Status> {
        if !self.circuit_breaker.allow_request() {
            tracing::warn!(
                host_id = %self.host_id,
                tool_name = %req.tool_name,
                "Circuit breaker is OPEN; fast-skipping tool call"
            );
            return Err(tonic::Status::unavailable(format!(
                "Circuit breaker is OPEN for host '{}'",
                self.host_id
            )));
        }

        let start = std::time::Instant::now();
        let mut client = self.pipeline_client.clone();
        match client.on_call_tool(req).await {
            Ok(response) => {
                self.circuit_breaker.record_success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                self.circuit_breaker
                    .record_failure(&format!("Tool call gRPC error: {}", status.code()));
                Err(status)
            }
        }
    }

    /// Invokes a control-plane management action on this host.
    ///
    /// Actions are the operator-facing counterpart of tools: they are never advertised to the
    /// model, so an adapter can expose credential binding or diagnostics without handing the LLM
    /// a function it would otherwise call mid-conversation. Circuit-breaker accounting matches
    /// tool calls, because both are request/response RPCs to the same host.
    pub async fn invoke_action(
        &self,
        req: kanon_proto::v1::PluginActionRequest,
    ) -> Result<kanon_proto::v1::PluginActionResponse, tonic::Status> {
        if !self.circuit_breaker.allow_request() {
            tracing::warn!(
                host_id = %self.host_id,
                action = %req.action,
                "Circuit breaker is OPEN; fast-skipping management action"
            );
            return Err(tonic::Status::unavailable(format!(
                "Circuit breaker is OPEN for host '{}'",
                self.host_id
            )));
        }

        let start = std::time::Instant::now();
        let mut client = self.host_client.clone();
        match client.invoke_action(req).await {
            Ok(response) => {
                self.circuit_breaker.record_success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                self.circuit_breaker
                    .record_failure(&format!("Management action gRPC error: {}", status.code()));
                Err(status)
            }
        }
    }

    /// Pushes a refreshed configuration object to this host and triggers in-process hot reload.
    ///
    /// The plugin host updates its memory-resident configuration cache synchronously, so the
    /// next PreFilter / command invocation observes the new values without a process restart.
    pub async fn reload_config(
        &self,
        plugin_id: &str,
        config: kanon_proto::prost_types::Struct,
        version: u64,
    ) -> Result<ReloadPluginConfigResponse, tonic::Status> {
        let mut client = self.host_client.clone();
        let response = client
            .reload_plugin_config(ReloadPluginConfigRequest {
                plugin_id: plugin_id.to_string(),
                config: Some(config),
                version,
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Hands an outbound message to this host so the plugin acting as a platform adapter can
    /// publish it to the target platform.
    pub async fn deliver_message(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, tonic::Status> {
        let start = std::time::Instant::now();
        let mut client = self.pipeline_client.clone();
        match client.on_deliver_message(request).await {
            Ok(response) => {
                self.circuit_breaker.record_success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                self.circuit_breaker
                    .record_failure(&format!("DeliverMessage gRPC error: {}", status.code()));
                Err(status)
            }
        }
    }
}

impl ManagedHost {
    /// Current runtime health of this host process.
    pub async fn health(&self) -> HostHealth {
        self.health.lock().await.clone()
    }

    /// Replaces the reported health (watchdog and control plane only).
    pub async fn set_health(&self, health: HostHealth) {
        *self.health.lock().await = health;
    }

    /// Records one health observation.
    pub async fn report_health(&self, state: &str, restarts: u32, last_error: Option<String>) {
        *self.health.lock().await = HostHealth::new(state, restarts, last_error);
    }

    /// Identifier of the plugin this host was launched for, when it declared one.
    pub fn primary_plugin_id(&self) -> Option<String> {
        self.metas()
            .first()
            .map(|meta| meta.id.clone())
            .or_else(|| self.manifest.as_ref().map(|m| m.plugin.id.clone()))
    }

    /// Whether the supervisor can relaunch this host from a recorded recipe.
    pub fn is_restartable(&self) -> bool {
        self.launch_spec.is_some()
    }
}

#[tonic::async_trait]
#[tonic::async_trait]
impl kanon_llm::tool_router::ToolHost for ManagedHost {
    fn host_id(&self) -> &str {
        &self.host_id
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        self.metas()
    }

    async fn call_tool(&self, mut req: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        // The router knows the session, not the platform event; the pipeline scoped the event
        // around the turn, so the plugin learns who asked and where.
        if req.context.is_none() {
            req.context = TOOL_EVENT.try_with(Clone::clone).ok();
        }
        self.on_call_tool(req).await
    }
}

tokio::task_local! {
    // Task scope (not a field) because one agent serves overlapping turns; each turn's tool calls
    // must see their own event and never a neighbour's.
    static TOOL_EVENT: PipelineEventRequest;
}

/// Runs `turn` with `event` attached to every plugin tool call it makes.
///
/// Tool calls made inside the future carry the event as `ToolCallRequest.context`, so a tool can
/// tell who invoked it and in which conversation without trusting model-supplied arguments.
pub async fn with_tool_event<F: std::future::Future>(
    event: PipelineEventRequest,
    turn: F,
) -> F::Output {
    TOOL_EVENT.scope(event, turn).await
}

/// Where an outbound message for a platform should be delivered.
#[derive(Clone)]
pub enum AdapterRoute {
    /// An in-process adapter registered on the [`AdapterRegistry`].
    Builtin(Arc<dyn crate::adapter::PlatformAdapter>),
    /// A plugin host whose manifest declares the platform.
    Plugin {
        /// Host process that owns the adapter plugin.
        host: Arc<ManagedHost>,
        /// Plugin identifier declared by the manifest.
        plugin_id: String,
    },
}

impl AdapterRoute {
    /// The capabilities the routed adapter declares, from its trait or its plugin manifest.
    pub fn capabilities(&self) -> Vec<crate::adapter::Capability> {
        match self {
            AdapterRoute::Builtin(adapter) => adapter.capabilities().to_vec(),
            AdapterRoute::Plugin { host, .. } => host.adapter_capabilities(),
        }
    }
}

impl std::fmt::Debug for AdapterRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterRoute::Builtin(adapter) => f
                .debug_struct("AdapterRoute::Builtin")
                .field("platform", &adapter.platform())
                .finish(),
            AdapterRoute::Plugin { host, plugin_id } => f
                .debug_struct("AdapterRoute::Plugin")
                .field("host_id", &host.host_id)
                .field("plugin_id", plugin_id)
                .finish(),
        }
    }
}

/// Supervisor responsible for managing the lifecycle of out-of-process plugin hosts.
pub struct Supervisor {
    /// Base directory where IPC sockets and temporary state reside.
    run_dir: PathBuf,
    /// Path to the Kanon Core IPC server socket (`core.sock`).
    core_sock_path: PathBuf,
    /// Registry of active managed plugin host instances.
    hosts: Arc<RwLock<HashMap<String, Arc<ManagedHost>>>>,
    /// Registry of in-process platform adapters.
    adapters: Arc<AdapterRegistry>,
    /// Monotonically increasing configuration version tracking per plugin for CAS updates.
    config_versions: Arc<RwLock<HashMap<String, u64>>>,
    /// Plugins whose launch was prevented or deferred due to missing runtime environments.
    unavailable_plugins: Arc<RwLock<HashMap<String, UnavailablePlugin>>>,
    /// Interpreter for TypeScript plugins; `bun`, then `node`, from `PATH` when unset.
    typescript_runtime: Option<PathBuf>,
    /// Host ids whose process this supervisor is starting right now (see [`LaunchGuard`]).
    ///
    /// A launched host calls `RegisterHost` while its launch is still waiting for the socket, and
    /// the Rust SDK does so before it even binds the socket. The launch owns that host's handshake
    /// and registry entry, so [`Supervisor::register_host_endpoint`] must only acknowledge it:
    /// dialing back would fail on the missing socket, or wait on a host that is itself waiting
    /// for the registration reply. A std mutex suffices because no await happens while it is held.
    launching: Arc<std::sync::Mutex<HashSet<String>>>,
}

/// Outcome of a host's `RegisterHost` call.
#[derive(Debug)]
pub enum HostRegistration {
    /// The host is in the registry: it already was, or it was just attached from its endpoint.
    Registered(Arc<ManagedHost>),
    /// This supervisor is launching the host; that launch completes the handshake and adds the
    /// registry entry once the host is serving.
    Launching,
}

/// Marks a host id as being launched for as long as the guard lives.
///
/// Dropping clears the mark on every exit of the launch, including early returns and failures,
/// so a host whose launch failed is never mistaken for one still starting.
struct LaunchGuard {
    launching: Arc<std::sync::Mutex<HashSet<String>>>,
    host_id: String,
}

impl LaunchGuard {
    fn new(launching: &Arc<std::sync::Mutex<HashSet<String>>>, host_id: &str) -> Self {
        lock_launching(launching).insert(host_id.to_string());
        Self {
            launching: launching.clone(),
            host_id: host_id.to_string(),
        }
    }
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        lock_launching(&self.launching).remove(&self.host_id);
    }
}

/// Locks the set of launching hosts.
///
/// Its critical sections are single `HashSet` operations that cannot panic midway, so a poisoned
/// lock still guards a consistent set and is safe to reuse.
fn lock_launching(
    launching: &std::sync::Mutex<HashSet<String>>,
) -> std::sync::MutexGuard<'_, HashSet<String>> {
    launching
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("run_dir", &self.run_dir)
            .field("core_sock_path", &self.core_sock_path)
            .finish_non_exhaustive()
    }
}

/// Grace period a plugin host gets to finish `on_unload` before it is killed.
pub const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// How often the host watchdog checks its children for unexpected exits.
pub const HOST_WATCHDOG_INTERVAL: Duration = Duration::from_secs(5);

/// Consecutive automatic restarts allowed before a host is parked as crashed.
///
/// A plugin that crashes immediately after every launch is broken; restarting it forever would
/// burn CPU and hide the fault from the console.
pub const HOST_WATCHDOG_MAX_RESTARTS: u32 = 5;

/// Uptime after which a restarted host is considered healthy again and its budget resets.
pub const HOST_WATCHDOG_HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Runtime health of a supervised host, as observed by the host watchdog.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostHealth {
    /// `running`, `restarting`, `crashed` or `disabled`.
    pub state: String,
    /// Automatic restarts performed since the node started.
    pub restarts: u32,
    /// Last observed failure, when any.
    pub last_error: Option<String>,
}

impl HostHealth {
    /// Builds a health snapshot.
    pub fn new(state: impl Into<String>, restarts: u32, last_error: Option<String>) -> Self {
        Self {
            state: state.into(),
            restarts,
            last_error,
        }
    }
}

impl Default for HostHealth {
    fn default() -> Self {
        Self::new("running", 0, None)
    }
}

/// Restart bookkeeping for one host, owned by the watchdog task.
#[derive(Debug, Default)]
struct RestartBudget {
    /// Attempts since the host last stayed up long enough to be considered healthy.
    attempts: u32,
    /// Earliest instant at which the next attempt may run (exponential backoff).
    next_at: Option<Instant>,
    /// When the last attempt ran, used to detect a healthy period.
    last_attempt: Option<Instant>,
}

/// Terminates a host process as gently as it allows.
///
/// Lifecycle order: ask the process to stop (`SIGTERM` on Unix, which the Python, Rust and
/// TypeScript hosts all translate into a graceful unload), give it [`HOST_SHUTDOWN_GRACE`] to
/// close its platform connections, then kill it. Skipping the polite request would deny plugins
/// their `on_unload` hook and drop platform connections abruptly; skipping the escalation would
/// let a wedged host outlive its core.
///
/// Windows has no `SIGTERM`; there the process is terminated directly, which is the platform's
/// only mechanism.
pub async fn terminate_child(child: &mut Child, grace: Duration) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // A failure here (already-exited process, for instance) is not fatal: the wait below
            // still reaps it.
            let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if rc != 0 {
                tracing::debug!(
                    pid,
                    error = %std::io::Error::last_os_error(),
                    "SIGTERM could not be delivered; falling back to killing the host"
                );
            } else {
                match tokio::time::timeout(grace, child.wait()).await {
                    Ok(Ok(status)) => {
                        tracing::debug!(pid, status = %status, "Host exited after SIGTERM");
                        return Ok(());
                    }
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {
                        tracing::warn!(
                            pid,
                            grace_ms = grace.as_millis() as u64,
                            "Host ignored SIGTERM within the grace period; killing it"
                        );
                    }
                }
            }
        }
    }

    #[cfg(not(unix))]
    let _ = grace;

    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(())
}

impl Supervisor {
    /// Creates a new `Supervisor` instance.
    ///
    /// If `run_dir` is not specified, [`default_run_dir`] is used.
    /// If `core_sock_path` is not specified, [`core_socket_path`] within `run_dir` is used.
    pub fn new(run_dir: Option<PathBuf>, core_sock_path: Option<PathBuf>) -> Self {
        let run_dir = run_dir.unwrap_or_else(default_run_dir);
        let core_sock_path = core_sock_path.unwrap_or_else(|| core_socket_path(Some(&run_dir)));

        // Ensure the run directory exists with strict permissions and symlink validation.
        let _ = kanon_transport::ensure_run_dir(&run_dir);

        Self {
            run_dir,
            core_sock_path,
            hosts: Arc::new(RwLock::new(HashMap::new())),
            adapters: Arc::new(AdapterRegistry::new()),
            config_versions: Arc::new(RwLock::new(HashMap::new())),
            unavailable_plugins: Arc::new(RwLock::new(HashMap::new())),
            typescript_runtime: None,
            launching: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }

    /// Uses `runtime` (a `bun` or `node` binary) for TypeScript plugins instead of the `PATH`
    /// lookup, as the node's `startup.typescript_runtime` setting asks.
    pub fn with_typescript_runtime(mut self, runtime: Option<PathBuf>) -> Self {
        self.typescript_runtime = runtime;
        self
    }

    /// Records a plugin as unavailable due to missing runtime environments or dependencies.
    pub async fn record_unavailable_plugin(
        &self,
        manifest: PluginManifest,
        manifest_path: PathBuf,
        reason: String,
    ) {
        let plugin_id = manifest.plugin.id.clone();
        self.unavailable_plugins.write().await.insert(
            plugin_id,
            UnavailablePlugin {
                manifest,
                manifest_path,
                reason,
                status: "RuntimeUnavailable".to_string(),
            },
        );
    }

    /// Returns all plugins currently recorded as unavailable.
    pub async fn get_unavailable_plugins(&self) -> Vec<UnavailablePlugin> {
        self.unavailable_plugins
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Removes a recorded unavailable plugin entry (e.g. after a successful launch).
    pub async fn remove_unavailable_plugin(&self, plugin_id: &str) {
        self.unavailable_plugins.write().await.remove(plugin_id);
    }

    /// Returns the currently applied configuration version for a plugin (0 if never configured).
    pub async fn config_version(&self, plugin_id: &str) -> u64 {
        self.config_versions
            .read()
            .await
            .get(plugin_id)
            .copied()
            .unwrap_or(0)
    }

    /// Returns the built-in adapter registry owned by this supervisor.
    ///
    /// The composition root registers in-process adapters here; plugin-declared adapters are
    /// discovered from host manifests instead of being registered, so host restarts and crashes
    /// can never leave stale routing entries behind.
    pub fn adapters(&self) -> &Arc<AdapterRegistry> {
        &self.adapters
    }

    /// Resolves the adapter responsible for a platform.
    ///
    /// Built-in adapters win over plugins: an in-process adapter is the operator's explicit
    /// override, and it can never be unavailable because of a crashed sub-process.
    pub async fn resolve_adapter(&self, platform: &str) -> Option<AdapterRoute> {
        if let Some(adapter) = self.adapters.get(platform).await {
            return Some(AdapterRoute::Builtin(adapter));
        }

        let hosts = self.hosts.read().await;
        for host in hosts.values() {
            if host.adapter_platforms().iter().any(|p| p == platform) {
                let plugin_id = host
                    .adapter_plugin_id()
                    .unwrap_or_else(|| host.host_id.clone());
                return Some(AdapterRoute::Plugin {
                    host: host.clone(),
                    plugin_id,
                });
            }
        }

        None
    }

    /// Builds the console-facing adapter catalog: built-ins first, then plugin adapters.
    ///
    /// Note: Circuit breaker states default to [`CircuitState::Closed`] here; live platform
    /// outbound breaker states are tracked and queried through [`crate::pipeline::PipelineEngine::adapter_catalog`].
    pub async fn adapter_catalog(&self) -> Vec<AdapterDescriptor> {
        let mut catalog: Vec<AdapterDescriptor> = Vec::new();
        for adapter in self.adapters.list().await {
            catalog.push(AdapterDescriptor {
                platform: adapter.platform().to_string(),
                display_name: adapter.display_name().to_string(),
                kind: AdapterKind::Builtin,
                connected: adapter.is_connected(),
                circuit_state: CircuitState::Closed,
                plugin_id: None,
                host_id: None,
                capabilities: {
                    let mut capabilities = adapter.capabilities().to_vec();
                    capabilities.sort();
                    capabilities
                },
            });
        }

        for host in self.hosts.read().await.values() {
            let platforms = host.adapter_platforms();
            if platforms.is_empty() {
                continue;
            }

            let display_name = host
                .adapter_display_name()
                .unwrap_or_else(|| host.host_id.clone());
            let plugin_id = host.adapter_plugin_id();

            for platform in platforms {
                catalog.push(AdapterDescriptor {
                    platform,
                    display_name: display_name.clone(),
                    kind: AdapterKind::Plugin,
                    // A host present in the registry is a live process; a crashed host is removed
                    // by the supervisor, so presence is the connection signal.
                    connected: true,
                    circuit_state: CircuitState::Closed,
                    plugin_id: plugin_id.clone(),
                    host_id: Some(host.host_id.clone()),
                    capabilities: host.adapter_capabilities(),
                });
            }
        }

        // Deterministic ordering keeps console tables stable across polls.
        catalog.sort_by(|a, b| a.platform.cmp(&b.platform));
        catalog
    }

    /// Returns a reference to the active run directory.
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Returns a reference to the Core socket path.
    pub fn core_sock_path(&self) -> &Path {
        &self.core_sock_path
    }

    /// Spawns a plugin host sub-process, waits for its IPC socket readiness,
    /// performs the initial `GetPluginMeta` handshake, and registers it with default priority (500).
    pub async fn spawn_plugin(
        &self,
        host_id: &str,
        executable_path: impl AsRef<Path>,
        args: &[&str],
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        self.spawn_plugin_with_priority(host_id, executable_path, args, 500)
            .await
    }

    /// Spawns a plugin host sub-process with explicit execution priority,
    /// waits for its IPC socket readiness, performs the initial `GetPluginMeta`
    /// handshake, and registers it in the supervisor registry.
    pub async fn spawn_plugin_with_priority(
        &self,
        host_id: &str,
        executable_path: impl AsRef<Path>,
        args: &[&str],
        priority: i32,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        let spec = LaunchSpec::Direct {
            executable: executable_path.as_ref().to_path_buf(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            priority,
        };
        self.launch_host(
            host_id,
            executable_path.as_ref(),
            args,
            priority,
            spec,
            None,
        )
        .await
    }

    /// Core host launch routine shared by every spawn path.
    ///
    /// `spec` records how the process was launched so that a later control-plane restart
    /// can faithfully reconstruct the same command line, and `manifest` retains the static
    /// plugin declaration for metadata / configuration-schema queries.
    async fn launch_host(
        &self,
        host_id: &str,
        executable_path: &Path,
        args: &[&str],
        priority: i32,
        spec: LaunchSpec,
        manifest: Option<PluginManifest>,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        // Held until this function returns: the host registers itself while we are still
        // waiting for its socket below, and that registration must not dial it (see `launching`).
        let _launching = LaunchGuard::new(&self.launching, host_id);
        let socket_path = host_socket_path(host_id, Some(&self.run_dir));

        // Clean up stale socket file if it exists prior to launching child.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        tracing::info!(
            host_id = %host_id,
            priority = priority,
            executable = %executable_path.display(),
            socket = %socket_path.display(),
            "Spawning plugin host process"
        );

        let mut cmd = Command::new(executable_path);
        cmd.args(args)
            .env("KANON_HOST_ID", host_id)
            .env("KANON_HOST_SOCK", &socket_path)
            .env("KANON_CORE_SOCK", &self.core_sock_path)
            .kill_on_drop(true);

        let mut child = cmd.spawn()?;

        // Wait for child process to bind the socket and become ready.
        // We poll every 50ms with a 5-second deadline.
        let channel = match self
            .wait_for_readiness(&mut child, host_id, &socket_path, Duration::from_secs(5))
            .await
        {
            Ok(ch) => ch,
            Err(e) => {
                // The process never became usable, so there is nothing to unload gracefully:
                // kill it outright to prevent an orphan.
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(e);
            }
        };

        // Create gRPC clients for handshake and future message pipeline calls.
        let mut host_client = PluginHostServiceClient::new(channel.clone());
        let pipeline_client = MessagePipelineServiceClient::new(channel);

        // Perform initial GetPluginMeta handshake to verify contract compatibility
        // and discover static command/tool definitions.
        tracing::debug!(host_id = %host_id, "Conducting GetPluginMeta handshake");
        let meta_response = host_client
            .get_plugin_meta(GetPluginMetaRequest {})
            .await
            .map_err(|status| {
                tracing::error!(host_id = %host_id, error = %status, "Handshake failed");
                SupervisorError::from(status)
            })?;

        let plugins = meta_response.into_inner().plugins;
        tracing::info!(
            host_id = %host_id,
            plugin_count = plugins.len(),
            "Handshake completed successfully with plugin host"
        );

        let managed_host = Arc::new(ManagedHost {
            host_id: host_id.to_string(),
            socket_path: socket_path.clone(),
            child: Mutex::new(Some(child)),
            host_client,
            pipeline_client,
            meta: std::sync::RwLock::new(plugins),
            priority,
            launch_spec: Some(spec),
            manifest,
            circuit_breaker: Arc::new(CircuitBreaker::with_defaults()),
            health: Mutex::new(HostHealth::default()),
        });

        self.hosts
            .write()
            .await
            .insert(host_id.to_string(), managed_host.clone());

        Ok(managed_host)
    }

    /// Spawns a plugin sub-process based on a `plugin.toml` manifest file,
    /// dynamically resolving the runtime launcher (Rust native binary, the plugin's own Python
    /// virtual environment, or Node/Bun runtime for TypeScript).
    ///
    /// Kanon never installs plugin dependencies. A Python plugin must ship a `.venv` built by the
    /// operator (`uv sync`), and a TypeScript plugin that declares dependencies must have its
    /// `node_modules` installed; otherwise the plugin is reported as `RuntimeUnavailable` before
    /// any process starts, instead of crashing on an import error inside a restart loop.
    pub async fn spawn_from_manifest(
        &self,
        manifest_path: impl AsRef<Path>,
        executable_override: Option<&Path>,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        let manifest_path_ref = manifest_path.as_ref();
        let manifest = PluginManifest::load_from_file(manifest_path_ref)
            .map_err(|e| SupervisorError::Manifest(e.to_string()))?;
        if let Some(adapter) = manifest
            .adapter
            .as_ref()
            .filter(|adapter| adapter.sends_no_media())
        {
            tracing::warn!(
                plugin_id = %manifest.plugin.id,
                platform = %adapter.platform,
                "Adapter plugin declares no send_image/send_voice/send_video/send_file capability; \
                 tool-produced media is left out of its replies. Declare the kinds the platform \
                 delivers under [adapter] capabilities in plugin.toml"
            );
        }

        let host_id = manifest.plugin.id.replace('.', "_");
        let priority = manifest.plugin.priority.unwrap_or(500);
        let parent = manifest_path_ref.parent().unwrap_or_else(|| Path::new("."));

        // Manifest-driven launches are replayed through the manifest on restart, so the
        // recipe only needs to remember the manifest location and priority.
        let spec = LaunchSpec::Manifest {
            manifest_path: manifest_path_ref.to_path_buf(),
            executable_override: executable_override.map(Path::to_path_buf),
            priority,
        };

        // The resolution steps below return early with `?`. Running them inside this block keeps
        // those early returns from skipping the unavailable-plugin bookkeeping that follows.
        let result = async {
            if let Some(override_path) = executable_override {
                self.launch_host(
                    &host_id,
                    override_path,
                    &[],
                    priority,
                    spec,
                    Some(manifest.clone()),
                )
                .await
            } else {
                match manifest.plugin.runtime.as_str() {
                    "rust" => {
                        let exec_path = parent.join(&manifest.plugin.entrypoint);
                        self.launch_host(
                            &host_id,
                            &exec_path,
                            &[],
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    "python" => {
                        // Each plugin runs in its own environment so two plugins can never
                        // disagree about a package version. There is deliberately no fallback to a
                        // shared or system interpreter: it would start without the plugin's packages.
                        let python_bin = plugin_python(parent).ok_or_else(|| {
                            SupervisorError::RuntimeUnavailable {
                                runtime: "python".to_string(),
                                reason: format!(
                                    "Python environment '{}' not found; run `uv sync` in the plugin directory",
                                    parent.join(".venv").display()
                                ),
                            }
                        })?;

                        // The host runner ships with the SDK, which the plugin's environment
                        // depends on, so it is run from there: no path to configure, and it works
                        // wherever the plugin is installed.
                        let manifest_str = manifest_path_ref.to_string_lossy();
                        let args = ["-m", "kanon_host.main", "--plugin", manifest_str.as_ref()];

                        self.launch_host(
                            &host_id,
                            &python_bin,
                            &args,
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    "typescript" | "ts" => {
                        ensure_node_modules(parent).map_err(|reason| {
                            SupervisorError::RuntimeUnavailable {
                                runtime: "typescript".to_string(),
                                reason,
                            }
                        })?;

                        let node_bin = self
                            .typescript_runtime
                            .clone()
                            .or_else(|| find_binary_in_path("bun"))
                            .or_else(|| find_binary_in_path("node"))
                            .ok_or_else(|| SupervisorError::RuntimeUnavailable {
                                runtime: "typescript".to_string(),
                                reason: "Neither bun nor node was found in PATH".to_string(),
                            })?;

                        // A plugin that depends on the SDK carries the host runner in its own
                        // `node_modules`; inside a Kanon checkout the built SDK is used instead.
                        let installed_host = parent
                            .join("node_modules/@kanon/sdk-and-host/dist/src/host/index.js");
                        let host_script = installed_host
                            .is_file()
                            .then_some(installed_host)
                            .or_else(|| find_file_upwards(parent, "sdks/typescript/dist/src/host/index.js"))
                            .or_else(|| find_file_upwards(parent, "dist/src/host/index.js"))
                            .ok_or_else(|| SupervisorError::RuntimeUnavailable {
                                runtime: "typescript".to_string(),
                                reason: "Could not locate TypeScript host runner script (dist/src/host/index.js)"
                                    .to_string(),
                            })?;

                        let host_script_str = host_script.to_string_lossy();
                        let manifest_str = manifest_path_ref.to_string_lossy();
                        let args = [host_script_str.as_ref(), "--plugin", manifest_str.as_ref()];

                        self.launch_host(
                            &host_id,
                            &node_bin,
                            &args,
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    other => Err(SupervisorError::RuntimeUnavailable {
                        runtime: other.to_string(),
                        reason: format!("Unsupported plugin runtime '{other}' declared in manifest"),
                    }),
                }
            }
        }
        .await;

        match result {
            Ok(host) => {
                self.remove_unavailable_plugin(&manifest.plugin.id).await;
                Ok(host)
            }
            Err(SupervisorError::RuntimeUnavailable { runtime, reason }) => {
                self.record_unavailable_plugin(
                    manifest,
                    manifest_path_ref.to_path_buf(),
                    reason.clone(),
                )
                .await;
                Err(SupervisorError::RuntimeUnavailable { runtime, reason })
            }
            Err(err) => Err(err),
        }
    }

    /// Restarts the host process identified by `host_id` using its recorded launch recipe.
    ///
    /// The old process is terminated and unregistered first, then relaunched with an identical
    /// command line and a fresh `GetPluginMeta` handshake, so a failed relaunch surfaces as an
    /// explicit error while the registry never retains a half-dead host entry.
    pub async fn restart_host(&self, host_id: &str) -> Result<Arc<ManagedHost>, SupervisorError> {
        let host = self
            .get_host(host_id)
            .await
            .ok_or_else(|| SupervisorError::HostNotFound(host_id.to_string()))?;

        let spec = host
            .launch_spec()
            .cloned()
            .ok_or_else(|| SupervisorError::RestartUnavailable(host_id.to_string()))?;
        let manifest = host.manifest().cloned();

        tracing::info!(host_id = %host_id, "Restarting plugin host process");

        self.stop_host(host_id).await?;

        match &spec {
            LaunchSpec::Direct {
                executable,
                args,
                priority,
            } => {
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                self.launch_host(
                    host_id,
                    executable,
                    &arg_refs,
                    *priority,
                    spec.clone(),
                    manifest,
                )
                .await
            }
            LaunchSpec::Manifest {
                manifest_path,
                executable_override,
                ..
            } => {
                self.spawn_from_manifest(manifest_path, executable_override.as_deref())
                    .await
            }
        }
    }

    /// Locates the host process that declares the given plugin identifier.
    pub async fn find_host_for_plugin(&self, plugin_id: &str) -> Option<Arc<ManagedHost>> {
        self.hosts
            .read()
            .await
            .values()
            .find(|host| host.declares_plugin(plugin_id))
            .cloned()
    }

    /// Pushes an updated configuration object to the host owning `plugin_id` and triggers hot reload.
    ///
    /// Monotonically increments the plugin's configuration version token. Returns the applied version.
    pub async fn reload_plugin_config(
        &self,
        plugin_id: &str,
        config: &serde_json::Value,
    ) -> Result<u64, SupervisorError> {
        self.reload_plugin_config_cas(plugin_id, config, None).await
    }

    /// Pushes an updated configuration with optimistic concurrency control (CAS).
    ///
    /// If `expected_version` is `Some(v)`, the reload only proceeds if the currently applied
    /// version matches `v`. On conflict, returns [`SupervisorError::StaleConfigVersion`].
    pub async fn reload_plugin_config_cas(
        &self,
        plugin_id: &str,
        config: &serde_json::Value,
        expected_version: Option<u64>,
    ) -> Result<u64, SupervisorError> {
        let host = self
            .find_host_for_plugin(plugin_id)
            .await
            .ok_or_else(|| SupervisorError::PluginNotFound(plugin_id.to_string()))?;

        let mut versions = self.config_versions.write().await;
        let current_ver = *versions.get(plugin_id).unwrap_or(&0);

        if let Some(expected) = expected_version
            && expected != current_ver
        {
            return Err(SupervisorError::StaleConfigVersion {
                plugin_id: plugin_id.to_string(),
                current_version: current_ver,
                requested_version: expected,
            });
        }

        let next_ver = current_ver + 1;

        let structured = kanon_llm::tool_router::json_to_prost_struct(config)
            .ok_or(SupervisorError::InvalidConfigPayload)?;

        let response = host.reload_config(plugin_id, structured, next_ver).await?;
        if !response.success {
            return Err(SupervisorError::ConfigReloadRejected {
                host_id: host.host_id.clone(),
                plugin_id: plugin_id.to_string(),
                reason: response.error_message,
            });
        }

        let applied = if response.applied_version > 0 {
            response.applied_version
        } else {
            next_ver
        };

        versions.insert(plugin_id.to_string(), applied);

        tracing::info!(
            plugin_id = %plugin_id,
            host_id = %host.host_id,
            version = applied,
            "Plugin configuration reloaded with version token"
        );

        // A plugin may derive its commands and tools from its configuration (an API key that
        // enables a tool, for example), so the metadata is asked for again. The configuration
        // itself is already applied, which is why a failed refresh is reported, not rolled back:
        // the previous metadata stays in effect until the next reload or restart.
        match host.get_plugin_meta().await {
            Ok(metas) if metas.iter().any(|meta| meta.id == plugin_id) => host.set_metas(metas),
            // A host that stops declaring the plugin it just reconfigured is inconsistent;
            // adopting that answer would make the plugin vanish from routing and the console.
            Ok(_) => tracing::warn!(
                plugin_id = %plugin_id,
                host_id = %host.host_id,
                "Refreshed metadata no longer declares the reloaded plugin; keeping the previous commands and tools"
            ),
            Err(status) => tracing::warn!(
                plugin_id = %plugin_id,
                host_id = %host.host_id,
                error = %status,
                "Plugin metadata could not be refreshed after a configuration reload; keeping the previous commands and tools"
            ),
        }

        Ok(applied)
    }

    /// Directly registers an externally created or mocked `ManagedHost` (useful for unit tests).
    pub async fn register_managed_host(&self, host: Arc<ManagedHost>) {
        self.hosts.write().await.insert(host.host_id.clone(), host);
    }

    /// Binds an external or gRPC-registered plugin host endpoint into the unified Supervisor registry.
    ///
    /// If the host has already been spawned and registered by this supervisor, returns the existing
    /// instance. If this supervisor is still launching it, the registration is only acknowledged
    /// ([`HostRegistration::Launching`]): the launch performs the handshake once the host serves.
    /// Otherwise, establishes an IPC connection to `endpoint`, performs the [`GetPluginMeta`]
    /// handshake, and inserts a new [`ManagedHost`] into the host registry.
    pub async fn register_host_endpoint(
        &self,
        host_id: &str,
        _runtime: &str,
        endpoint: &str,
        loaded_plugin_ids: &[String],
    ) -> Result<HostRegistration, SupervisorError> {
        // The launch mark is read before the registry: a launch sets it before spawning and
        // inserts its host before clearing it, so reading in this order never misses a host that
        // this supervisor owns. The reverse order could miss both and dial the host a second time,
        // replacing the launched entry (and its child handle) with an unmanaged one.
        if lock_launching(&self.launching).contains(host_id) {
            tracing::info!(
                host_id = %host_id,
                "Host is being launched by this Supervisor; the launch completes its registration"
            );
            return Ok(HostRegistration::Launching);
        }
        if let Some(existing) = self.get_host(host_id).await {
            tracing::info!(
                host_id = %host_id,
                "Host already known to Supervisor; keeping existing registration"
            );
            return Ok(HostRegistration::Registered(existing));
        }

        let socket_path = PathBuf::from(endpoint);
        let channel = connect_ipc(&socket_path).await?;
        let mut host_client = PluginHostServiceClient::new(channel.clone());

        // Attempt initial GetPluginMeta handshake over the established channel.
        let plugins = match host_client.get_plugin_meta(GetPluginMetaRequest {}).await {
            Ok(resp) => resp.into_inner().plugins,
            Err(status) => {
                tracing::warn!(
                    host_id = %host_id,
                    error = %status,
                    "GetPluginMeta handshake failed on external host; synthesising metadata from declarations"
                );
                loaded_plugin_ids
                    .iter()
                    .map(|id| PluginMeta {
                        id: id.clone(),
                        name: id.clone(),
                        version: "0.1.0".to_string(),
                        author: String::new(),
                        description: String::new(),
                        commands: vec![],
                        tools: vec![],
                        triggers: vec![],
                        events: vec![],
                        decorates_replies: false,
                        prepares_turns: false,
                        rewrites_system_prompt: false,
                        serves_http: false,
                    })
                    .collect()
            }
        };

        let managed_host = Arc::new(ManagedHost::new(
            host_id.to_string(),
            socket_path,
            channel,
            plugins,
            500,
        ));

        self.hosts
            .write()
            .await
            .insert(host_id.to_string(), managed_host.clone());

        tracing::info!(
            host_id = %host_id,
            "Externally registered host added to unified Supervisor registry"
        );

        Ok(HostRegistration::Registered(managed_host))
    }

    /// Retrieves an active managed host by its identifier.
    pub async fn get_host(&self, host_id: &str) -> Option<Arc<ManagedHost>> {
        self.hosts.read().await.get(host_id).cloned()
    }

    /// Returns a list of all currently active managed plugin hosts.
    pub async fn get_all_hosts(&self) -> Vec<Arc<ManagedHost>> {
        self.hosts.read().await.values().cloned().collect()
    }

    /// Starts the host watchdog, which notices crashed hosts and brings them back.
    ///
    /// Why this is separate from shutdown handling: a host can die at any time (panic, OOM,
    /// `SIGKILL`, a plugin's own `process.exit`). Without a monitor the supervisor keeps the dead
    /// host in its registry, so the console shows it as healthy and every routed event fails
    /// against a closed socket. The watchdog prunes the entry, relaunches from the recorded
    /// recipe with exponential backoff, and parks a repeatedly-crashing host as `crashed` so the
    /// fault is visible instead of silently looped.
    ///
    /// Disabled plugins are never restarted: their absence is intentional.
    pub fn spawn_host_watchdog(
        self: &Arc<Self>,
        plugin_state: Arc<ToggleStore>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut budgets: HashMap<String, RestartBudget> = HashMap::new();
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                ticker.tick().await;
                let hosts = supervisor.get_all_hosts().await.len();
                supervisor.watchdog_tick(&plugin_state, &mut budgets).await;
                // Debug-level heartbeat: a watchdog that stopped running would otherwise leave a
                // dead host advertised as healthy with nothing in the log to explain it.
                tracing::debug!(hosts, "Host watchdog pass complete");
            }
        })
    }

    /// One watchdog pass: inspects every host with a child process.
    async fn watchdog_tick(
        &self,
        plugin_state: &ToggleStore,
        budgets: &mut HashMap<String, RestartBudget>,
    ) {
        for host in self.get_all_hosts().await {
            let Some(plugin_id) = host.primary_plugin_id() else {
                continue;
            };

            let attempts = budgets
                .get(&host.host_id)
                .map(|budget| budget.attempts)
                .unwrap_or(0);

            if !plugin_state.is_enabled(PLUGIN_SECTION, &plugin_id).await {
                // The control plane stops disabled hosts; if one is still present, mark it so the
                // console shows why it is not serving.
                host.report_health("disabled", attempts, None).await;
                continue;
            }

            let exit_status = {
                let mut guard = host.child.lock().await;
                match guard.as_mut() {
                    // `try_wait` reaps the child; `None` means it is still running.
                    Some(child) => child.try_wait().ok().flatten(),
                    // Externally registered hosts have no child handle to observe.
                    None => continue,
                }
            };

            let Some(status) = exit_status else {
                // Still alive: a host that stayed up long enough has earned a fresh budget.
                let budget = budgets.entry(host.host_id.clone()).or_default();
                let healthy = budget
                    .last_attempt
                    .map(|at| at.elapsed() >= HOST_WATCHDOG_HEALTHY_AFTER)
                    .unwrap_or(true);
                if budget.attempts > 0 && healthy {
                    tracing::info!(
                        host_id = %host.host_id,
                        restarts = budget.attempts,
                        "Host stayed up after restart; clearing its restart budget"
                    );
                    budget.attempts = 0;
                    budget.next_at = None;
                }
                host.report_health("running", budget.attempts, None).await;
                continue;
            };

            tracing::error!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                status = %status,
                "Plugin host exited unexpectedly"
            );
            let failure = format!("exited with {status}");

            if !host.is_restartable() {
                tracing::error!(
                    host_id = %host.host_id,
                    "Host has no launch recipe; removing it from the registry instead of restarting"
                );
                let _ = self.stop_host(&host.host_id).await;
                continue;
            }

            let budget = budgets.entry(host.host_id.clone()).or_default();
            let now = Instant::now();

            if budget.attempts >= HOST_WATCHDOG_MAX_RESTARTS {
                // Park it: a plugin that dies immediately on every launch needs a human.
                if budget.next_at.is_none() {
                    tracing::error!(
                        host_id = %host.host_id,
                        plugin_id = %plugin_id,
                        attempts = budget.attempts,
                        "Host crashed after repeated restarts; parking it as crashed until an operator restarts it"
                    );
                    budget.next_at = Some(now);
                    host.report_health("crashed", budget.attempts, Some(failure.clone()))
                        .await;
                    let _ = self.stop_host(&host.host_id).await;
                }
                continue;
            }

            if let Some(next_at) = budget.next_at
                && next_at > now
            {
                host.report_health("restarting", budget.attempts, Some(failure.clone()))
                    .await;
                continue;
            }

            // Exponential backoff: 2s, 4s, 8s, 16s, capped at 30s.
            let backoff = Duration::from_secs(2u64.saturating_pow(budget.attempts).min(30));
            budget.attempts += 1;
            let attempt = budget.attempts;
            budget.last_attempt = Some(now);
            budget.next_at = Some(now + backoff);

            tracing::warn!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                attempt,
                max_attempts = HOST_WATCHDOG_MAX_RESTARTS,
                backoff_ms = backoff.as_millis() as u64,
                "Restarting crashed plugin host"
            );

            match self.restart_host(&host.host_id).await {
                Ok(restarted) => {
                    // Carry the attempt count forward so the console shows the full story.
                    restarted.report_health("running", attempt, None).await;
                }
                Err(err) => {
                    tracing::error!(
                        host_id = %host.host_id,
                        error = %err,
                        "Failed to restart crashed plugin host"
                    );
                    host.report_health("crashed", attempt, Some(err.to_string()))
                        .await;
                }
            }
        }
    }

    /// Stops a specific managed host and cleans up its socket.
    pub async fn stop_host(&self, host_id: &str) -> Result<(), SupervisorError> {
        let host = {
            let mut hosts = self.hosts.write().await;
            hosts.remove(host_id)
        };

        if let Some(host) = host {
            let mut child_guard = host.child.lock().await;
            if let Some(mut child) = child_guard.take() {
                // Graceful first: plugins flush state and close platform connections in
                // `on_unload`, which a bare kill would skip entirely.
                if let Err(err) = terminate_child(&mut child, HOST_SHUTDOWN_GRACE).await {
                    tracing::warn!(host_id = %host_id, error = %err, "Failed to terminate host cleanly");
                }
                tracing::info!(host_id = %host_id, "Host process terminated");
            } else {
                // Externally registered hosts have no child handle. They cannot be signalled, so
                // they rely on their own core-liveness watchdog to stop; the removal above means
                // the core no longer routes anything to them in the meantime.
                tracing::info!(
                    host_id = %host_id,
                    "Host had no child handle (externally registered); it stops via its own core-liveness watchdog"
                );
            }
            if host.socket_path.exists() {
                let _ = std::fs::remove_file(&host.socket_path);
                tracing::debug!(socket = %host.socket_path.display(), "Removed host socket");
            }
            Ok(())
        } else {
            Err(SupervisorError::HostNotFound(host_id.to_string()))
        }
    }

    /// Stops all managed host processes and cleans up runtime sockets.
    pub async fn stop_all(&self) -> Result<(), SupervisorError> {
        let host_ids: Vec<String> = self.hosts.read().await.keys().cloned().collect();
        for id in host_ids {
            let _ = self.stop_host(&id).await;
        }
        Ok(())
    }

    /// Polls the designated socket until the host server is ready to accept connections.
    async fn wait_for_readiness(
        &self,
        child: &mut Child,
        host_id: &str,
        socket_path: &Path,
        timeout: Duration,
    ) -> Result<Channel, SupervisorError> {
        let start = std::time::Instant::now();
        let interval = Duration::from_millis(50);

        while start.elapsed() < timeout {
            // First verify that the child process has not exited unexpectedly.
            if let Ok(Some(status)) = child.try_wait() {
                return Err(SupervisorError::PrematureExit {
                    host_id: host_id.to_string(),
                    status: status.to_string(),
                });
            }

            // Attempt to establish a test connection to the host socket.
            if socket_path.exists() {
                match connect_ipc(socket_path).await {
                    Ok(channel) => {
                        tracing::debug!(
                            host_id = %host_id,
                            elapsed_ms = start.elapsed().as_millis(),
                            "Socket ready and connection established"
                        );
                        return Ok(channel);
                    }
                    Err(_) => {
                        // Socket file exists but listener is not yet ready to accept connections.
                    }
                }
            }

            tokio::time::sleep(interval).await;
        }

        Err(SupervisorError::Timeout(host_id.to_string()))
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // As a safeguard against leftover socket files on unexpected drops:
        if self.core_sock_path.exists() {
            let _ = std::fs::remove_file(&self.core_sock_path);
        }
    }
}

/// Returns the interpreter of the plugin's own virtual environment (`<plugin>/.venv`).
///
/// `is_file` follows the venv's interpreter symlink, so an environment whose base Python was
/// removed counts as missing rather than failing later with a confusing spawn error.
fn plugin_python(plugin_dir: &Path) -> Option<PathBuf> {
    let python = if cfg!(windows) {
        plugin_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        plugin_dir.join(".venv").join("bin").join("python")
    };
    python.is_file().then_some(python)
}

/// Fails when the plugin's `package.json` declares dependencies that are not installed.
///
/// Node resolves a plugin's imports from `<plugin>/node_modules`, so that directory is the
/// plugin's environment. A plugin without `package.json` or without dependencies needs none.
fn ensure_node_modules(plugin_dir: &Path) -> Result<(), String> {
    let manifest = plugin_dir.join("package.json");
    let content = match std::fs::read_to_string(&manifest) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read '{}': {error}", manifest.display())),
    };
    let package: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("invalid '{}': {error}", manifest.display()))?;
    let has_dependencies = package
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|dependencies| !dependencies.is_empty());
    if has_dependencies && !plugin_dir.join("node_modules").is_dir() {
        return Err(format!(
            "dependencies in '{}' are not installed; run `npm install` or `bun install` in the plugin directory",
            manifest.display()
        ));
    }
    Ok(())
}

/// Searches the system PATH environment variable for a given executable name.
fn find_binary_in_path(bin_name: &str) -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let full = path.join(bin_name);
            if full.is_file() {
                return Some(full);
            }
            #[cfg(windows)]
            {
                let full_exe = path.join(format!("{bin_name}.exe"));
                if full_exe.is_file() {
                    return Some(full_exe);
                }
            }
        }
    }
    None
}

/// Searches upwards from a starting directory for a relative target file path (up to 6 levels).
fn find_file_upwards(start: &Path, rel_path: &str) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    for _ in 0..6 {
        let candidate = current.join(rel_path);
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}
