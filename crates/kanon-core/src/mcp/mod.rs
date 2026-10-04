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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};

use crate::instance::BotInstance;
use crate::toggle::{MCP_SECTION, ToggleStore};

/// Default MCP configuration path, relative to the node working directory.
pub const DEFAULT_MCP_CONFIG: &str = "./data/mcp.json";

/// Per-request deadline for an MCP call.
pub const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum wire size of one response, enough for four maximum-sized base64 attachments.
pub const MCP_MAX_MESSAGE_BYTES: usize = 48 * 1024 * 1024;

/// Protocol revisions implemented by this client, newest first.
const MCP_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

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
    /// The RPC succeeded, but the tool reported an execution failure in its result.
    #[error("MCP tool '{tool}' on server '{server}' failed: {message}")]
    Tool {
        /// Server identifier.
        server: String,
        /// Unqualified tool name.
        tool: String,
        /// Model-facing diagnostic supplied by the tool.
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
    let id = config.id.as_str();
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
        McpTransport::Http { url, headers } => {
            let endpoint = reqwest::Url::parse(url)
                .map_err(|err| McpError::InvalidConfig(format!("invalid HTTP MCP url: {err}")))?;
            if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
                return Err(McpError::InvalidConfig(
                    "HTTP MCP url must use http:// or https:// and include a host".into(),
                ));
            }
            for (name, value) in headers {
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
                    McpError::InvalidConfig(format!("invalid HTTP MCP header name: {err}"))
                })?;
                reqwest::header::HeaderValue::from_str(value).map_err(|err| {
                    // Header values can contain credentials; never include them in diagnostics.
                    McpError::InvalidConfig(format!("invalid HTTP MCP header '{name}': {err}"))
                })?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Reads the configuration document, returning `None` when it does not exist.
fn read_document(path: &Path) -> Result<Option<McpDocument>, McpError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(McpError::Config(format!(
                "failed to read {}: {err}",
                path.display()
            )));
        }
    };
    let document: McpDocument = serde_json::from_str(&raw)
        .map_err(|err| McpError::Config(format!("failed to parse {}: {err}", path.display())))?;
    if document.version != default_version() {
        return Err(McpError::Config(format!(
            "unsupported MCP configuration version {} in {}",
            document.version,
            path.display()
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for server in &document.servers {
        validate_config(server).map_err(|err| {
            McpError::Config(format!("invalid server in {}: {err}", path.display()))
        })?;
        if !ids.insert(&server.id) {
            return Err(McpError::Config(format!(
                "duplicate MCP server id '{}' in {}",
                server.id,
                path.display()
            )));
        }
    }
    Ok(Some(document))
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
    /// HTTP endpoint and its negotiated session.
    Http(HttpConnection),
}

/// Protocol-owned headers are assembled once for requests, notifications and server replies.
struct HttpConnection {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    session_id: Option<String>,
    protocol_version: Option<String>,
}

impl HttpConnection {
    /// Sends one message without consuming its body. The caller owns the overall deadline.
    async fn send(&mut self, payload: &serde_json::Value) -> Result<reqwest::Response, McpError> {
        let mut request = self.client.post(&self.url);
        for (key, value) in &self.headers {
            if ![
                "accept",
                "content-type",
                "mcp-session-id",
                "mcp-protocol-version",
            ]
            .iter()
            .any(|reserved| key.eq_ignore_ascii_case(reserved))
            {
                request = request.header(key, value);
            }
        }
        request = request
            .json(payload)
            .header("accept", "application/json, text/event-stream");
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(version) = &self.protocol_version {
            request = request.header("mcp-protocol-version", version);
        }
        let response = request
            .send()
            .await
            .map_err(|error| McpError::Transport(format!("HTTP request failed: {error}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && self.session_id.is_some() {
            self.session_id = None;
            self.protocol_version = None;
            // The caller drops this connection. A subsequent operation initializes afresh;
            // never replay a possibly side-effecting tools/call behind the model's back.
            return Err(McpError::Transport(
                "MCP HTTP session expired; reconnect before retrying".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "MCP HTTP request rejected with {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MCP_MAX_MESSAGE_BYTES as u64)
        {
            return Err(McpError::Transport(
                "MCP response exceeds the message size limit".into(),
            ));
        }
        if payload.get("method").and_then(|value| value.as_str()) == Some("initialize") {
            self.session_id = response
                .headers()
                .get("mcp-session-id")
                .map(|value| value.to_str().map(str::to_string))
                .transpose()
                .map_err(|error| {
                    McpError::Transport(format!("invalid MCP session header: {error}"))
                })?;
            if self.session_id.as_ref().is_some_and(|id| {
                id.is_empty() || !id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            }) {
                return Err(McpError::Transport("invalid MCP session identifier".into()));
            }
        }
        Ok(response)
    }

    /// Notifications and responses to server requests are acknowledged without an RPC body.
    async fn send_one_way(&mut self, payload: &serde_json::Value) -> Result<(), McpError> {
        let response = self.send(payload).await.map_err(|error| {
            McpError::Transport(format!("MCP notification or reply failed: {error}"))
        })?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err(McpError::Transport(format!(
                "MCP notification or reply expected HTTP 202, received {}",
                response.status()
            )));
        }
        Ok(())
    }
}

/// A validated inbound message either completes our request or needs a separate server reply.
enum IncomingMessage {
    Result(serde_json::Value),
    Reply(serde_json::Value),
    Ignore,
}

/// Writes complete JSON-RPC lines through the same path for all stdio message kinds.
async fn write_stdio(
    stdin: &mut tokio::process::ChildStdin,
    payload: &serde_json::Value,
) -> Result<(), McpError> {
    let mut line = serde_json::to_vec(payload)
        .map_err(|error| McpError::Transport(format!("failed to encode message: {error}")))?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| McpError::Transport(format!("failed to write message: {error}")))?;
    stdin
        .flush()
        .await
        .map_err(|error| McpError::Transport(format!("failed to flush message: {error}")))
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
        let mut health = self.health.lock().await.clone();
        // A cancelled stdio operation drops its locally owned connection before releasing the
        // mutex. Release the health guard before inspecting the connection to preserve the
        // writer lock order. try_lock keeps health reporting responsive during a slow tool.
        if health.state == "connected"
            && self
                .connection
                .try_lock()
                .is_ok_and(|connection| connection.is_none())
        {
            health.state = "disconnected".to_string();
            health.tools = 0;
        }
        health
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

        let attempt = if matches!(current.as_ref(), Some(Connection::Stdio { .. })) {
            // A cancelled read may already have consumed a JSON prefix, and a cancelled write
            // may have sent only part of a request. Own stdio until this probe commits so either
            // cancellation drops the child or the next operation receives complete framing.
            // Keep the committed catalog visible while discovery is in flight.
            let mut connection = current.take().expect("checked stdio connection");
            self.read_tools(&mut connection)
                .await
                .map(|metadata| (Some(connection), metadata))
        } else if let Some(connection) = current.as_mut() {
            // A probe refreshes an already initialized transport. Keep its committed metadata
            // while the RPC runs so concurrent tool discovery never sees an empty catalog.
            // HTTP responses have independent framing, so cancellation can retain its session.
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
            McpTransport::Http { url, headers } => Ok(Connection::Http(HttpConnection {
                client: reqwest::Client::builder().build().map_err(|error| {
                    McpError::Transport(format!("failed to create HTTP client: {error}"))
                })?,
                url: url.clone(),
                headers: headers.clone(),
                session_id: None,
                protocol_version: None,
            })),
        }
    }

    /// Performs the MCP `initialize` handshake.
    async fn handshake(&self, connection: &mut Connection) -> Result<(), McpError> {
        let result = self
            .request_on(
                connection,
                "initialize",
                serde_json::json!({
                    "protocolVersion": MCP_PROTOCOL_VERSIONS[0],
                    "capabilities": {},
                    "clientInfo": { "name": "kanon", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;

        let version = result
            .get("protocolVersion")
            .and_then(|value| value.as_str())
            .filter(|version| MCP_PROTOCOL_VERSIONS.contains(version))
            .ok_or_else(|| {
                McpError::Transport(
                    "MCP server selected an unsupported or missing protocol version".into(),
                )
            })?;
        if let Connection::Http(http) = connection {
            http.protocol_version = Some(version.to_string());
        }

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
        let malformed = |message: &str| {
            McpError::Transport(format!(
                "MCP server '{}' returned invalid tools/list data: {message}",
                self.config.id
            ))
        };
        let mut metas = Vec::new();
        let mut names = std::collections::HashSet::new();
        let mut cursors = std::collections::HashSet::new();
        let mut params = serde_json::json!({});
        let deadline = tokio::time::Instant::now() + MCP_REQUEST_TIMEOUT;
        let mut remaining_bytes = MCP_MAX_MESSAGE_BYTES;
        loop {
            let response = tokio::time::timeout_at(
                deadline,
                self.request_on(connection, "tools/list", params),
            )
            .await
            .map_err(|_| malformed("tool discovery deadline exceeded"))??;
            // A valid next cursor must not reset either budget: discovery is one bounded operation.
            remaining_bytes = remaining_bytes
                .checked_sub(response.to_string().len())
                .ok_or_else(|| {
                    malformed("tool discovery exceeds the aggregate message size limit")
                })?;
            let tools = response
                .get("tools")
                .and_then(|tools| tools.as_array())
                .ok_or_else(|| malformed("tools must be an array"))?;

            for tool in tools {
                let name = tool
                    .get("name")
                    .and_then(|name| name.as_str())
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| malformed("tool name must be a nonempty string"))?;
                if !names.insert(name.to_string()) {
                    return Err(malformed(&format!("duplicate tool name '{name}'")));
                }
                let description = match tool.get("description") {
                    None => String::new(),
                    Some(value) => value
                        .as_str()
                        .ok_or_else(|| malformed("tool description must be a string"))?
                        .to_string(),
                };
                let schema = tool
                    .get("inputSchema")
                    .filter(|schema| {
                        schema.get("type").and_then(|value| value.as_str()) == Some("object")
                    })
                    .ok_or_else(|| malformed("tool inputSchema must declare type 'object'"))?;
                if schema
                    .get("properties")
                    .is_some_and(|properties| !properties.is_object())
                {
                    return Err(malformed("inputSchema properties must be an object"));
                }
                if schema.get("required").is_some_and(|required| {
                    required
                        .as_array()
                        .is_none_or(|fields| fields.iter().any(|field| !field.is_string()))
                }) {
                    return Err(malformed(
                        "inputSchema required must be an array of strings",
                    ));
                }
                let parameters = json_to_prost_struct(schema)
                    .ok_or_else(|| malformed("tool inputSchema must be an object"))?;

                metas.push(ToolMeta {
                    name: qualified_tool_name(&self.config.id, name),
                    description,
                    parameters: Some(parameters),
                });
            }

            let Some(cursor) = response.get("nextCursor") else {
                break;
            };
            let cursor = cursor
                .as_str()
                .ok_or_else(|| malformed("nextCursor must be a string"))?;
            if !cursors.insert(cursor.to_string()) {
                return Err(malformed("pagination cursor repeated"));
            }
            // Cursors are opaque, including whitespace. Publish nothing until every page validates.
            params = serde_json::json!({"cursor": cursor});
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(malformed("tool discovery deadline exceeded"));
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

        let malformed = |message: &str| McpError::Tool {
            server: self.config.id.clone(),
            tool: tool.to_string(),
            message: format!("invalid tool result: {message}"),
        };
        let is_error = match response.get("isError") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| malformed("isError must be a boolean"))?,
        };
        let structured = response.get("structuredContent");
        if structured.is_some_and(|value| !value.is_object()) {
            return Err(malformed("structuredContent must be an object"));
        }
        let items = response
            .get("content")
            .and_then(|value| value.as_array())
            .ok_or_else(|| malformed("content must be an array"))?;
        let mut texts: Vec<String> = Vec::new();
        // Reasons an attachment was dropped travel with the text: a missing picture must be
        // visible to the operator instead of silently vanishing from the conversation.
        let mut notes: Vec<String> = Vec::new();
        let mut attachments: Vec<ToolAttachment> = Vec::new();

        {
            for item in items {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                            // MCP servers commonly include the same JSON in a text block for old
                            // clients. Keep its structured form once, without losing other prose.
                            let duplicates_structured = structured.is_some_and(|structured| {
                                serde_json::from_str::<serde_json::Value>(text)
                                    .is_ok_and(|value| &value == structured)
                            });
                            if !text.is_empty() && !duplicates_structured {
                                texts.push(text.to_string());
                            }
                        }
                    }
                    Some("image") | Some("audio") if !is_error => {
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
                            if !text.is_empty() {
                                texts.push(text.to_string());
                            }
                        } else if !is_error
                            && let Some(blob) =
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
                    Some("resource_link") => {
                        // The URI, name and description are model-visible output even when a
                        // server also supplied a textual summary. Never fetch the URI implicitly.
                        texts.push(item.to_string());
                    }
                    // Error attachments intentionally remain undelivered; unsupported content
                    // is made explicit instead of silently claiming that the result was empty.
                    Some("image" | "audio") if is_error => {}
                    kind => notes.push(format!(
                        "[unsupported MCP content type: {}]",
                        kind.unwrap_or("missing")
                    )),
                }
            }
        }

        if let Some(structured) = structured {
            texts.push(structured.to_string());
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
        } else if is_error {
            "the tool reported failure without diagnostic content".to_string()
        } else {
            "[tool completed without content]".to_string()
        };

        if is_error {
            // An execution failure is a paired tool result, not a broken MCP connection. No
            // attachment was materialized, and ToolHost reports the diagnostic as a failure.
            return Err(McpError::Tool {
                server: self.config.id.clone(),
                tool: tool.to_string(),
                message: text,
            });
        }
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
        // Reject impossible sizes before the decoder reserves an output buffer. The decoded
        // check remains necessary because the final base64 quartet may contain padding.
        if payload.len() > MCP_MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 {
            return Err(format!(
                "{mime} encoded data exceeds the {} byte attachment limit",
                MCP_MAX_ATTACHMENT_BYTES
            ));
        }
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
        // Stdio has one byte stream for every request. On cancellation, dropping this local
        // owner also closes any half-written request or half-consumed response; never hand those
        // bytes to the next caller. HTTP request cancellation leaves other responses intact.
        let mut owned = if matches!(guard.as_ref(), Some(Connection::Stdio { .. })) {
            guard.take()
        } else {
            None
        };
        let connection = owned.as_mut().or(guard.as_mut()).ok_or_else(|| {
            McpError::Transport(format!("MCP server '{}' is not connected", self.config.id))
        })?;
        let result = self.request_on(connection, method, params).await;
        if let Err(McpError::Transport(error)) = &result {
            // Framing, I/O and expired-session failures make the connection unusable. RPC/tool
            // errors are different: the server answered correctly and may handle the next call.
            *guard = None;
            self.clear_metadata();
            let mut health = self.health.lock().await;
            health.failures += 1;
            health.state = "reconnecting".to_string();
            health.tools = 0;
            health.last_error = Some(error.clone());
        } else if let Some(connection) = owned {
            // A complete result or RPC error consumed its entire frame. Restoring ownership has
            // no await, so cancellation cannot interrupt the commit after transport validation.
            *guard = Some(connection);
        }
        result
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
        let payload =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        // One deadline includes writes, server-initiated pings, and every streamed event.
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdout, stdin, .. } => {
                    write_stdio(stdin, &payload).await?;
                    let mut buffer = Vec::new();
                    loop {
                        buffer.clear();
                        // take() bounds allocation even if a broken server never writes a newline.
                        let read = (&mut *stdout)
                            .take((MCP_MAX_MESSAGE_BYTES + 1) as u64)
                            .read_until(b'\n', &mut buffer)
                            .await
                            .map_err(|error| {
                                McpError::Transport(format!("failed to read response: {error}"))
                            })?;
                        if read == 0 {
                            return Err(McpError::Transport(format!(
                                "MCP server '{}' closed its output while answering '{method}'",
                                self.config.id
                            )));
                        }
                        if read > MCP_MAX_MESSAGE_BYTES {
                            return Err(McpError::Transport(
                                "MCP response exceeds the message size limit".into(),
                            ));
                        }
                        let message = serde_json::from_slice(&buffer).map_err(|error| {
                            McpError::Transport(format!("invalid MCP JSON: {error}"))
                        })?;
                        match self.incoming(message, id, method)? {
                            IncomingMessage::Result(result) => return Ok(result),
                            IncomingMessage::Reply(reply) => write_stdio(stdin, &reply).await?,
                            IncomingMessage::Ignore => {}
                        }
                    }
                }
                Connection::Http(http) => {
                    let mut response = http.send(&payload).await?;
                    let content_type = response
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.split(';').next())
                        .unwrap_or("")
                        .trim();
                    let is_sse = content_type.eq_ignore_ascii_case("text/event-stream");
                    if !is_sse && !content_type.eq_ignore_ascii_case("application/json") {
                        return Err(McpError::Transport(format!(
                            "unsupported MCP response Content-Type '{content_type}'"
                        )));
                    }
                    let mut decoder = kanon_llm::SseDecoder::new();
                    let mut body = Vec::new();
                    let mut received = 0;
                    while let Some(chunk) = response.chunk().await.map_err(|error| {
                        McpError::Transport(format!("failed to read HTTP response: {error}"))
                    })? {
                        received += chunk.len();
                        if received > MCP_MAX_MESSAGE_BYTES {
                            return Err(McpError::Transport(
                                "MCP response exceeds the message size limit".into(),
                            ));
                        }
                        if !is_sse {
                            body.extend_from_slice(&chunk);
                            continue;
                        }
                        for event in decoder.decode(&chunk) {
                            let message = serde_json::from_str(&event.data).map_err(|error| {
                                McpError::Transport(format!("invalid MCP SSE JSON: {error}"))
                            })?;
                            match self.incoming(message, id, method)? {
                                IncomingMessage::Result(result) => return Ok(result),
                                IncomingMessage::Reply(reply) => http.send_one_way(&reply).await?,
                                IncomingMessage::Ignore => {}
                            }
                        }
                    }
                    if !is_sse {
                        let message = serde_json::from_slice(&body).map_err(|error| {
                            McpError::Transport(format!("invalid MCP JSON: {error}"))
                        })?;
                        if let IncomingMessage::Result(result) =
                            self.incoming(message, id, method)?
                        {
                            return Ok(result);
                        }
                    }
                    Err(McpError::Transport(
                        "MCP HTTP response ended without the matching JSON-RPC result".into(),
                    ))
                }
            }
        })
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP server '{}' did not answer '{method}' in time",
                self.config.id
            ))
        })?
    }

    /// Validates envelopes before interpreting payloads; server and client request IDs are separate.
    fn incoming(
        &self,
        mut message: serde_json::Value,
        id: u64,
        method: &str,
    ) -> Result<IncomingMessage, McpError> {
        let invalid = || McpError::Transport("invalid MCP JSON-RPC envelope".into());
        if message.get("jsonrpc").and_then(|value| value.as_str()) != Some("2.0") {
            return Err(invalid());
        }
        if let Some(server_method) = message.get("method") {
            let server_method = server_method.as_str().ok_or_else(invalid)?;
            if message.get("result").is_some() || message.get("error").is_some() {
                return Err(invalid());
            }
            let Some(server_id) = message.get("id") else {
                return Ok(IncomingMessage::Ignore);
            };
            if !server_id.is_string() && !server_id.is_i64() && !server_id.is_u64() {
                return Err(invalid());
            }
            // No sampling/elicitation capability is advertised. Respond explicitly instead of
            // leaving a server blocked waiting on a request this client cannot implement.
            let reply = if server_method == "ping" {
                serde_json::json!({"jsonrpc":"2.0", "id":server_id, "result":{}})
            } else {
                serde_json::json!({"jsonrpc":"2.0", "id":server_id, "error":{"code":-32601, "message":"Method not supported"}})
            };
            return Ok(IncomingMessage::Reply(reply));
        }
        let result = message.get("result");
        let error = message.get("error");
        if result.is_some() == error.is_some()
            || result.is_some_and(|value| !value.is_object())
            || error.is_some_and(|error| {
                error.get("code").and_then(|value| value.as_i64()).is_none()
                    || error
                        .get("message")
                        .and_then(|value| value.as_str())
                        .is_none()
            })
        {
            return Err(invalid());
        }
        let response_id = message.get("id").ok_or_else(invalid)?;
        if !response_id.is_string() && !response_id.is_i64() && !response_id.is_u64() {
            return Err(invalid());
        }
        if response_id.as_u64() != Some(id) {
            // A canceled stdio request may leave a late response. Never use it for another call.
            return Ok(IncomingMessage::Ignore);
        }
        if let Some(error) = error {
            return Err(McpError::Rpc {
                server: self.config.id.clone(),
                method: method.to_string(),
                message: error["message"]
                    .as_str()
                    .expect("validated error message")
                    .to_string(),
            });
        }
        // The envelope is owned here; move its potentially large result without a second copy.
        Ok(IncomingMessage::Result(message["result"].take()))
    }

    /// Sends one JSON-RPC notification (no response expected).
    async fn notify(
        &self,
        connection: &mut Connection,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), McpError> {
        let payload = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, async {
            match connection {
                Connection::Stdio { stdin, .. } => write_stdio(stdin, &payload).await,
                Connection::Http(http) => http.send_one_way(&payload).await,
            }
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
                    // This is already the model-facing text; a Struct wrapper would quote and
                    // escape any rendered JSON again in the conversation history.
                    payload: Some(kanon_proto::v1::tool_call_response::Payload::RawBytes(
                        outcome.text.into_bytes(),
                    )),
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
