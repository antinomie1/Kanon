//! MCP (Model Context Protocol) servers as tool sources.
//!
//! # What this is
//! An MCP server exposes tools over JSON-RPC: over a child process' stdio (`npx`, `uvx`, any
//! script) or over HTTP. Rather than inventing a parallel tool path, a server is adapted to the
//! existing [`ToolHost`] contract, so its tools are aggregated, namespaced, circuit-broken and
//! routed exactly like plugin tools — the model cannot tell the difference.
//!
//! Tools are exposed as `mcp__<server>__<tool>`: the prefix keeps them recognisable in logs and
//! prevents an MCP tool from silently shadowing a plugin tool with the same name.
//!
//! # Lifecycle
//! Servers are connected lazily on first use and probed by [`McpPool::spawn_watchdog`]. A server
//! that stops answering is marked `reconnecting` (or `failed` once its budget is exhausted) and
//! its process is replaced on the next successful probe, so a crashed MCP server degrades to
//! "its tools are unavailable" instead of breaking every conversation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kanon_llm::tool_router::{ToolAttachment, ToolHost, json_to_prost_struct};
use kanon_proto::v1::{PluginMeta, ToolCallRequest, ToolCallResponse, ToolMeta};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};

use crate::instance::BotInstance;
use crate::toggle::{MCP_SECTION, ToggleStore};

/// Default MCP configuration path, relative to the node working directory.
pub const DEFAULT_MCP_CONFIG: &str = "./data/mcp.json";

/// Per-request deadline for an MCP call.
pub const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the MCP watchdog probes every configured server.
pub const MCP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);

/// Consecutive failed probes before a server is parked as failed.
pub const MCP_MAX_FAILURES: u32 = 3;

/// Default directory receiving attachments materialized from tool results.
pub const DEFAULT_ATTACHMENT_DIR: &str = "./data/attachments";

/// How long an attachment file is kept before the next node start sweeps it.
///
/// An attachment only needs to outlive the delivery attempt that follows the tool call; keeping
/// them forever would grow the data directory by one image per call.
pub const ATTACHMENT_RETENTION: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// Largest attachment the core forwards to an outbound message.
///
/// A tool result is transport for a user-visible file, not a data channel: anything larger would
/// sit in memory and in the platform upload path for no benefit.
pub const MCP_MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

/// Largest number of attachments forwarded from one tool result.
pub const MCP_MAX_ATTACHMENTS: usize = 4;

/// Longest raw JSON fallback handed to the model when a tool replies with neither text nor
/// attachments.
///
/// MCP servers may answer with structured content of any size; passing it through unbounded would
/// let a single call consume the whole context window.
pub const MCP_FALLBACK_TEXT_LIMIT: usize = 2000;

/// Failures raised while configuring or talking to MCP servers.
#[derive(Debug, Error)]
pub enum McpError {
    /// The server is not configured on this node.
    #[error("MCP server '{0}' is not configured")]
    NotFound(String),
    /// The configuration document could not be read or written.
    #[error("MCP configuration failed: {0}")]
    Config(String),
    /// A supplied server definition violates the configuration contract.
    #[error("Invalid MCP configuration: {0}")]
    InvalidConfig(String),
    /// The transport could not be established.
    #[error("MCP transport failed: {0}")]
    Transport(String),
    /// The server answered with a JSON-RPC error.
    #[error("MCP server '{server}' rejected '{method}': {message}")]
    Rpc {
        /// Server identifier.
        server: String,
        /// Method that failed.
        method: String,
        /// Error message reported by the server.
        message: String,
    },
}

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Server runs as a child process speaking JSON-RPC on stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments passed to it.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment variables.
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Server answers JSON-RPC over HTTP POST.
    Http {
        /// Endpoint URL.
        url: String,
        /// Extra headers (authorization, etc.).
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

/// One configured MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Stable identifier, also used in tool names.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Transport configuration.
    pub transport: McpTransport,
}

/// Document persisted at the MCP configuration path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct McpDocument {
    /// Schema version.
    #[serde(default = "default_version")]
    version: u32,
    /// Configured servers.
    #[serde(default)]
    servers: Vec<McpServerConfig>,
    /// Unrecognized keys are preserved verbatim.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// Current MCP schema version.
fn default_version() -> u32 {
    1
}

/// Persisted MCP configuration.
#[derive(Debug)]
pub struct McpConfigStore {
    /// Path of the configuration document; `None` for an in-memory store.
    path: Option<PathBuf>,
    /// Configured servers by identifier.
    servers: RwLock<HashMap<String, McpServerConfig>>,
    /// Complete API mutations of the same server share one guard, including runtime effects.
    operations: std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
}

impl Default for McpConfigStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl McpConfigStore {
    /// Creates a configuration that lives only for this process.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            servers: RwLock::new(HashMap::new()),
            operations: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Opens (or creates) the configuration document.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, McpError> {
        let path = path.into();
        let document = read_document(&path)?;
        let servers = document
            .map(|document| {
                document
                    .servers
                    .into_iter()
                    .map(|server| (server.id.clone(), server))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            path: Some(path),
            servers: RwLock::new(servers),
            operations: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Serializes one server's configuration, toggle and runtime update until its caller commits.
    pub async fn lock_server(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let operation = {
            let mut operations = self
                .operations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            operations.retain(|_, operation| operation.strong_count() > 0);
            match operations.get(id).and_then(std::sync::Weak::upgrade) {
                Some(operation) => operation,
                None => {
                    let operation = Arc::new(Mutex::new(()));
                    operations.insert(id.to_string(), Arc::downgrade(&operation));
                    operation
                }
            }
        };
        operation.lock_owned().await
    }

    /// Lists configured servers, ordered by identifier.
    pub async fn list(&self) -> Vec<McpServerConfig> {
        let mut servers: Vec<McpServerConfig> =
            self.servers.read().await.values().cloned().collect();
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        servers
    }

    /// Returns one server configuration.
    pub async fn get(&self, id: &str) -> Option<McpServerConfig> {
        self.servers.read().await.get(id).cloned()
    }

    /// Inserts or replaces a server configuration.
    pub async fn upsert(&self, config: McpServerConfig) -> Result<(), McpError> {
        validate_config(&config)?;
        let mut servers = self.servers.write().await;
        // Staged on a copy and swapped in only after the write succeeds, so a failed write never
        // leaves the node running a server list the file does not record.
        let mut next = servers.clone();
        next.insert(config.id.clone(), config);
        self.persist(&next)?;
        *servers = next;
        Ok(())
    }

    /// Removes a server configuration.
    pub async fn remove(&self, id: &str) -> Result<bool, McpError> {
        let mut servers = self.servers.write().await;
        let mut next = servers.clone();
        let removed = next.remove(id).is_some();
        if removed {
            self.persist(&next)?;
            *servers = next;
        }
        Ok(removed)
    }

    /// Atomically writes the configuration with owner-only permissions.
    fn persist(&self, servers: &HashMap<String, McpServerConfig>) -> Result<(), McpError> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                McpError::Config(format!("failed to create {}: {err}", parent.display()))
            })?;
        }

        let mut document = read_document(path)?.unwrap_or_default();
        document.version = default_version();
        let mut ordered: Vec<McpServerConfig> = servers.values().cloned().collect();
        ordered.sort_by(|a, b| a.id.cmp(&b.id));
        document.servers = ordered;

        let payload = serde_json::to_string_pretty(&document)
            .map_err(|err| McpError::Config(format!("failed to serialize MCP config: {err}")))?;

        let temp_path = path.with_extension("json.tmp");
        std::fs::write(&temp_path, payload).map_err(|err| {
            McpError::Config(format!("failed to write {}: {err}", temp_path.display()))
        })?;

        // The document may carry server credentials in headers/env.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600)).map_err(
                |err| {
                    McpError::Config(format!("failed to restrict {}: {err}", temp_path.display()))
                },
            )?;
        }

        std::fs::rename(&temp_path, path).map_err(|err| {
            McpError::Config(format!(
                "failed to move {} into place at {}: {err}",
                temp_path.display(),
                path.display()
            ))
        })
    }
}

/// Validates a server description before it is stored.
fn validate_config(config: &McpServerConfig) -> Result<(), McpError> {
    let id = config.id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(McpError::InvalidConfig(format!(
            "invalid MCP server id '{}': use letters, digits, '-' or '_'",
            config.id
        )));
    }
    if config.name.trim().is_empty() {
        return Err(McpError::InvalidConfig(
            "server name must not be empty".into(),
        ));
    }
    match &config.transport {
        McpTransport::Stdio { command, .. } if command.trim().is_empty() => Err(
            McpError::InvalidConfig("stdio command must not be empty".into()),
        ),
        McpTransport::Http { url, .. }
            if !url.starts_with("http://") && !url.starts_with("https://") =>
        {
            Err(McpError::InvalidConfig(
                "HTTP MCP url must start with http:// or https://".into(),
            ))
        }
        _ => Ok(()),
    }
}

/// Reads the configuration document, returning `None` when it does not exist.
fn read_document(path: &Path) -> Result<Option<McpDocument>, McpError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| McpError::Config(format!("failed to read {}: {err}", path.display())))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|err| McpError::Config(format!("failed to parse {}: {err}", path.display())))
}

/// Live connection to one MCP server.
enum Connection {
    /// Child process speaking JSON-RPC on stdio.
    Stdio {
        /// Kill wrappers and their server descendants before dropping the direct child handle.
        #[cfg(unix)]
        _process_group: crate::process::ProcessGroup,
        /// Child process handle.
        ///
        /// Never read directly, but it must stay alive: dropping it closes the pipes and, with
        /// `kill_on_drop`, stops the server process, which is exactly what reconnecting needs.
        #[allow(dead_code)]
        child: Child,
        /// Line reader over the child's stdout.
        stdout: BufReader<tokio::process::ChildStdout>,
        /// Child's stdin for outgoing requests.
        stdin: tokio::process::ChildStdin,
    },
    /// HTTP endpoint.
    Http {
        /// Shared HTTP client.
        client: reqwest::Client,
        /// Endpoint URL.
        url: String,
        /// Extra headers.
        headers: HashMap<String, String>,
        /// Session assigned by `initialize`, sent on every subsequent request and notification.
        session_id: Option<String>,
    },
}

/// Runtime health of one MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHealth {
    /// `connected`, `connecting`, `reconnecting`, `failed` or `disabled`.
    pub state: String,
    /// Tools currently advertised by the server.
    pub tools: usize,
    /// Consecutive failed probes.
    pub failures: u32,
    /// Last observed error, when any.
    pub last_error: Option<String>,
}

impl Default for McpHealth {
    fn default() -> Self {
        Self {
            state: "connecting".to_string(),
            tools: 0,
            failures: 0,
            last_error: None,
        }
    }
}

/// Result of one MCP tool call: the text for the model plus any rich media to deliver.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpToolOutcome {
    /// Text handed to the model as the tool result.
    pub text: String,
    /// Files materialized from the result so they can be attached to the reply.
    pub attachments: Vec<ToolAttachment>,
}

/// One MCP server, adapted to the [`ToolHost`] contract.
pub struct McpServer {
    /// Static configuration.
    config: McpServerConfig,
    /// The pool and control plane share this single source of global enablement.
    toggles: Arc<ToggleStore>,
    /// Host identifier used in tool metadata (`mcp_<server>`), stable for the server's lifetime.
    host_id: String,
    /// Live connection, absent until the first successful connect.
    connection: Mutex<Option<Connection>>,
    /// Removed or replaced definitions never reconnect through an old turn's retained handle.
    retired: std::sync::atomic::AtomicBool,
    /// Synthetic plugin metadata advertised to the tool router.
    ///
    /// A synchronous lock: [`ToolHost::plugin_metas`] is a synchronous trait method, so the
    /// metadata must be readable without awaiting. Critical sections only clone a `Vec`.
    meta: std::sync::RwLock<Vec<PluginMeta>>,
    /// Health snapshot for the console.
    health: Mutex<McpHealth>,
    /// Monotonic JSON-RPC request id.
    next_request_id: std::sync::atomic::AtomicU64,
    /// Directory receiving attachments materialized from tool results.
    attachment_dir: PathBuf,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("id", &self.config.id)
            .finish_non_exhaustive()
    }
}

impl McpServer {
    /// Creates a server handle from its configuration.
    pub fn new(config: McpServerConfig, toggles: Arc<ToggleStore>) -> Self {
        let host_id = host_id(&config.id);
        Self {
            config,
            toggles,
            host_id,
            connection: Mutex::new(None),
            retired: std::sync::atomic::AtomicBool::new(false),
            meta: std::sync::RwLock::new(Vec::new()),
            health: Mutex::new(McpHealth::default()),
            next_request_id: std::sync::atomic::AtomicU64::new(1),
            attachment_dir: PathBuf::from(DEFAULT_ATTACHMENT_DIR),
        }
    }

    /// Overrides where attachments from this server are written.
    ///
    /// The pool points every server at the node's data directory; tests point them at a temporary
    /// directory so a tool result can never litter the repository.
    pub fn with_attachment_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.attachment_dir = dir.into();
        self
    }

    /// Configuration of this server.
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    /// Current health snapshot.
    pub async fn health(&self) -> McpHealth {
        self.health.lock().await.clone()
    }

    /// Whether a transport is currently open.
    pub async fn is_connected(&self) -> bool {
        self.connection.lock().await.is_some()
    }

    /// One watchdog pass: proves the server still answers and keeps health up to date.
    ///
    /// A server that has never been used (or was dropped after a failure) is *connected* rather
    /// than probed: reporting "not connected" as a failure would park a perfectly healthy server
    /// as broken simply because no conversation had needed it yet. Health bookkeeping happens
    /// exactly once per pass, inside [`McpServer::connect`] or here.
    pub async fn probe(&self) -> Result<(), McpError> {
        self.connect_or_refresh(true).await
    }

    /// Connects (if needed), performs the MCP handshake and refreshes the tool list.
    pub async fn connect(&self) -> Result<(), McpError> {
        self.connect_or_refresh(false).await
    }

    /// Serializes initialization, probes and disconnection on the transport's existing lock.
    /// A candidate stays local until its handshake and tool discovery succeed; cancellation
    /// therefore drops its child instead of leaving a half-initialized connection published.
    async fn connect_or_refresh(&self, refresh: bool) -> Result<(), McpError> {
        let mut current = self.connection.lock().await;
        self.ensure_available().await?;
        if current.is_some() && !refresh {
            return Ok(());
        }

        let attempt = if let Some(connection) = current.as_mut() {
            // A probe refreshes an already initialized transport. Keep its committed metadata
            // while the RPC runs so concurrent tool discovery never sees an empty catalog.
            // Cancelling this read leaves the previous connected state intact.
            self.read_tools(connection)
                .await
                .map(|metadata| (None, metadata))
        } else {
            let mut health = self.health.lock().await;
            // No transport is published yet. This remains truthful even when the caller drops
            // the initialization future, with no detached cleanup task or extra state machine.
            health.state = "disconnected".to_string();
            health.tools = 0;
            drop(health);
            self.clear_metadata();
            async {
                let mut connection = self.open_transport().await?;
                self.handshake(&mut connection).await?;
                let metadata = self.read_tools(&mut connection).await?;
                Ok::<_, McpError>((Some(connection), metadata))
            }
            .await
        };

        let mut health = self.health.lock().await;
        // A pool edit can retire this handle during the RPC. Check again at the commit boundary;
        // there is no await between this check and publication of transport, metadata and health.
        let attempt = match attempt {
            Ok(ready) => self.ensure_available().await.map(|()| ready),
            Err(error) => Err(error),
        };
        match attempt {
            Ok((connection, metadata)) => {
                health.state = "connected".to_string();
                health.failures = 0;
                health.last_error = None;
                health.tools = metadata.first().map_or(0, |meta| meta.tools.len());
                *self
                    .meta
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = metadata;
                if let Some(connection) = connection {
                    *current = Some(connection);
                }
                Ok(())
            }
            Err(err) => {
                *current = None;
                self.clear_metadata();
                health.failures += 1;
                health.state = if health.failures >= MCP_MAX_FAILURES {
                    "failed".to_string()
                } else {
                    "reconnecting".to_string()
                };
                health.last_error = Some(err.to_string());
                health.tools = 0;
                Err(err)
            }
        }
    }

    /// Refuses stale handles retained by a turn after their definition was replaced or removed.
    fn ensure_active(&self) -> Result<(), McpError> {
        if self.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(McpError::Transport(format!(
                "MCP server '{}' was removed or replaced",
                self.config.id
            )));
        }
        Ok(())
    }

    /// Rechecks the shared switch inside the connection boundary, including for retained tools.
    async fn ensure_available(&self) -> Result<(), McpError> {
        self.ensure_active()?;
        if !self.toggles.is_enabled(MCP_SECTION, &self.config.id).await {
            return Err(McpError::Transport(format!(
                "MCP server '{}' is disabled",
                self.config.id
            )));
        }
        self.ensure_active()
    }

    /// Clears tool advertisement whenever no initialized transport is published.
    fn clear_metadata(&self) {
        self.meta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Drops the live connection and marks the server as not currently connected.
    ///
    /// Used by the watchdog before reconnecting and by the console when a server is switched off;
    /// the connection is genuinely gone either way, so the health snapshot must say so.
    pub async fn disconnect(&self) {
        let mut connection = self.connection.lock().await;
        let mut health = self.health.lock().await;
        *connection = None;
        self.clear_metadata();
        health.state = "disconnected".to_string();
        health.tools = 0;
    }

    /// Opens the configured transport.
    async fn open_transport(&self) -> Result<Connection, McpError> {
        match &self.config.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = Command::new(command);
                cmd.args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                for (key, value) in env {
                    cmd.env(key, value);
                }
                #[cfg(unix)]
                cmd.process_group(0);

                let mut child = cmd.spawn().map_err(|err| {
                    McpError::Transport(format!(
                        "failed to start MCP server '{}' ({}): {err}",
                        self.config.id, command
                    ))
                })?;
                #[cfg(unix)]
                let process_group = crate::process::ProcessGroup(
                    child.id().expect("a newly spawned MCP process has a PID"),
                );

                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| McpError::Transport("child stdin unavailable".into()))?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| McpError::Transport("child stdout unavailable".into()))?;

                Ok(Connection::Stdio {
                    #[cfg(unix)]
                    _process_group: process_group,
                    child,
                    stdout: BufReader::new(stdout),
                    stdin,
                })
            }
            McpTransport::Http { url, headers } => Ok(Connection::Http {
                client: reqwest::Client::new(),
                url: url.clone(),
                headers: headers.clone(),
                session_id: None,
            }),
        }
    }

    /// Performs the MCP `initialize` handshake.
    async fn handshake(&self, connection: &mut Connection) -> Result<(), McpError> {
        self.request_on(
            connection,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "kanon", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;

        // No JSON-RPC response is expected, but delivery must still succeed before tools are used.
        self.notify(
            connection,
            "notifications/initialized",
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    /// Refreshes the advertised tool list and rebuilds the synthetic metadata.
    pub async fn refresh_tools(&self) -> Result<(), McpError> {
        self.connect_or_refresh(true).await
    }

    /// Reads candidate metadata without exposing it before the connection commit.
    async fn read_tools(&self, connection: &mut Connection) -> Result<Vec<PluginMeta>, McpError> {
        let response = self
            .request_on(connection, "tools/list", serde_json::json!({}))
            .await?;
        let tools = response
            .get("tools")
            .and_then(|tools| tools.as_array())
            .cloned()
            .unwrap_or_default();

        let mut metas = Vec::with_capacity(tools.len());
        for tool in &tools {
            let Some(name) = tool.get("name").and_then(|name| name.as_str()) else {
                continue;
            };
            let description = tool
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .map(|schema| json_to_prost_struct(&schema).unwrap_or_default());

            metas.push(ToolMeta {
                name: qualified_tool_name(&self.config.id, name),
                description,
                parameters,
            });
        }

        Ok(vec![PluginMeta {
            id: host_id(&self.config.id),
            name: self.config.name.clone(),
            version: "mcp".to_string(),
            author: "MCP".to_string(),
            description: format!("Model Context Protocol server '{}'", self.config.name),
            commands: Vec::new(),
            tools: metas,
            triggers: Vec::new(),
            events: Vec::new(),
            decorates_replies: false,
            prepares_turns: false,
            rewrites_system_prompt: false,
            serves_http: false,
        }])
    }

    /// Calls one tool by its *MCP* name (unqualified).
    pub async fn call(
        &self,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolOutcome, McpError> {
        self.connect().await?;

        let response = self
            .request(
                "tools/call",
                serde_json::json!({ "name": tool, "arguments": arguments }),
            )
            .await?;

        let mut texts: Vec<String> = Vec::new();
        // Reasons an attachment was dropped travel with the text: a missing picture must be
        // visible to the operator instead of silently vanishing from the conversation.
        let mut notes: Vec<String> = Vec::new();
        let mut attachments: Vec<ToolAttachment> = Vec::new();

        if let Some(items) = response.get("content").and_then(|value| value.as_array()) {
            for item in items {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                            texts.push(text.to_string());
                        }
                    }
                    Some("image") | Some("audio") => {
                        let mime = item
                            .get("mimeType")
                            .and_then(|value| value.as_str())
                            .unwrap_or("application/octet-stream");
                        let Some(data) = item.get("data").and_then(|value| value.as_str()) else {
                            notes
                                .push(format!("[attachment skipped: {mime} item carries no data]"));
                            continue;
                        };
                        self.collect_attachment(&mut attachments, &mut notes, mime, data, None);
                    }
                    // An embedded resource is either readable text for the model or a file
                    // (`blob`, base64) for the user, named after the last component of its URI.
                    Some("resource") => {
                        let Some(resource) = item.get("resource") else {
                            continue;
                        };
                        if let Some(text) = resource.get("text").and_then(|value| value.as_str()) {
                            texts.push(text.to_string());
                        } else if let Some(blob) =
                            resource.get("blob").and_then(|value| value.as_str())
                        {
                            let mime = resource
                                .get("mimeType")
                                .and_then(|value| value.as_str())
                                .unwrap_or("application/octet-stream");
                            let name = resource
                                .get("uri")
                                .and_then(|value| value.as_str())
                                .and_then(|uri| resource_file_name(uri, mime));
                            self.collect_attachment(
                                &mut attachments,
                                &mut notes,
                                mime,
                                blob,
                                name.as_deref(),
                            );
                        }
                    }
                    // Other content kinds (resource links, ...) are ignored rather than guessed
                    // at; the text items still describe the result.
                    _ => {}
                }
            }
        }

        texts.extend(notes);
        let text = if !texts.is_empty() {
            texts.join("\n")
        } else if !attachments.is_empty() {
            let kinds: Vec<&str> = attachments
                .iter()
                .map(|attachment| attachment.mime_type.as_str())
                .collect();
            format!(
                "[tool returned {} attachment(s): {}]",
                attachments.len(),
                kinds.join(", ")
            )
        } else {
            // Structured content the model can still read, bounded so one call cannot consume the
            // whole context window.
            truncate_for_model(&response.to_string(), MCP_FALLBACK_TEXT_LIMIT)
        };

        Ok(McpToolOutcome { text, attachments })
    }

    /// Stores one base64 attachment unless the per-call limit is reached; every skip becomes a
    /// note in the tool result.
    fn collect_attachment(
        &self,
        attachments: &mut Vec<ToolAttachment>,
        notes: &mut Vec<String>,
        mime: &str,
        data: &str,
        name: Option<&str>,
    ) {
        if attachments.len() >= MCP_MAX_ATTACHMENTS {
            notes.push(format!(
                "[attachment skipped: at most {MCP_MAX_ATTACHMENTS} attachments are forwarded per call]"
            ));
            return;
        }
        match self.store_attachment(mime, data, name) {
            Ok(attachment) => attachments.push(attachment),
            Err(reason) => notes.push(format!("[attachment skipped: {reason}]")),
        }
    }

    /// Decodes one base64 attachment and writes it beside the node's data.
    ///
    /// A named attachment gets a directory of its own so the file keeps exactly that name: the
    /// file name is what a recipient sees. Returns a human-readable reason on failure; the caller
    /// turns it into a note in the tool result so a dropped file is never mistaken for a
    /// successful one.
    fn store_attachment(
        &self,
        mime: &str,
        data: &str,
        name: Option<&str>,
    ) -> Result<ToolAttachment, String> {
        use base64::Engine;

        // Some servers inline a full data URL instead of raw base64.
        let payload = data
            .split_once(";base64,")
            .map(|(_, encoded)| encoded)
            .unwrap_or(data)
            .trim();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|err| format!("{mime} is not valid base64: {err}"))?;

        if bytes.len() > MCP_MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "{mime} is {} bytes, above the {} byte limit",
                bytes.len(),
                MCP_MAX_ATTACHMENT_BYTES
            ));
        }

        let path = new_attachment_path(&self.attachment_dir, mime, name)?;
        std::fs::write(&path, &bytes)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;

        // The path crosses a process boundary (the adapter plugin opens it), so it is handed over
        // absolute: a relative path would resolve against whatever directory that host runs in.
        let absolute = std::fs::canonicalize(&path).unwrap_or(path);
        Ok(ToolAttachment {
            mime_type: mime.to_string(),
            file_path: Some(absolute.to_string_lossy().to_string()),
            url: None,
        })
    }

    /// Sends one JSON-RPC request and waits for its response.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let mut guard = self.connection.lock().await;
        self.ensure_available().await?;
        let connection = guard.as_mut().ok_or_else(|| {
            McpError::Transport(format!("MCP server '{}' is not connected", self.config.id))
        })?;
        self.request_on(connection, method, params).await
    }

    /// Uses a transport already exclusively owned by an initialization or live request.
    async fn request_on(
        &self,
        connection: &mut Connection,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        // One deadline covers writes, every ignored stdio line, HTTP headers and the entire body.
        // Resetting it for each read lets a noisy server keep this exclusive connection forever.
        let response = tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdout, stdin, .. } => {
                    let mut line = serde_json::to_vec(&payload).map_err(|err| {
                        McpError::Transport(format!("failed to encode request: {err}"))
                    })?;
                    line.push(b'\n');
                    stdin.write_all(&line).await.map_err(|err| {
                        McpError::Transport(format!("failed to write request: {err}"))
                    })?;
                    stdin.flush().await.map_err(|err| {
                        McpError::Transport(format!("failed to flush request: {err}"))
                    })?;

                    // Read until the matching response id; notifications are ignored.
                    let mut buffer = String::new();
                    loop {
                        buffer.clear();
                        let read = stdout.read_line(&mut buffer).await.map_err(|err| {
                            McpError::Transport(format!("failed to read response: {err}"))
                        })?;

                        if read == 0 {
                            return Err(McpError::Transport(format!(
                                "MCP server '{}' closed its output while answering '{method}'",
                                self.config.id
                            )));
                        }

                        let trimmed = buffer.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let Ok(message) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                            // Servers may log to stdout; ignore anything that is not JSON-RPC.
                            continue;
                        };
                        if message.get("id").and_then(|value| value.as_u64()) != Some(id) {
                            continue;
                        }
                        break Ok(message);
                    }
                }
                Connection::Http {
                    client,
                    url,
                    headers,
                    session_id,
                } => {
                    let mut request = client
                        .post(url.as_str())
                        .json(&payload)
                        .header("accept", "application/json, text/event-stream");
                    for (key, value) in headers {
                        request = request.header(key.as_str(), value.as_str());
                    }
                    if let Some(session_id) = session_id.as_deref() {
                        request = request.header("mcp-session-id", session_id);
                    }
                    let response = request.send().await.map_err(|err| {
                        McpError::Transport(format!("HTTP request failed: {err}"))
                    })?;

                    let status = response.status();
                    if method == "initialize" && status.is_success() {
                        *session_id = response
                            .headers()
                            .get("mcp-session-id")
                            .map(|value| value.to_str().map(str::to_string))
                            .transpose()
                            .map_err(|err| {
                                McpError::Transport(format!("invalid MCP session header: {err}"))
                            })?;
                    }
                    let body = response.text().await.map_err(|err| {
                        McpError::Transport(format!("failed to read HTTP response: {err}"))
                    })?;

                    if !status.is_success() {
                        return Err(McpError::Transport(format!(
                            "MCP server '{}' answered HTTP {status}",
                            self.config.id
                        )));
                    }

                    // Streamable HTTP servers may answer with an SSE stream; take the last data line.
                    let json = body
                        .lines()
                        .filter_map(|line| line.strip_prefix("data:"))
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .next_back()
                        .unwrap_or(body.trim());
                    serde_json::from_str::<serde_json::Value>(json).map_err(|err| {
                        McpError::Transport(format!("failed to decode HTTP response: {err}"))
                    })
                }
            }
        })
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP server '{}' did not answer '{method}' in time",
                self.config.id
            ))
        })??;

        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error");
            return Err(McpError::Rpc {
                server: self.config.id.clone(),
                method: method.to_string(),
                message: message.to_string(),
            });
        }

        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Sends one JSON-RPC notification (no response expected).
    async fn notify(
        &self,
        connection: &mut Connection,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), McpError> {
        let payload = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdin, .. } => {
                    let mut line = serde_json::to_vec(&payload).map_err(|err| {
                        McpError::Transport(format!("failed to encode notice: {err}"))
                    })?;
                    line.push(b'\n');
                    stdin.write_all(&line).await.map_err(|err| {
                        McpError::Transport(format!("failed to write notice: {err}"))
                    })?;
                    stdin.flush().await.map_err(|err| {
                        McpError::Transport(format!("failed to flush notice: {err}"))
                    })?;
                }
                Connection::Http {
                    client,
                    url,
                    headers,
                    session_id,
                } => {
                    let mut request = client
                        .post(url.as_str())
                        .json(&payload)
                        .header("accept", "application/json, text/event-stream");
                    for (key, value) in headers {
                        request = request.header(key.as_str(), value.as_str());
                    }
                    if let Some(session_id) = session_id.as_deref() {
                        request = request.header("mcp-session-id", session_id);
                    }
                    let response = request.send().await.map_err(|err| {
                        McpError::Transport(format!("HTTP notification failed: {err}"))
                    })?;
                    if !response.status().is_success() {
                        return Err(McpError::Transport(format!(
                            "MCP server '{}' rejected notification '{method}' with HTTP {}",
                            self.config.id,
                            response.status()
                        )));
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP server '{}' did not accept notification '{method}' in time",
                self.config.id
            ))
        })?
    }
}

/// Host identifier used for MCP servers in tool metadata.
pub fn host_id(server_id: &str) -> String {
    format!("mcp_{server_id}")
}

/// Qualified tool name exposed to the model.
pub fn qualified_tool_name(server_id: &str, tool: &str) -> String {
    format!("mcp__{server_id}__{tool}")
}

/// Recovers the MCP tool name from its qualified form.
pub fn unqualified_tool_name(server_id: &str, qualified: &str) -> Option<String> {
    qualified
        .strip_prefix(&format!("mcp__{server_id}__"))
        .map(str::to_string)
}

#[async_trait]
impl ToolHost for McpServer {
    fn host_id(&self) -> &str {
        &self.host_id
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        self.meta
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    async fn call_tool(&self, req: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        let Some(tool) = unqualified_tool_name(&self.config.id, &req.tool_name) else {
            return Ok(ToolCallResponse {
                call_id: req.call_id,
                success: false,
                error_message: format!(
                    "Tool '{}' does not belong to MCP server '{}'",
                    req.tool_name, self.config.id
                ),
                payload: None,
                attachments: Vec::new(),
            });
        };

        let arguments = match req.payload {
            Some(kanon_proto::v1::tool_call_request::Payload::StructuredArgs(args)) => {
                kanon_llm::tool_router::prost_struct_to_json(args)
                    .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?
            }
            _ => serde_json::json!({}),
        };

        match self.call(&tool, arguments).await {
            Ok(outcome) => {
                let result = json_to_prost_struct(&serde_json::json!({ "content": outcome.text }))
                    .unwrap_or_default();
                // Attachments ride beside the text: the router forwards them to the pipeline, which
                // turns them into message segments the platform can deliver.
                let attachments = outcome
                    .attachments
                    .iter()
                    .map(ToolAttachment::to_proto)
                    .collect();
                Ok(ToolCallResponse {
                    call_id: req.call_id,
                    success: true,
                    error_message: String::new(),
                    payload: Some(
                        kanon_proto::v1::tool_call_response::Payload::StructuredResult(result),
                    ),
                    attachments,
                })
            }
            Err(err) => {
                tracing::warn!(server = %self.config.id, tool = %tool, error = %err, "MCP tool call failed");
                Ok(ToolCallResponse {
                    call_id: req.call_id,
                    success: false,
                    error_message: err.to_string(),
                    payload: None,
                    attachments: Vec::new(),
                })
            }
        }
    }
}

/// Every configured MCP server, connected on demand.
#[derive(Debug)]
pub struct McpPool {
    /// Servers by identifier.
    servers: RwLock<HashMap<String, Arc<McpServer>>>,
    /// The same persistent enablement store used by the node's control plane.
    toggles: Arc<ToggleStore>,
    /// Directory receiving attachments materialized from tool results.
    attachment_dir: PathBuf,
}

impl McpPool {
    /// Creates an empty pool writing attachments under the node's data directory.
    pub fn new(toggles: Arc<ToggleStore>) -> Self {
        Self {
            servers: RwLock::new(HashMap::new()),
            toggles,
            attachment_dir: PathBuf::from(DEFAULT_ATTACHMENT_DIR),
        }
    }

    /// Shared global switches; the API must use this same store when changing MCP enablement.
    pub fn toggle_store(&self) -> &Arc<ToggleStore> {
        &self.toggles
    }

    /// Overrides where tool attachments are materialized.
    pub fn with_attachment_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.attachment_dir = dir.into();
        self
    }

    /// Rebuilds the pool from the configuration document.
    pub async fn sync_from_config(&self, config: &McpConfigStore) {
        let mut servers = self.servers.write().await;
        // Read after taking the publication lock: concurrent edits must not apply an older
        // snapshot after a newer sync has already installed its server handles.
        let configured = config.list().await;
        let mut retired = Vec::new();

        // A turn can retain an Arc after it leaves the pool. Retire that identity before making
        // the new map visible so old handles cannot recreate a deleted server's child process.
        servers.retain(|id, server| {
            if configured.iter().any(|config| &config.id == id) {
                true
            } else {
                server
                    .retired
                    .store(true, std::sync::atomic::Ordering::Release);
                retired.push(server.clone());
                false
            }
        });
        for server in configured {
            match servers.get(&server.id) {
                Some(existing) if existing.config() == &server => {}
                _ => {
                    let handle = McpServer::new(server, self.toggles.clone())
                        .with_attachment_dir(self.attachment_dir.clone());
                    if let Some(previous) =
                        servers.insert(handle.config().id.clone(), Arc::new(handle))
                    {
                        previous
                            .retired
                            .store(true, std::sync::atomic::Ordering::Release);
                        retired.push(previous);
                    }
                }
            }
        }
        drop(servers);

        if !retired.is_empty() {
            // The new map is committed. Cleanup owns the retired handles even if the API caller
            // disconnects while an old RPC is finishing, and never blocks unrelated pool reads.
            let cleanup = tokio::spawn(async move {
                for server in retired {
                    server.disconnect().await;
                }
            });
            if let Err(error) = cleanup.await {
                tracing::error!(%error, "Retired MCP connection cleanup failed");
            }
        }
    }

    /// Returns one server.
    pub async fn get(&self, id: &str) -> Option<Arc<McpServer>> {
        self.servers.read().await.get(id).cloned()
    }

    /// Lists servers with their health, ordered by identifier.
    pub async fn describe(&self) -> Vec<(McpServerConfig, McpHealth)> {
        let servers: Vec<Arc<McpServer>> = self.servers.read().await.values().cloned().collect();
        let mut described = Vec::with_capacity(servers.len());
        for server in servers {
            described.push((server.config().clone(), server.health().await));
        }
        described.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        described
    }

    /// Tool hosts this instance may use.
    ///
    /// Both switches are honoured: the node-wide toggle and the instance's own override. A server
    /// is only connected when an instance actually reaches it, so an unused server costs nothing.
    pub async fn hosts_for_instance(
        &self,
        instance: Option<&BotInstance>,
    ) -> Vec<Arc<dyn ToolHost>> {
        let servers: Vec<Arc<McpServer>> = self.servers.read().await.values().cloned().collect();

        let mut hosts: Vec<Arc<dyn ToolHost>> = Vec::new();
        for server in servers {
            let id = server.config().id.clone();
            let globally_enabled = self.toggles.is_enabled(MCP_SECTION, &id).await;
            if !globally_enabled {
                continue;
            }
            if let Some(instance) = instance
                && !instance.allows_mcp(&id, globally_enabled)
            {
                continue;
            }
            // Connecting here keeps the first token of latency on the pipeline worker instead of
            // making the model wait for a handshake mid-conversation.
            if let Err(err) = server.connect().await {
                tracing::warn!(server = %id, error = %err, "MCP server unavailable; its tools are skipped");
                continue;
            }
            hosts.push(server as Arc<dyn ToolHost>);
        }
        hosts
    }

    /// Starts the MCP watchdog: probes every enabled server and reconnects when it stops answering.
    pub fn spawn_watchdog(
        self: &Arc<Self>,
        config: Arc<McpConfigStore>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                pool.sync_from_config(&config).await;

                let servers: Vec<Arc<McpServer>> =
                    pool.servers.read().await.values().cloned().collect();
                for server in servers {
                    // A server switched off in the console is not probed: it holds no connection
                    // worth keeping, and the toggle store is the single source of that decision.
                    if !pool
                        .toggles
                        .is_enabled(MCP_SECTION, &server.config().id)
                        .await
                    {
                        continue;
                    }

                    // `tools/list` (or the handshake, for a server not yet connected) doubles as the
                    // liveness probe: it proves the transport, the handshake and the server's own
                    // dispatch loop are all working.
                    if let Err(err) = server.probe().await {
                        let failures = server.health.lock().await.failures;
                        tracing::warn!(
                            server = %server.config().id,
                            failures,
                            error = %err,
                            "MCP server liveness probe failed; it will be reconnected on the next attempt"
                        );
                    }
                }
            }
        })
    }
}

/// Tool metadata listing is asynchronous; this helper exposes it to the pipeline.
impl McpServer {
    /// Synthetic plugin metadata describing this server's tools.
    pub fn plugin_meta(&self) -> Vec<PluginMeta> {
        self.meta
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Tool definitions currently advertised by this server.
    pub fn tool_count(&self) -> usize {
        self.plugin_meta()
            .first()
            .map(|meta| meta.tools.len())
            .unwrap_or(0)
    }
}

/// The file name an embedded resource is delivered under, taken from its URI.
///
/// Only the last path component is used, and anything that could leave the attachment directory
/// or is not a portable file name is rejected (the attachment then gets a generated name). A name
/// without an extension gets the MIME type's, so the recipient can open the file.
fn resource_file_name(uri: &str, mime: &str) -> Option<String> {
    let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let name = path.rsplit('/').next().unwrap_or_default().trim();
    let portable = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 200
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    if !portable {
        return None;
    }
    let extension = extension_for_mime(mime);
    if name.contains('.') || extension == "bin" {
        Some(name.to_string())
    } else {
        Some(format!("{name}.{extension}"))
    }
}

/// File extension used for one MIME type.
pub(crate) fn extension_for_mime(mime: &str) -> &'static str {
    let mime = mime.trim().to_ascii_lowercase();
    MEDIA_TYPES
        .iter()
        .find(|(known, _)| *known == mime)
        .map_or("bin", |(_, extension)| extension)
}

/// MIME type for a file extension; `application/octet-stream` when it is not a known one.
///
/// An unknown type is still sent, as a named file: the extension it keeps tells the recipient's
/// client what opens it.
pub(crate) fn mime_for_extension(extension: &str) -> &'static str {
    let extension = extension.trim().to_ascii_lowercase();
    MEDIA_TYPES
        .iter()
        .find(|(_, known)| *known == extension)
        .map_or("application/octet-stream", |(mime, _)| mime)
}

/// MIME types and the file extensions they are stored under, for lookups in both directions.
///
/// The first row naming a MIME type gives its extension and the first row naming an extension
/// gives its MIME type, so canonical rows come before their aliases.
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/jpg", "jpg"),
    ("image/jpeg", "jpeg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("audio/mpeg", "mp3"),
    ("audio/mp3", "mp3"),
    ("audio/wav", "wav"),
    ("audio/wave", "wav"),
    ("audio/x-wav", "wav"),
    ("audio/ogg", "ogg"),
    ("audio/ogg", "oga"),
    ("audio/opus", "opus"),
    ("audio/aac", "aac"),
    ("audio/mp4", "m4a"),
    ("audio/m4a", "m4a"),
    ("audio/x-m4a", "m4a"),
    ("audio/flac", "flac"),
    ("audio/aiff", "aiff"),
    ("audio/amr", "amr"),
    ("audio/silk", "silk"),
    ("video/mp4", "mp4"),
    ("video/webm", "webm"),
    ("video/quicktime", "mov"),
    ("video/x-matroska", "mkv"),
    ("application/pdf", "pdf"),
    ("application/zip", "zip"),
    ("application/json", "json"),
    ("text/plain", "txt"),
    ("text/csv", "csv"),
    ("text/markdown", "md"),
];

/// Process-wide sequence that keeps attachment names unique when several tools store attachments
/// within the same millisecond.
static ATTACHMENT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Reserves a fresh path in `dir` for one attachment; the caller writes the file.
///
/// A named attachment gets a directory of its own so the file keeps exactly that name: the file
/// name is what a recipient sees. An unnamed one is stored flat under its MIME type's extension.
pub(crate) fn new_attachment_path(
    dir: &Path,
    mime: &str,
    name: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let seq = ATTACHMENT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    match name {
        Some(name) => {
            let slot = dir.join(format!("{stamp}-{seq}"));
            std::fs::create_dir(&slot)
                .map_err(|err| format!("failed to create {}: {err}", slot.display()))?;
            Ok(slot.join(name))
        }
        None => Ok(dir.join(format!("{stamp}-{seq}.{}", extension_for_mime(mime)))),
    }
}

/// Truncates a tool result so it can never flood the model context window.
///
/// Truncation happens on a character boundary and marks itself, so the model can tell a clipped
/// result from a complete one.
fn truncate_for_model(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let head: String = value.chars().take(limit).collect();
    format!("{head}… [truncated at {limit} characters]")
}

/// Deletes attachment files older than `max_age` and reports how many were swept.
///
/// Called at node start: a file that has survived its retention window was either never delivered
/// (and is already recorded in the dead-letter log) or has long since been sent.
pub fn prune_attachments(dir: &Path, max_age: Duration) -> std::io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Named attachments live in a directory of their own; either entry is one attachment.
        let is_dir = path.is_dir();
        if !is_dir && !path.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        let swept = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if swept.is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}
