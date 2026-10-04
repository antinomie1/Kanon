//! Shared plugin context and dynamic tool registry updates.

use super::*;

impl ContextSlot {
    /// The context, `None` before the host loaded the plugin.
    pub fn get(&self) -> Option<PluginContext> {
        self.0
            .context
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The Core handle, for handlers that have no event to take it from (HTTP routes, actions,
    /// background tasks). Fails with [`CoreError::Standalone`] before the host loaded the plugin
    /// and in standalone mode.
    pub fn core(&self) -> Result<CoreHandle, CoreError> {
        self.handle().ok_or(CoreError::Standalone)
    }

    /// The Core handle if there is one, for events built by the router.
    pub(super) fn handle(&self) -> Option<CoreHandle> {
        self.get().and_then(|ctx| ctx.core)
    }

    pub(super) fn update(&self, apply: impl FnOnce(&mut Option<PluginContext>)) {
        apply(
            &mut self
                .0
                .context
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }

    // The tool table is only ever touched under a short, synchronous lock: never across an
    // await, so a slow tool or refresh cannot block `GetPluginMeta`.
    pub(super) fn tools(&self) -> std::sync::RwLockReadGuard<'_, Vec<ToolEntry>> {
        self.0
            .tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn tools_mut(&self) -> std::sync::RwLockWriteGuard<'_, Vec<ToolEntry>> {
        self.0
            .tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn tool_metas(&self) -> Vec<ToolMeta> {
        self.tools()
            .iter()
            .map(|entry| entry.meta.clone())
            .collect()
    }

    pub(super) fn tool_handler(&self, name: &str) -> Option<ToolHandler> {
        self.tools()
            .iter()
            .find(|entry| entry.meta.name == name)
            .map(|entry| entry.handler.clone())
    }

    pub(super) fn insert_tool(&self, entry: ToolEntry) -> Result<(), String> {
        let mut tools = self.tools_mut();
        if entry.meta.name.is_empty() {
            return Err("a tool name must not be empty".to_string());
        }
        if tools.iter().any(|known| known.meta.name == entry.meta.name) {
            return Err(format!("tool '{}' is already declared", entry.meta.name));
        }
        tools.push(entry);
        Ok(())
    }

    /// Removes tool `name`, returning it and where it was.
    pub(super) fn take_tool(&self, name: &str) -> Option<(usize, ToolEntry)> {
        let mut tools = self.tools_mut();
        let index = tools.iter().position(|entry| entry.meta.name == name)?;
        Some((index, tools.remove(index)))
    }

    /// Adds a tool while the plugin runs and tells the core, which offers it from the next turn
    /// on. Takes the same [`ToolSpec`] and handler as [`Router::tool`].
    ///
    /// The tool list is part of the model's request prefix: every change invalidates the
    /// provider's prompt cache, so change tools rarely (on configuration, not per message).
    ///
    /// Fails with [`CoreError::InvalidArgument`] when the name is taken, and with the core's
    /// error when it cannot refresh the plugin's metadata — the tool is then removed again, so
    /// the plugin and the core never disagree. In standalone mode there is no core to tell, and
    /// the tool is only registered.
    pub async fn add_tool<A, F, Fut, R>(
        &self,
        spec: ToolSpec<A>,
        handler: F,
    ) -> Result<(), CoreError>
    where
        A: Send + 'static,
        F: Fn(A, Option<MessageEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoToolOutput + 'static,
    {
        let name = spec.meta.name.clone();
        self.insert_tool(tool_entry(spec, handler))
            .map_err(CoreError::InvalidArgument)?;
        if let Some(core) = self.handle() {
            if let Err(err) = core.refresh_plugin_meta().await {
                self.take_tool(&name);
                return Err(err);
            }
        }
        Ok(())
    }

    /// Removes tool `name` while the plugin runs and tells the core; returns whether it existed.
    ///
    /// When the core cannot refresh the plugin's metadata the tool is restored and the core's
    /// error returned.
    pub async fn remove_tool(&self, name: &str) -> Result<bool, CoreError> {
        let Some((index, entry)) = self.take_tool(name) else {
            return Ok(false);
        };
        if let Some(core) = self.handle() {
            if let Err(err) = core.refresh_plugin_meta().await {
                let mut tools = self.tools_mut();
                let index = index.min(tools.len());
                tools.insert(index, entry);
                return Err(err);
            }
        }
        Ok(true)
    }
}
