//! Shared MCP connection pool and watchdog.

use super::*;

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
