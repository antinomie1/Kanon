//! Sub-process plugin host supervisor and lifecycle manager.
//!
//! Responsible for spawning child plugin processes, managing per-host IPC endpoints
//! (`./run/host_<id>.sock`), verifying socket readiness, conducting initial
//! `GetPluginMeta` handshakes, and ensuring graceful process termination and cleanup.

mod host;
pub use host::with_tool_event;
mod config;
mod launch;
mod watchdog;
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
mod deps;
pub use deps::{DEFAULT_INSTALL_TIMEOUT, DependencyInstaller};

/// Maximum time a command, trigger or captured reply may occupy an inbound chat lane.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Selects plugin hosts using one snapshot of global toggles and an optional instance policy.
///
/// All model and pipeline entry points share this selection: a registered process may still exist
/// while disabled, but must not supply tools, commands or hooks. Without a toggle store, plugins
/// default to globally enabled; an explicit instance restriction still applies.
pub async fn filter_plugin_hosts(
    hosts: Vec<Arc<ManagedHost>>,
    toggles: Option<&ToggleStore>,
    instance: Option<&crate::instance::BotInstance>,
) -> Vec<Arc<ManagedHost>> {
    let disabled: HashSet<String> = match toggles {
        Some(toggles) => toggles
            .disabled_ids(PLUGIN_SECTION)
            .await
            .into_iter()
            .collect(),
        None => HashSet::new(),
    };
    hosts
        .into_iter()
        .filter(|host| {
            let plugin_id = host.primary_plugin_id().unwrap_or_default();
            let globally_enabled = !disabled.contains(&plugin_id);
            instance.map_or(globally_enabled, |instance| {
                instance.allows_plugin(&plugin_id, globally_enabled)
            })
        })
        .collect()
}

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
    #[error("Host '{0}' timed out waiting for readiness or metadata")]
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
    /// The host already runs or another operation currently owns its launch.
    #[error("Host '{0}' is already running or being launched")]
    HostBusy(String),
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

/// Exclusive access to one plugin's configuration value and version until the complete commit ends.
///
/// The control plane holds this guard while saving or restoring the file. Readers acquire the
/// same guard before loading the file, so they cannot pair old values with a newly applied version.
pub struct PluginConfigGuard {
    plugin_id: String,
    version: tokio::sync::OwnedMutexGuard<u64>,
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
    host_client: PluginHostServiceClient<kanon_transport::AuthenticatedChannel>,
    /// Client for event and message dispatching (`MessagePipelineService`), cloned per call.
    pipeline_client: MessagePipelineServiceClient<kanon_transport::AuthenticatedChannel>,
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
    ipc_token: String,
    /// Registry of active managed plugin host instances.
    hosts: Arc<RwLock<HashMap<String, Arc<ManagedHost>>>>,
    /// Registry of in-process platform adapters.
    adapters: Arc<AdapterRegistry>,
    /// Per-plugin transaction locks and configuration versions; unrelated hosts never share an RPC lock.
    config_versions: Arc<RwLock<HashMap<String, Arc<Mutex<u64>>>>>,
    /// Plugins whose launch was prevented or deferred due to missing runtime environments.
    unavailable_plugins: Arc<RwLock<HashMap<String, UnavailablePlugin>>>,
    /// Interpreter for TypeScript plugins; `bun`, then `node`, from `PATH` when unset.
    typescript_runtime: Option<PathBuf>,
    /// Installs plugins' dependencies before launch; `None` only checks that they are present.
    dependencies: Option<Arc<DependencyInstaller>>,
    /// Host ids this supervisor is launching or attaching right now (see [`LaunchGuard`]).
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
    /// A launch or external attachment owns the handshake and will publish the registry entry.
    Launching,
}

/// Reserves a host id throughout process launch or external attachment.
///
/// Dropping clears the mark on every exit of the launch, including early returns and failures,
/// so a host whose launch failed is never mistaken for one still starting.
struct LaunchGuard {
    launching: Arc<std::sync::Mutex<HashSet<String>>>,
    host_id: String,
}

impl LaunchGuard {
    fn new(
        launching: &Arc<std::sync::Mutex<HashSet<String>>>,
        host_id: &str,
    ) -> Result<Self, SupervisorError> {
        if !lock_launching(launching).insert(host_id.to_string()) {
            return Err(SupervisorError::HostBusy(host_id.to_string()));
        }
        Ok(Self {
            launching: launching.clone(),
            host_id: host_id.to_string(),
        })
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
            ipc_token: String::new(),
            hosts: Arc::new(RwLock::new(HashMap::new())),
            adapters: Arc::new(AdapterRegistry::new()),
            config_versions: Arc::new(RwLock::new(HashMap::new())),
            unavailable_plugins: Arc::new(RwLock::new(HashMap::new())),
            typescript_runtime: None,
            dependencies: None,
            launching: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }

    /// Shares the node's per-process Windows IPC authentication credential.
    pub fn with_ipc_token(mut self, token: String) -> Self {
        self.ipc_token = token;
        self
    }

    /// Uses `runtime` (a `bun` or `node` binary) for TypeScript plugins instead of the `PATH`
    /// lookup, as the node's `startup.typescript_runtime` setting asks.
    pub fn with_typescript_runtime(mut self, runtime: Option<PathBuf>) -> Self {
        self.typescript_runtime = runtime;
        self
    }

    /// Installs Python and TypeScript plugins' dependencies with their native tool before each
    /// launch when their environment is missing or out of date (see [`DependencyInstaller`]).
    /// Without an installer a launch only checks that the environment exists.
    pub fn with_dependency_installer(mut self, installer: Option<DependencyInstaller>) -> Self {
        self.dependencies = installer.map(Arc::new);
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
        self.lock_plugin_config(plugin_id).await.version()
    }

    /// Locks one plugin's complete configuration transaction, including persistence by the caller.
    pub async fn lock_plugin_config(&self, plugin_id: &str) -> PluginConfigGuard {
        let lock = self
            .config_versions
            .write()
            .await
            .entry(plugin_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(0)))
            .clone();
        PluginConfigGuard {
            plugin_id: plugin_id.to_string(),
            version: lock.lock_owned().await,
        }
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

        for host in self.get_all_hosts().await {
            let platforms = host.adapter_platforms();
            if platforms.is_empty() {
                continue;
            }

            let display_name = host
                .adapter_display_name()
                .unwrap_or_else(|| host.host_id.clone());
            let plugin_id = host.adapter_plugin_id();
            let connected = host.health().await.state == "running";

            for platform in platforms {
                catalog.push(AdapterDescriptor {
                    platform,
                    display_name: display_name.clone(),
                    kind: AdapterKind::Plugin,
                    // Failed restarts retain their registry entry and recipe for recovery.
                    connected,
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
        let mut transaction = self.lock_plugin_config(plugin_id).await;
        transaction.check_version(expected_version)?;
        let host = self
            .find_host_for_plugin(plugin_id)
            .await
            .ok_or_else(|| SupervisorError::PluginNotFound(plugin_id.to_string()))?;
        transaction.reload(&host, config).await
    }

    /// Fetches a host's plugin metadata again, for a plugin that changed its tools, commands or
    /// triggers at runtime; returns the plugin ids the host now declares.
    ///
    /// The new metadata applies from the next turn on. A host that stops declaring a plugin it
    /// declared before is refused and keeps its previous metadata: a plugin vanishing from
    /// routing and the console is a restart's business, not a refresh's.
    ///
    /// The host is asked while its own `RefreshPluginMeta` call is still waiting, so a host must
    /// serve `GetPluginMeta` concurrently with its outgoing calls (every SDK does).
    pub async fn refresh_plugin_meta(&self, host_id: &str) -> Result<Vec<String>, SupervisorError> {
        let host = self
            .get_host(host_id)
            .await
            .ok_or_else(|| SupervisorError::HostNotFound(host_id.to_string()))?;
        let metas = host.get_plugin_meta().await?;
        if let Some(missing) = host
            .metas()
            .into_iter()
            .find(|previous| !metas.iter().any(|meta| meta.id == previous.id))
        {
            return Err(SupervisorError::PluginNotFound(missing.id));
        }
        let plugin_ids = metas.iter().map(|meta| meta.id.clone()).collect();
        host.set_metas(metas);
        tracing::info!(host_id = %host_id, "Plugin metadata refreshed at the plugin's request");
        Ok(plugin_ids)
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
        // External attachment owns the same reservation as process launch through its entire
        // handshake. Checking only before the awaits would let a later spawn publish its child,
        // then have this slower attachment overwrite that child and its restart recipe.
        let _launching = match LaunchGuard::new(&self.launching, host_id) {
            Ok(launching) => launching,
            Err(SupervisorError::HostBusy(_)) => {
                tracing::info!(
                    host_id = %host_id,
                    "Host registration is already in progress; its owner completes the handshake"
                );
                return Ok(HostRegistration::Launching);
            }
            Err(error) => return Err(error),
        };
        if let Some(existing) = self.get_host(host_id).await {
            tracing::info!(
                host_id = %host_id,
                "Host already known to Supervisor; keeping existing registration"
            );
            return Ok(HostRegistration::Registered(existing));
        }

        // Registration is all-or-nothing: an unresponsive or incompatible host must not become
        // routable with guessed metadata. The deadline includes transport readiness and the RPC.
        let managed_host = tokio::time::timeout(Duration::from_secs(5), async {
            let socket_path = PathBuf::from(endpoint);
            let channel = connect_ipc(&socket_path).await?;
            let mut managed_host = ManagedHost::new(
                host_id.to_string(),
                socket_path,
                channel.clone(),
                Vec::new(),
                500,
            );
            managed_host.host_client = PluginHostServiceClient::with_interceptor(
                channel.clone(),
                kanon_transport::ClientAuthInterceptor(self.ipc_token.clone()),
            );
            managed_host.pipeline_client = MessagePipelineServiceClient::with_interceptor(
                channel,
                kanon_transport::ClientAuthInterceptor(self.ipc_token.clone()),
            );
            let plugins = managed_host.get_plugin_meta().await?;
            for id in loaded_plugin_ids {
                if !plugins.iter().any(|plugin| &plugin.id == id) {
                    return Err(SupervisorError::PluginNotFound(id.clone()));
                }
            }
            managed_host.set_metas(plugins);
            Ok::<_, SupervisorError>(Arc::new(managed_host))
        })
        .await
        .map_err(|_| SupervisorError::Timeout(host_id.to_string()))??;

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

    /// Polls the designated socket until the host server is ready to accept connections.
    pub(super) async fn wait_for_readiness(
        &self,
        child: &mut Child,
        host_id: &str,
        socket_path: &Path,
        deadline: tokio::time::Instant,
    ) -> Result<Channel, SupervisorError> {
        let start = std::time::Instant::now();
        let interval = Duration::from_millis(50);

        while tokio::time::Instant::now() < deadline {
            // First verify that the child process has not exited unexpectedly.
            if let Some(status) = child.try_wait()? {
                return Err(SupervisorError::PrematureExit {
                    host_id: host_id.to_string(),
                    status: status.to_string(),
                });
            }

            // Attempt to establish a test connection to the host socket.
            if socket_path.exists() {
                match tokio::time::timeout_at(deadline, connect_ipc(socket_path)).await {
                    Ok(Ok(channel)) => {
                        tracing::debug!(
                            host_id = %host_id,
                            elapsed_ms = start.elapsed().as_millis(),
                            "Socket ready and connection established"
                        );
                        return Ok(channel);
                    }
                    Ok(Err(_)) => {
                        // Socket file exists but listener is not yet ready to accept connections.
                    }
                    Err(_) => return Err(SupervisorError::Timeout(host_id.to_string())),
                }
            }

            tokio::time::sleep(interval).await;
        }

        Err(SupervisorError::Timeout(host_id.to_string()))
    }
}

/// Fails when the plugin's `package.json` declares dependencies that are not installed.
///
/// Node resolves a plugin's imports from `<plugin>/node_modules`, so that directory is the
/// plugin's environment. A plugin without `package.json` or without dependencies needs none.
fn ensure_node_modules(plugin_dir: &Path) -> Result<(), String> {
    if deps::declares_node_dependencies(plugin_dir)? && !plugin_dir.join("node_modules").is_dir() {
        return Err(format!(
            "dependencies in '{}' are not installed; run `npm install` or `bun install` in the plugin directory",
            plugin_dir.join("package.json").display()
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
