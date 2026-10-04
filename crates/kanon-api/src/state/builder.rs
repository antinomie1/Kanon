//! Composition of shared API services and optional agent backends.

use super::*;

/// Fluent builder assembling [`ApiState`].
pub struct ApiStateBuilder {
    startup: StartupConfig,
    supervisor: Arc<Supervisor>,
    version: String,
    sessions: Option<Arc<SessionManager>>,
    personas: Option<Arc<PersonaRegistry>>,
    persona_store: Option<Arc<PersonaStore>>,
    memory: Option<Arc<dyn Memory>>,
    agent: Option<Arc<dyn Agent>>,
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
            startup: StartupConfig::default(),
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

    /// Supplies the immutable startup configuration loaded by the composition root.
    pub fn with_startup(mut self, startup: StartupConfig) -> Self {
        self.startup = startup;
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
    pub fn with_agent(mut self, agent: Arc<dyn Agent>) -> Self {
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
    /// The composition root loads them from `data/system.json` and hands them over;
    /// the builder never reads the file itself so
    /// an embedded gateway or a test cannot accidentally adopt a real node's configuration.
    pub fn with_node_settings(mut self, settings: NodeSettings) -> Self {
        self.node_settings = Some(settings);
        self
    }

    /// Registers Bash, its `send_file` companion, its per-turn status hook and its management
    /// handle together.
    pub fn with_bash_tool(mut self, tool: Arc<BashTool>) -> Self {
        self.native_tools.push(tool.clone());
        // Copies go where every tool attachment goes, so the startup sweep covers them too.
        self.native_tools.push(Arc::new(
            tool.send_file_tool(kanon_core::DEFAULT_ATTACHMENT_DIR),
        ));
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
        self.try_build().expect("invalid API runtime configuration")
    }

    /// Builds the gateway while reporting invalid runtime configuration to the composition root.
    pub fn try_build(self) -> Result<ApiState, String> {
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
        node_settings.validate()?;
        #[cfg(feature = "dsh")]
        let dsh = factory.prepare_dsh(node_settings.dsh.as_ref())?;
        factory.set_backend(
            &node_settings.default_agent,
            #[cfg(feature = "dsh")]
            dsh,
        )?;
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
        let event_policy = Arc::new(EventPolicyStore::new(node_settings.event_policy));
        // A registered Bash tool already holds the stores it enforces; the API must publish into
        // those same stores, or a console edit would never reach the execution gate.
        let (command_policy, bash_policy) = match &self.bash_tool {
            Some(tool) => (tool.command_policy().clone(), tool.policy().clone()),
            None => Default::default(),
        };
        command_policy.set(node_settings.command_policy.clone());
        bash_policy.set(node_settings.bash_policy.clone());

        let instances = self.instances.unwrap_or_default();
        let plugin_state = self.plugin_state.unwrap_or_else(|| {
            self.mcp
                .as_ref()
                .map(|pool| pool.toggle_store().clone())
                .unwrap_or_default()
        });
        let mcp = self
            .mcp
            .unwrap_or_else(|| Arc::new(McpPool::new(plugin_state.clone())));
        // The API, pipeline and retained MCP handles must enforce the same enablement source.
        assert!(
            Arc::ptr_eq(mcp.toggle_store(), &plugin_state),
            "MCP pool and API state must share the same ToggleStore"
        );
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

        Ok(ApiState {
            pipeline: None,
            inner: Arc::new(ApiStateInner {
                startup: self.startup,
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
                event_policy,
                command_policy,
                bash_policy,
                bash_tool: self.bash_tool,
                node_settings: Arc::new(RwLock::new(node_settings)),
                milky: self.milky,
                onebot: self.onebot,
                qqofficial: self.qqofficial,
                observability,
                ingress: self.ingress,
                plugins_dir,
                // Empty until the first scan: the composition root runs it before the gateway
                // starts serving, so no request ever reads the unscanned state.
                plugins_on_disk: RwLock::new(Vec::new()),
            }),
        })
    }
}
