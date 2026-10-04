//! Versioned plugin configuration reloads under the per-plugin write guard.

use super::*;

impl PluginConfigGuard {
    /// Returns the version protected by this transaction.
    pub fn version(&self) -> u64 {
        *self.version
    }

    /// Rejects a stale editor before it changes either the file or the running host.
    pub fn check_version(&self, expected: Option<u64>) -> Result<(), SupervisorError> {
        if let Some(expected) = expected
            && expected != self.version()
        {
            return Err(SupervisorError::StaleConfigVersion {
                plugin_id: self.plugin_id.clone(),
                current_version: self.version(),
                requested_version: expected,
            });
        }
        Ok(())
    }

    /// Applies the candidate to its host and advances the version only after acceptance.
    pub async fn reload(
        &mut self,
        host: &ManagedHost,
        config: &serde_json::Value,
    ) -> Result<u64, SupervisorError> {
        if !host.declares_plugin(&self.plugin_id) {
            return Err(SupervisorError::PluginNotFound(self.plugin_id.clone()));
        }
        let plugin_id = self.plugin_id.as_str();
        let next_ver = self.version() + 1;

        let structured = kanon_llm::tool_router::json_to_prost_struct(config)
            .ok_or(SupervisorError::InvalidConfigPayload)?;

        // The transaction may outlive its HTTP caller. Bound every host await so a plugin that
        // never answers cannot retain its configuration lock and pending file indefinitely.
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            host.reload_config(plugin_id, structured, next_ver),
        )
        .await
        .map_err(|_| {
            tonic::Status::deadline_exceeded("Configuration reload timed out after 10s")
        })??;
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

        *self.version = applied;

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
        let metadata = host.get_plugin_meta().await;
        match metadata {
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
}
