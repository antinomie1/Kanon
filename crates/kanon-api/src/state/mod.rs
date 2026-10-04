//! Shared management-gateway state and its composition-root builder.
//!
//! [`ApiState`] is the single dependency bundle handed to every route handler. It is cheap to
//! clone (one `Arc` bump) because Axum clones state per request; all shared owners — supervisor,
//! session manager, persona registry, agent, observability channels — are themselves `Arc`-held.
//!
//! The builder is the honest composition root: it either receives fully constructed components
//! or builds them from a model provider, and it refuses to fabricate a fake agent when no
//! provider is configured. In that case `/api/v1/chat/completions` answers `503` explicitly.

mod builder;
pub use builder::ApiStateBuilder;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use kanon_adapter_milky::MilkyAdapter;
use kanon_adapter_onebot::OneBotAdapter;
use kanon_adapter_qqofficial::QqOfficialAdapter;
use kanon_core::{
    BashAvailabilityHook, BashPolicyStore, BashTool, CommandPolicyStore, ContextPolicyStore,
    DiscoveredPlugin, EventIngress, EventPolicyStore, InstanceRegistry, McpConfigStore, McpPool,
    ModelBashReviewer, PluginScanner, ReplyPolicyStore, SkillStore, Supervisor, ToggleStore,
};
use kanon_llm::{
    Agent, AgentConfig, AgentFactory, AgentSlot, InMemory, LlmProvider, Memory, PersonaRegistry,
    PersonaStore, ProviderRuntime, SessionManager,
};

use crate::error::ApiError;
use crate::llm_config::{NodeSettings, StartupConfig, SystemConfigStore};
use crate::observability::Observability;
use crate::plugin_config::PluginConfigStore;

/// Shared, cloneable state injected into every management route.
#[derive(Clone)]
pub struct ApiState {
    inner: Arc<ApiStateInner>,
    /// The running engine, attached after composing the shared factory and policy stores.
    pipeline: Option<Arc<kanon_core::pipeline::PipelineEngine>>,
}

/// Owners shared across all requests.
struct ApiStateInner {
    /// Startup-only gateway access settings; never changed by a management request.
    startup: StartupConfig,
    /// Instant the gateway process started, used for uptime reporting.
    started_at: Instant,
    /// Semantic version reported by the health and metrics endpoints.
    version: String,
    /// Process supervisor owning plugin host sub-processes.
    supervisor: Arc<Supervisor>,
    /// Conversation session lifecycle manager.
    sessions: Arc<SessionManager>,
    /// Persona catalog backing the persona endpoints.
    personas: Arc<PersonaRegistry>,
    /// Persistence of the operator-defined personas in `personas`.
    persona_store: Arc<PersonaStore>,
    /// Live agent runtime backing chat completions, conversational pipeline turns and IPC
    /// `RequestLLM`.
    ///
    /// The factory owns the node's provider slot and builds per-instance model overrides, so
    /// provider changes made through the control plane take effect on the next request without
    /// restarting the node.
    factory: Arc<AgentFactory>,
    /// Catalog of bot instances deciding whether and how inbound events are answered.
    instances: Arc<InstanceRegistry>,
    /// Persisted enable/disable state for discovered plugins, skills and MCP servers.
    plugin_state: Arc<ToggleStore>,
    /// MCP client pool contributing tools to the very same router as plugin hosts.
    mcp: Arc<McpPool>,
    /// Persisted MCP server definitions backing the console's server editor.
    mcp_config: Arc<McpConfigStore>,
    /// Installed skills backing the catalog hook and the `read_skill` tool.
    skills: Arc<SkillStore>,
    /// Persistence for per-plugin configuration values.
    config_store: Arc<PluginConfigStore>,
    /// Persistence for node-level system settings, including the console-selected provider.
    system_config: Arc<SystemConfigStore>,
    /// Node-wide reply policy shared with the pipeline worker.
    reply_policy: Arc<ReplyPolicyStore>,
    /// Node-wide context-extras policy shared with the pipeline worker.
    context_policy: Arc<ContextPolicyStore>,
    /// Node-wide notice policy, shared with the pipeline worker.
    event_policy: Arc<EventPolicyStore>,
    /// Node-wide command permissions, shared with the pipeline worker and the Bash tool.
    command_policy: Arc<CommandPolicyStore>,
    /// Bash switch and execution backend, shared with the Bash tool.
    bash_policy: Arc<BashPolicyStore>,
    /// Typed tool handle for explicit persistent-container reset.
    bash_tool: Option<Arc<BashTool>>,
    /// In-memory view of the persisted model-routing settings.
    ///
    /// Kept alongside the store so a read (listing providers, resolving a model) never touches the
    /// filesystem, while partial writes go through [`ApiState::update_node_settings`].
    node_settings: Arc<RwLock<NodeSettings>>,
    /// Milky platform adapter owned by this node, absent when the composition root registered none.
    ///
    /// Held as the concrete type rather than through the registry's `dyn PlatformAdapter`, because
    /// the console reconfigures it and reads its connection status — neither of which the generic
    /// adapter contract exposes.
    milky: Option<Arc<MilkyAdapter>>,
    /// OneBot v11 adapter hosted by this node.
    onebot: Option<Arc<OneBotAdapter>>,
    /// QQ Official adapter hosted by this node.
    qqofficial: Option<Arc<QqOfficialAdapter>>,
    /// Real-time log and trace channels plus the metrics registry.
    observability: Arc<Observability>,
    /// Fast-ACK ingest handle driving the inbound data plane, absent when no pipeline is attached.
    ingress: Option<EventIngress>,
    /// Base directory where discovered and installed plugins are stored.
    plugins_dir: PathBuf,
    /// Plugins the last scan of `plugins_dir` found, plus any installed through the gateway since.
    ///
    /// The directory is never scanned behind the operator's back: the node scans it once at startup
    /// and again only when the console asks for a rescan, so a folder dropped into it shows up on
    /// the next manual refresh rather than on the next catalog read. Listing a stopped plugin and
    /// enabling it both read this snapshot, so the two can never disagree about what is on disk.
    plugins_on_disk: RwLock<Vec<DiscoveredPlugin>>,
}

impl ApiState {
    /// Attaches the existing engine so console turns use its admission and tool scope.
    pub fn with_pipeline(mut self, pipeline: Arc<kanon_core::pipeline::PipelineEngine>) -> Self {
        self.pipeline = Some(pipeline);
        self
    }

    /// The running engine, absent in gateways that only expose management resources.
    pub fn pipeline(&self) -> Option<&Arc<kanon_core::pipeline::PipelineEngine>> {
        self.pipeline.as_ref()
    }

    /// Startup settings used to bind and protect the management gateway.
    pub fn startup(&self) -> &StartupConfig {
        &self.inner.startup
    }

    /// Returns a fluent builder rooted at a supervisor instance.
    pub fn builder(supervisor: Arc<Supervisor>) -> ApiStateBuilder {
        ApiStateBuilder::new(supervisor)
    }

    /// Gateway start instant, used to compute uptime.
    pub fn started_at(&self) -> Instant {
        self.inner.started_at
    }

    /// Reported gateway version.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// Plugin supervisor handle.
    pub fn supervisor(&self) -> &Arc<Supervisor> {
        &self.inner.supervisor
    }

    /// Session manager handle.
    pub fn sessions(&self) -> &Arc<SessionManager> {
        &self.inner.sessions
    }

    /// Persona registry handle.
    pub fn personas(&self) -> &Arc<PersonaRegistry> {
        &self.inner.personas
    }

    /// Persistence of the operator-defined personas.
    pub fn persona_store(&self) -> &Arc<PersonaStore> {
        &self.inner.persona_store
    }

    /// Agent runtime handle, if a model provider was configured.
    ///
    /// Returns a snapshot of the live slot: callers always observe the provider that is
    /// configured *now*, which is what lets the console change it at runtime.
    pub fn agent(&self) -> Option<Arc<dyn Agent>> {
        self.inner.factory.node_agent()
    }

    /// Agent that should serve a request using an optional model override.
    ///
    /// A blank or default model resolves to the node agent, so callers never need to compare
    /// model identifiers themselves.
    pub fn agent_for_model(&self, model: Option<&str>) -> Option<Arc<dyn Agent>> {
        self.inner.factory.agent_for_model(model)
    }

    /// Requires an agent runtime, failing with `503` when none is configured.
    pub fn require_agent(&self) -> Result<Arc<dyn Agent>, ApiError> {
        self.agent().ok_or_else(|| {
            ApiError::Unavailable(
                "No LLM provider is configured for this core; chat completions are disabled"
                    .to_string(),
            )
        })
    }

    /// Agent factory shared with the pipeline worker and the console sandbox.
    pub fn agent_factory(&self) -> &Arc<AgentFactory> {
        &self.inner.factory
    }

    /// Shared agent slot, used by the composition root to wire the IPC service to the node's
    /// provider source.
    pub fn llm_slot(&self) -> &Arc<AgentSlot> {
        self.inner.factory.slot()
    }

    /// Bot-instance catalog.
    pub fn instances(&self) -> &Arc<InstanceRegistry> {
        &self.inner.instances
    }

    /// Persisted plugin enable/disable state.
    pub fn plugin_state(&self) -> &Arc<ToggleStore> {
        &self.inner.plugin_state
    }

    /// MCP client pool shared with the pipeline worker.
    pub fn mcp(&self) -> &Arc<McpPool> {
        &self.inner.mcp
    }

    /// Persisted MCP server definitions.
    pub fn mcp_config(&self) -> &Arc<McpConfigStore> {
        &self.inner.mcp_config
    }

    /// Installed skills backing the console's skill management endpoints.
    pub fn skills(&self) -> &Arc<SkillStore> {
        &self.inner.skills
    }

    /// Installs a model provider on the running node and returns the resulting agent.
    ///
    /// The pipeline worker, the chat endpoints and the IPC gateway all observe the swap on their
    /// next call; nothing needs to be restarted and no request in flight is disturbed.
    pub fn apply_llm_provider(
        &self,
        name: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
        config: AgentConfig,
    ) -> Arc<dyn Agent> {
        self.inner.factory.install(name, provider, config)
    }

    /// Clears the configured provider, disabling chat and conversational routing.
    pub fn clear_llm_provider(&self) {
        self.inner.factory.clear();
    }

    /// Plugin configuration store handle.
    pub fn config_store(&self) -> &Arc<PluginConfigStore> {
        &self.inner.config_store
    }

    /// Node-level system configuration store handle.
    pub fn system_config(&self) -> &Arc<SystemConfigStore> {
        &self.inner.system_config
    }

    /// Node-wide reply policy shared with the pipeline worker.
    pub fn reply_policy(&self) -> &Arc<ReplyPolicyStore> {
        &self.inner.reply_policy
    }

    /// Node-wide context-extras policy shared with the pipeline worker.
    pub fn context_policy(&self) -> &Arc<ContextPolicyStore> {
        &self.inner.context_policy
    }

    /// Current Bash switch and execution backend, shared with the execution gate.
    pub fn bash_policy(&self) -> &Arc<BashPolicyStore> {
        &self.inner.bash_policy
    }

    /// Native Bash runtime managed by the node, when registered.
    pub fn bash_tool(&self) -> Option<&Arc<BashTool>> {
        self.inner.bash_tool.as_ref()
    }

    /// Node-wide notice policy shared with the pipeline worker.
    pub fn event_policy(&self) -> &Arc<EventPolicyStore> {
        &self.inner.event_policy
    }

    /// Node-wide command permissions shared with the pipeline worker.
    pub fn command_policy(&self) -> &Arc<CommandPolicyStore> {
        &self.inner.command_policy
    }

    /// Snapshot of the persisted model-routing settings.
    pub fn node_settings(&self) -> NodeSettings {
        self.inner
            .node_settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Validates, persists and applies model-routing settings, then publishes them.
    ///
    /// Order is *validate → persist → apply → publish*: nothing reaches disk before the settings
    /// are known-good, and the in-memory snapshot is updated only after the running node accepted
    /// the directory, so a failed apply cannot leave the console describing a node that does not
    /// exist.
    pub fn apply_node_settings(&self, settings: NodeSettings) -> Result<(), String> {
        let mut current = self
            .inner
            .node_settings
            .write()
            .map_err(|_| "Node settings lock is poisoned")?;
        self.persist_node_settings(&settings)?;
        *current = settings;
        Ok(())
    }

    /// Changes current settings under one write lock, including persistence and publication.
    ///
    /// Management handlers must mutate inside this closure instead of saving a snapshot read
    /// before the lock. Otherwise two independent form saves can silently undo one another.
    /// Network discovery happens before this method and must recheck its inputs in the closure.
    pub fn update_node_settings<T>(
        &self,
        change: impl FnOnce(&mut NodeSettings) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut current = self
            .inner
            .node_settings
            .write()
            .map_err(|_| ApiError::Internal("Node settings lock is poisoned".into()))?;
        let mut candidate = current.clone();
        let result = change(&mut candidate)?;
        self.persist_node_settings(&candidate)
            .map_err(ApiError::BadRequest)?;
        *current = candidate;
        Ok(result)
    }

    /// Validates, persists and applies while the caller holds the settings write lock.
    fn persist_node_settings(&self, settings: &NodeSettings) -> Result<(), String> {
        settings.validate()?;
        #[cfg(feature = "dsh")]
        let dsh = self.inner.factory.prepare_dsh(settings.dsh.as_ref())?;

        self.inner.system_config.save_node_settings(settings)?;
        self.inner.factory.configure(
            "kanon-core",
            ProviderRuntime {
                providers: settings.providers.clone(),
                default_model: settings.default_model.clone(),
                models: settings.models.clone(),
            },
        )?;
        self.inner.factory.set_backend(
            &settings.default_agent,
            #[cfg(feature = "dsh")]
            dsh,
        )?;
        self.inner.reply_policy.set(settings.reply_policy);
        self.inner.context_policy.set(settings.context_policy);
        self.inner.event_policy.set(settings.event_policy);
        self.inner
            .command_policy
            .set(settings.command_policy.clone());
        self.inner.bash_policy.set(settings.bash_policy.clone());
        Ok(())
    }

    /// Populates the model catalog for every endpoint that has no entries yet.
    ///
    /// Best-effort by design: an endpoint may be unreachable when the node starts, and a missing
    /// catalog entry only means the operator runs discovery from the console. Returns how many
    /// entries were written, which is what the caller logs.
    pub async fn autofill_model_catalog(&self) -> usize {
        let missing: Vec<String> = {
            let settings = self.node_settings();
            settings
                .providers
                .iter()
                .filter(|entry| {
                    !settings
                        .models
                        .iter()
                        .any(|model| model.provider == entry.name)
                })
                .map(|entry| entry.name.clone())
                .collect()
        };

        let mut written = 0usize;
        for provider in missing {
            written += self.autofill_provider_models(&provider).await;
        }
        written
    }

    /// Populates (or refreshes) the catalog for one endpoint.
    ///
    /// An entry the operator edited by hand marks the endpoint as curated, and nothing is
    /// overwritten. Otherwise the listing is re-read, which is what keeps an upstream correction —
    /// a newly reported context window or modality — flowing into the node on every restart.
    pub async fn autofill_provider_models(&self, provider: &str) -> usize {
        let curated = self.node_settings().models.iter().any(|model| {
            model.provider == provider && model.source == kanon_llm::ModelSettingsSource::Manual
        });
        if curated {
            return 0;
        }

        let Some(entry) = self
            .node_settings()
            .providers
            .into_iter()
            .find(|entry| entry.name == provider)
        else {
            return 0;
        };

        // A hung endpoint must not hold a management request (or startup) open forever.
        let discovered = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::model_discovery::discover_models(&entry),
        )
        .await
        {
            Ok(Ok(models)) => models,
            Ok(Err(err)) => {
                tracing::warn!(
                    provider = %provider,
                    error = %err,
                    "Model discovery failed; the catalog stays as configured"
                );
                return 0;
            }
            Err(_) => {
                tracing::warn!(
                    provider = %provider,
                    "Model discovery timed out; the catalog stays as configured"
                );
                return 0;
            }
        };

        if discovered.is_empty() {
            return 0;
        }

        let update = self.update_node_settings(|settings| {
            // A slow discovery must not resurrect a deleted provider or overwrite an edit made
            // while its HTTP request was in flight.
            if !settings.providers.iter().any(|current| current == &entry)
                || settings.models.iter().any(|model| {
                    model.provider == provider
                        && model.source == kanon_llm::ModelSettingsSource::Manual
                })
            {
                return Ok(0);
            }
            Ok(crate::model_discovery::merge_discovered(
                &mut settings.models,
                &discovered,
            ))
        });
        match update {
            Ok(written) => {
                tracing::info!(
                    provider = %provider,
                    count = written,
                    "Model catalog auto-filled from the endpoint"
                );
                written
            }
            Err(err) => {
                tracing::warn!(
                    provider = %provider,
                    error = %err,
                    "Failed to store auto-discovered models"
                );
                0
            }
        }
    }

    /// OneBot v11 adapter handle, when this node hosts one.
    pub fn onebot(&self) -> Option<&Arc<OneBotAdapter>> {
        self.inner.onebot.as_ref()
    }

    /// QQ Official adapter handle, when this node hosts one.
    pub fn qqofficial(&self) -> Option<&Arc<QqOfficialAdapter>> {
        self.inner.qqofficial.as_ref()
    }

    /// Milky platform adapter handle, when this node hosts one.
    pub fn milky(&self) -> Option<&Arc<MilkyAdapter>> {
        self.inner.milky.as_ref()
    }

    /// Observability hub handle.
    pub fn observability(&self) -> &Arc<Observability> {
        &self.inner.observability
    }

    /// Inbound ingest handle used by the adapter data plane.
    pub fn ingress(&self) -> Option<&EventIngress> {
        self.inner.ingress.as_ref()
    }

    /// Plugins directory handle.
    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.inner.plugins_dir
    }

    /// Plugins found on disk by the last scan, plus any installed through the gateway since.
    pub fn plugins_on_disk(&self) -> Vec<DiscoveredPlugin> {
        self.inner
            .plugins_on_disk
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Scans the plugins directory and replaces the snapshot with what it finds.
    ///
    /// This is the only way a plugin copied into the directory by hand becomes known to the node:
    /// the composition root calls it once at startup and the console calls it from its refresh
    /// button. A failed scan leaves the previous snapshot in place and reports the error.
    pub fn rescan_plugins(&self) -> Result<Vec<DiscoveredPlugin>, std::io::Error> {
        let found = PluginScanner::scan(&self.inner.plugins_dir)?;
        *self
            .inner
            .plugins_on_disk
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = found.clone();
        Ok(found)
    }

    /// Records a plugin the gateway just installed, replacing an earlier entry with the same id.
    ///
    /// Installing is an explicit operator action, so the plugin is known straight away even when
    /// its host then fails to start, without rescanning the rest of the directory.
    pub(crate) fn record_installed_plugin(&self, plugin: DiscoveredPlugin) {
        let mut on_disk = self
            .inner
            .plugins_on_disk
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        on_disk.retain(|known| known.manifest.plugin.id != plugin.manifest.plugin.id);
        on_disk.push(plugin);
    }
}

/// Applies the default agent configuration used by the standalone gateway binary.
pub fn default_agent_config(model: impl Into<String>) -> AgentConfig {
    AgentConfig {
        default_model: model.into(),
        ..AgentConfig::default()
    }
}
