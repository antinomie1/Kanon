//! Shared management-gateway state and its composition-root builder.
//!
//! [`ApiState`] is the single dependency bundle handed to every route handler. It is cheap to
//! clone (one `Arc` bump) because Axum clones state per request; all shared owners — supervisor,
//! session manager, persona registry, agent, observability channels — are themselves `Arc`-held.
//!
//! The builder is the honest composition root: it either receives fully constructed components
//! or builds them from a model provider, and it refuses to fabricate a fake agent when no
//! provider is configured. In that case `/api/v1/chat/completions` answers `503` explicitly.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use kanon_adapter_milky::MilkyAdapter;
use kanon_adapter_onebot::OneBotAdapter;
use kanon_adapter_qqofficial::QqOfficialAdapter;
use kanon_core::{
    BashAvailabilityHook, BashPolicyStore, BashTool, ContextPolicyStore, EventIngress,
    EventPolicyStore, InstanceRegistry, McpConfigStore, McpPool, ModelBashReviewer,
    ReplyPolicyStore, SkillStore, Supervisor, ToggleStore,
};
use kanon_llm::{
    Agent, AgentConfig, AgentFactory, AgentSlot, InMemory, LlmProvider, Memory, PersonaRegistry,
    ProviderRuntime, SessionManager,
};

use crate::error::ApiError;
use crate::llm_config::{NodeSettings, SystemConfigStore};
use crate::observability::Observability;
use crate::persona_store::PersonaStore;
use crate::plugin_config::PluginConfigStore;

/// Shared, cloneable state injected into every management route.
#[derive(Clone)]
pub struct ApiState {
    inner: Arc<ApiStateInner>,
}

/// Owners shared across all requests.
struct ApiStateInner {
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
    /// Live caller policy also held by the Bash tool and availability hook.
    bash_policy: Arc<BashPolicyStore>,
    /// Typed tool handle for explicit persistent-container reset.
    bash_tool: Option<Arc<BashTool>>,
    /// Node-wide notice policy, shared with the pipeline worker.
    event_policy: Arc<EventPolicyStore>,
    /// In-memory view of the persisted model-routing settings.
    ///
    /// Kept alongside the store so a read (listing providers, resolving a model) never touches the
    /// filesystem, while every write goes through [`ApiState::apply_node_settings`].
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
}

impl ApiState {
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
    pub fn agent(&self) -> Option<Arc<Agent>> {
        self.inner.factory.node_agent()
    }

    /// Agent that should serve a request using an optional model override.
    ///
    /// A blank or default model resolves to the node agent, so callers never need to compare
    /// model identifiers themselves.
    pub fn agent_for_model(&self, model: Option<&str>) -> Option<Arc<Agent>> {
        self.inner.factory.agent_for_model(model)
    }

    /// Requires an agent runtime, failing with `503` when none is configured.
    pub fn require_agent(&self) -> Result<Arc<Agent>, ApiError> {
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
    ) -> Arc<Agent> {
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

    /// Current Bash permission policy, shared with the execution gate.
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
        settings.validate()?;

        self.inner.system_config.save_node_settings(&settings)?;
        self.inner.factory.configure(
            "kanon-core",
            ProviderRuntime {
                providers: settings.providers.clone(),
                default_model: settings.default_model.clone(),
                models: settings.models.clone(),
            },
        )?;
        self.inner.reply_policy.set(settings.reply_policy);
        self.inner.context_policy.set(settings.context_policy);
        self.inner.bash_policy.set(settings.bash_policy.clone());
        self.inner.event_policy.set(settings.event_policy);
        *self
            .inner
            .node_settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
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

        let mut settings = self.node_settings();
        let written = crate::model_discovery::merge_discovered(&mut settings.models, &discovered);
        if written == 0 {
            return 0;
        }

        match self.apply_node_settings(settings) {
            Ok(()) => {
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
}

/// Fluent builder assembling [`ApiState`].
pub struct ApiStateBuilder {
    supervisor: Arc<Supervisor>,
    version: String,
    sessions: Option<Arc<SessionManager>>,
    personas: Option<Arc<PersonaRegistry>>,
    persona_store: Option<Arc<PersonaStore>>,
    memory: Option<Arc<dyn Memory>>,
    agent: Option<Arc<Agent>>,
    pending_llm: Option<PendingLlm>,
    agent_slot: Option<Arc<AgentSlot>>,
    native_tools: Vec<Arc<dyn kanon_llm::AgentTool>>,
    hooks: Vec<Arc<dyn kanon_llm::AgentHook>>,
    instances: Option<Arc<InstanceRegistry>>,
    plugin_state: Option<Arc<ToggleStore>>,
    mcp: Option<Arc<McpPool>>,
    mcp_config: Option<Arc<McpConfigStore>>,
    skills: Option<Arc<SkillStore>>,
    system_config: Option<Arc<SystemConfigStore>>,
    node_settings: Option<NodeSettings>,
    bash_policy: Option<Arc<BashPolicyStore>>,
    bash_tool: Option<Arc<BashTool>>,
    milky: Option<Arc<MilkyAdapter>>,
    /// OneBot v11 adapter hosted by this node.
    onebot: Option<Arc<OneBotAdapter>>,
    /// QQ Official adapter hosted by this node.
    qqofficial: Option<Arc<QqOfficialAdapter>>,
    config_base_dir: Option<PathBuf>,
    observability: Option<Arc<Observability>>,
    ingress: Option<EventIngress>,
    plugins_dir: Option<PathBuf>,
}

/// Model provider awaiting agent construction at build time.
struct PendingLlm {
    name: String,
    provider: Arc<dyn LlmProvider>,
    config: AgentConfig,
}

impl ApiStateBuilder {
    /// Creates a builder for the given supervisor.
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        Self {
            supervisor,
            version: env!("CARGO_PKG_VERSION").to_string(),
            sessions: None,
            personas: None,
            persona_store: None,
            memory: None,
            agent: None,
            pending_llm: None,
            agent_slot: None,
            native_tools: Vec::new(),
            hooks: Vec::new(),
            instances: None,
            plugin_state: None,
            mcp: None,
            mcp_config: None,
            skills: None,
            system_config: None,
            node_settings: None,
            bash_policy: None,
            bash_tool: None,
            milky: None,
            onebot: None,
            qqofficial: None,
            config_base_dir: None,
            observability: None,
            ingress: None,
            plugins_dir: None,
        }
    }

    /// Overrides the version string reported by the gateway.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Overrides the directory where plugins are discovered and installed.
    pub fn with_plugins_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plugins_dir = Some(dir.into());
        self
    }

    /// Injects a pre-built conversation memory backend.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Injects a pre-built session manager (its memory becomes the shared memory backend).
    pub fn with_sessions(mut self, sessions: Arc<SessionManager>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Injects a pre-built persona registry.
    ///
    /// The composition root loads the operator's personas from the store and registers them before
    /// handing the registry over, so the builder never reads `data/personas.json` itself.
    pub fn with_personas(mut self, personas: Arc<PersonaRegistry>) -> Self {
        self.personas = Some(personas);
        self
    }

    /// Overrides where operator-defined personas are persisted (`data/personas.json` by default).
    pub fn with_persona_store(mut self, store: Arc<PersonaStore>) -> Self {
        self.persona_store = Some(store);
        self
    }

    /// Injects a fully constructed agent.
    ///
    /// Prefer [`ApiStateBuilder::with_llm_provider`] unless the agent already carries its own
    /// lifecycle hooks: an agent built elsewhere will not publish tool-calling trace events.
    /// Ignored when an explicit slot is injected, which stays authoritative.
    pub fn with_agent(mut self, agent: Arc<Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Shares an externally owned agent slot as the node's provider source.
    ///
    /// The composition root uses this to hand the *same* slot to the pipeline worker, the IPC
    /// service and the management gateway. When a slot is injected it is authoritative: the
    /// builder neither constructs nor overwrites an agent for it.
    pub fn with_agent_slot(mut self, slot: Arc<AgentSlot>) -> Self {
        self.agent_slot = Some(slot);
        self
    }

    /// Builds the agent runtime from a model provider.
    ///
    /// The resulting agent shares this gateway's memory, session manager and persona registry,
    /// and carries the event bus as an [`kanon_llm::AgentHook`] so that LLM request/response and
    /// tool-calling stages appear on `/ws/v1/events`.
    pub fn with_llm_provider(
        mut self,
        name: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
        config: AgentConfig,
    ) -> Self {
        self.pending_llm = Some(PendingLlm {
            name: name.into(),
            provider,
            config,
        });
        self
    }

    /// Registers native in-process tools that every agent may call (e.g. `read_skill`).
    pub fn with_native_tools(mut self, tools: Vec<Arc<dyn kanon_llm::AgentTool>>) -> Self {
        self.native_tools = tools;
        self
    }

    /// Registers lifecycle hooks every agent runs on the node (e.g. the skill catalog).
    ///
    /// Hooks registered here run after the built-in trace hook, so an operator-visible hook never
    /// hides the observability stream.
    pub fn with_hooks(mut self, hooks: Vec<Arc<dyn kanon_llm::AgentHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Shares the persisted plugin enable/disable state.
    pub fn with_plugin_state(mut self, state: Arc<ToggleStore>) -> Self {
        self.plugin_state = Some(state);
        self
    }

    /// Shares the MCP client pool used by both the console and the pipeline.
    pub fn with_mcp_pool(mut self, mcp: Arc<McpPool>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Shares the persisted MCP server definitions.
    pub fn with_mcp_config(mut self, config: Arc<McpConfigStore>) -> Self {
        self.mcp_config = Some(config);
        self
    }

    /// Shares the installed-skill store.
    pub fn with_skill_store(mut self, skills: Arc<SkillStore>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Shares the bot-instance catalog that gates and partitions inbound events.
    pub fn with_instances(mut self, instances: Arc<InstanceRegistry>) -> Self {
        self.instances = Some(instances);
        self
    }

    /// Overrides the node-level system configuration store.
    ///
    /// Defaults to the node's `data/system.json`, which is where the provider endpoints persist
    /// the operator's selection.
    pub fn with_system_config(mut self, store: Arc<SystemConfigStore>) -> Self {
        self.system_config = Some(store);
        self
    }

    /// Seeds the node's model-routing settings.
    ///
    /// The composition root loads them from `data/system.json` (falling back to the
    /// `KANON_LLM_*` environment) and hands them over; the builder never reads the file itself so
    /// an embedded gateway or a test cannot accidentally adopt a real node's configuration.
    pub fn with_node_settings(mut self, settings: NodeSettings) -> Self {
        self.node_settings = Some(settings);
        self
    }

    /// Shares the policy store used by the native Bash tool and its availability hook.
    pub fn with_bash_policy(mut self, policy: Arc<BashPolicyStore>) -> Self {
        self.bash_policy = Some(policy);
        self
    }

    /// Registers Bash, its per-turn status hook and its management handle together.
    pub fn with_bash_tool(mut self, tool: Arc<BashTool>) -> Self {
        self.native_tools.push(tool.clone());
        self.hooks
            .push(Arc::new(BashAvailabilityHook(tool.clone())));
        self.bash_tool = Some(tool);
        self
    }

    /// Shares the OneBot v11 adapter registered by the composition root.
    pub fn with_onebot_adapter(mut self, adapter: Arc<OneBotAdapter>) -> Self {
        self.onebot = Some(adapter);
        self
    }

    /// Shares the QQ Official adapter registered by the composition root.
    pub fn with_qqofficial_adapter(mut self, adapter: Arc<QqOfficialAdapter>) -> Self {
        self.qqofficial = Some(adapter);
        self
    }

    /// Shares the Milky platform adapter this node registered.
    ///
    /// The gateway never constructs the adapter itself: registration must happen before
    /// `start_all` hands it the ingest queue, so the composition root owns its life cycle and only
    /// lends it to the management routes.
    pub fn with_milky_adapter(mut self, adapter: Arc<MilkyAdapter>) -> Self {
        self.milky = Some(adapter);
        self
    }

    /// Overrides the base directory used to persist plugin configuration.
    pub fn with_config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config_base_dir = Some(dir.into());
        self
    }

    /// Injects a pre-built observability hub (shared log/trace channels and metrics).
    pub fn with_observability(mut self, observability: Arc<Observability>) -> Self {
        self.observability = Some(observability);
        self
    }

    /// Injects the Fast-ACK ingest handle that connects the adapter data plane to the pipeline.
    ///
    /// Without it the ingest endpoint answers `503`: the gateway refuses to accept messages it
    /// cannot hand to a running pipeline.
    pub fn with_ingress(mut self, ingress: EventIngress) -> Self {
        self.ingress = Some(ingress);
        self
    }

    /// Finalizes the state graph.
    pub fn build(self) -> ApiState {
        let observability = self
            .observability
            .unwrap_or_else(|| Arc::new(Observability::new()));

        // Memory resolution order: explicit backend, session manager's backend, in-memory default.
        let memory = self
            .memory
            .or_else(|| self.sessions.as_ref().map(|sm| sm.memory().clone()))
            .unwrap_or_else(|| Arc::new(InMemory::new()));

        let sessions = self
            .sessions
            .unwrap_or_else(|| Arc::new(SessionManager::new(memory.clone())));
        let personas = self.personas.unwrap_or_default();
        let persona_store = self
            .persona_store
            .unwrap_or_else(|| Arc::new(PersonaStore::default()));

        // Agent construction is centralised in the factory so the node agent, per-instance model
        // overrides and the console sandbox all share one memory, session manager, persona
        // registry and trace bus. A caller-supplied slot is adopted verbatim (the composition root
        // hands the same slot to the pipeline and the IPC service).
        let slot = self
            .agent_slot
            .unwrap_or_else(|| Arc::new(AgentSlot::new()));
        // Order is semantic, not cosmetic:
        // 1. operator-registered hooks (skill catalog, RAG, ...) add their context;
        // 2. the trace hook runs last so `llm_request.message_count` describes the request the
        //    provider actually receives, injection included.
        let mut hooks: Vec<Arc<dyn kanon_llm::AgentHook>> = Vec::new();
        hooks.extend(self.hooks);
        hooks.push(observability.events.clone());
        let factory = Arc::new(AgentFactory::new(
            "kanon-core",
            slot.clone(),
            memory,
            sessions.clone(),
            personas.clone(),
            hooks,
            self.native_tools.clone(),
        ));
        if let Some(tool) = &self.bash_tool {
            tool.set_reviewer(Arc::new(ModelBashReviewer::new(Arc::downgrade(&factory))));
        }

        // A provider the builder was handed is installed into the shared slot; dropping it
        // silently would leave the node reporting a provider it cannot use. A caller that supplied
        // model-routing settings instead gets the named-provider directory applied here, which is
        // how the composition root restores `data/system.json` at startup.
        let node_settings = self.node_settings.unwrap_or_default();
        if let Some(agent) = self.agent {
            slot.set(Some(agent));
        } else if let Some(pending) = self.pending_llm {
            factory.install(pending.name, pending.provider, pending.config);
        } else if node_settings.has_providers() {
            if let Err(err) = factory.configure(
                "kanon-core",
                ProviderRuntime {
                    providers: node_settings.providers.clone(),
                    default_model: node_settings.default_model.clone(),
                    models: node_settings.models.clone(),
                },
            ) {
                // A malformed persisted directory must not prevent the node from starting: the
                // gateway still serves its management API, and the operator can fix the endpoint
                // from the console. Reporting it loudly is what keeps that recoverable.
                tracing::error!(
                    error = %err,
                    "Persisted model provider directory is invalid; the node starts without a provider"
                );
            }
        }
        let reply_policy = Arc::new(ReplyPolicyStore::new(node_settings.reply_policy));
        let context_policy = Arc::new(ContextPolicyStore::new(node_settings.context_policy));
        let bash_policy = self
            .bash_tool
            .as_ref()
            .map(|tool| tool.policy().clone())
            .or(self.bash_policy)
            .unwrap_or_default();
        bash_policy.set(node_settings.bash_policy.clone());
        let event_policy = Arc::new(EventPolicyStore::new(node_settings.event_policy));

        let instances = self.instances.unwrap_or_default();
        let plugin_state = self.plugin_state.unwrap_or_default();
        let mcp = self.mcp.unwrap_or_default();
        let mcp_config = self.mcp_config.unwrap_or_default();
        let skills = self
            .skills
            .unwrap_or_else(|| Arc::new(SkillStore::new(kanon_core::DEFAULT_SKILLS_DIR)));

        let config_store = Arc::new(match self.config_base_dir {
            Some(dir) => PluginConfigStore::new(dir),
            None => PluginConfigStore::default(),
        });

        let system_config = self
            .system_config
            .unwrap_or_else(|| Arc::new(SystemConfigStore::default()));

        let plugins_dir = self
            .plugins_dir
            .unwrap_or_else(|| PathBuf::from("./plugins"));

        ApiState {
            inner: Arc::new(ApiStateInner {
                started_at: Instant::now(),
                version: self.version,
                supervisor: self.supervisor,
                sessions,
                personas,
                persona_store,
                factory,
                instances,
                plugin_state,
                mcp,
                mcp_config,
                skills,
                config_store,
                system_config,
                reply_policy,
                context_policy,
                bash_policy,
                bash_tool: self.bash_tool,
                event_policy,
                node_settings: Arc::new(RwLock::new(node_settings)),
                milky: self.milky,
                onebot: self.onebot,
                qqofficial: self.qqofficial,
                observability,
                ingress: self.ingress,
                plugins_dir,
            }),
        }
    }
}

/// Applies the default agent configuration used by the standalone gateway binary.
pub fn default_agent_config(model: impl Into<String>) -> AgentConfig {
    AgentConfig {
        default_model: model.into(),
        ..AgentConfig::default()
    }
}
