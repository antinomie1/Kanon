//! Managed host RPCs, metadata snapshots and per-turn tool context.

use super::*;

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
            host_client: PluginHostServiceClient::with_interceptor(
                channel.clone(),
                kanon_transport::ClientAuthInterceptor::default(),
            ),
            pipeline_client: MessagePipelineServiceClient::with_interceptor(
                channel,
                kanon_transport::ClientAuthInterceptor::default(),
            ),
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

    /// Dispatches a command with a deadline so an unresponsive host cannot exhaust chat lanes.
    pub async fn execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let Some(permit) = self.circuit_breaker.try_acquire() else {
            return Err(tonic::Status::unavailable(format!(
                "Circuit breaker is OPEN for host '{}'",
                self.host_id
            )));
        };
        let start = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
        let mut client = self.pipeline_client.clone();
        let mut request = tonic::Request::new(req);
        request.set_timeout(COMMAND_TIMEOUT);
        // The local deadline also bounds channel readiness. The wire deadline lets a host
        // cancel its handler; neither side retries a command whose effects may have committed.
        let response = tokio::select! {
            // Tonic may report its wire timeout as Cancelled. Once our deadline is reached,
            // give the local timer priority so the public error remains DeadlineExceeded.
            biased;
            () = tokio::time::sleep_until(deadline) => Err(tonic::Status::deadline_exceeded(
                "Plugin command exceeded the 30-second deadline; its effects may have committed",
            )),
            response = client.on_execute_command(request) => response,
        };
        match response {
            Ok(response) => {
                permit.success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                permit.failure(&format!("Command gRPC error: {}", status.code()));
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
        let mut request = tonic::Request::new(GetPluginMetaRequest {});
        request.set_timeout(Duration::from_secs(10));
        let response = client.get_plugin_meta(request).await?;
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
        let Some(permit) = self.circuit_breaker.try_acquire() else {
            tracing::warn!(
                host_id = %self.host_id,
                tool_name = %req.tool_name,
                "Circuit breaker is OPEN; fast-skipping tool call"
            );
            return Err(tonic::Status::unavailable(format!(
                "Circuit breaker is OPEN for host '{}'",
                self.host_id
            )));
        };

        let start = std::time::Instant::now();
        let mut client = self.pipeline_client.clone();
        match client.on_call_tool(req).await {
            Ok(response) => {
                permit.success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                permit.failure(&format!("Tool call gRPC error: {}", status.code()));
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
        let Some(permit) = self.circuit_breaker.try_acquire() else {
            tracing::warn!(
                host_id = %self.host_id,
                action = %req.action,
                "Circuit breaker is OPEN; fast-skipping management action"
            );
            return Err(tonic::Status::unavailable(format!(
                "Circuit breaker is OPEN for host '{}'",
                self.host_id
            )));
        };

        let start = std::time::Instant::now();
        let mut client = self.host_client.clone();
        match client.invoke_action(req).await {
            Ok(response) => {
                permit.success(start.elapsed());
                Ok(response.into_inner())
            }
            Err(status) => {
                permit.failure(&format!("Management action gRPC error: {}", status.code()));
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
