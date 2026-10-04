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

mod attachments;
mod config;
pub use attachments::prune_attachments;
pub(crate) use attachments::{extension_for_mime, mime_for_extension, new_attachment_path};
mod wire;
pub use wire::{host_id, qualified_tool_name, unqualified_tool_name};
mod pool;
pub use pool::McpPool;
mod transport;
use attachments::resource_file_name;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use transport::*;

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
