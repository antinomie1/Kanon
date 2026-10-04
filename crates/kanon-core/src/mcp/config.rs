//! MCP configuration validation and durable catalog updates.

use super::*;

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
pub(super) fn validate_config(config: &McpServerConfig) -> Result<(), McpError> {
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
pub(super) fn read_document(path: &Path) -> Result<Option<McpDocument>, McpError> {
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
