//! Construction of the built-in agent and its shared runtime handles.

use super::*;

/// Fluent builder for constructing customizable [`BuiltinAgent`] instances.
pub struct AgentBuilder {
    name: String,
    system_prompt: Option<String>,
    provider: Arc<dyn LlmProvider>,
    memory: Option<Arc<dyn Memory>>,
    session_manager: Option<Arc<crate::session::SessionManager>>,
    persona_registry: Option<Arc<crate::prompt::PersonaRegistry>>,
    tools: Vec<Arc<dyn AgentTool>>,
    hooks: Vec<Arc<dyn AgentHook>>,
    config: AgentConfig,
}

impl AgentBuilder {
    /// Initiates a new builder with the agent name and backend model provider.
    pub fn new(name: impl Into<String>, provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            name: name.into(),
            system_prompt: None,
            provider,
            memory: None,
            session_manager: None,
            persona_registry: None,
            tools: Vec::new(),
            hooks: Vec::new(),
            config: AgentConfig::default(),
        }
    }

    /// Sets the base persona / system prompt for the agent.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Injects a custom memory backend (e.g. SQLite, Redis, or custom plugin memory).
    pub fn memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Injects a session manager for metadata, multi-scope keys, and turn lifecycle tracking.
    pub fn session_manager(mut self, manager: Arc<crate::session::SessionManager>) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Injects a persona registry for dynamic persona resolution.
    pub fn persona_registry(mut self, registry: Arc<crate::prompt::PersonaRegistry>) -> Self {
        self.persona_registry = Some(registry);
        self
    }

    /// Registers a native in-process tool on this agent.
    pub fn tool(mut self, tool: impl AgentTool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Registers a native in-process tool wrapped in an [`Arc`].
    pub fn tool_arc(mut self, tool: Arc<dyn AgentTool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Registers a lifecycle interception hook on this agent.
    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.hooks.push(Arc::new(hook));
        self
    }

    /// Registers a lifecycle hook wrapped in an [`Arc`].
    pub fn hook_arc(mut self, hook: Arc<dyn AgentHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Sets default model tag.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.default_model = model.into();
        self
    }

    /// Names the provider endpoint that serves the default model.
    pub fn provider(mut self, provider: Option<String>) -> Self {
        self.config.provider = provider;
        self
    }

    /// Records the default model's context window, when known.
    pub fn context_length(mut self, context_length: Option<u32>) -> Self {
        self.config.context_length = context_length;
        self
    }

    /// Sets maximum reasoning iterations (default 5).
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.max_iterations = max;
        self
    }

    /// Allows tool schemas and execution only when the selected model supports tool calling.
    pub fn tool_calling(mut self, enabled: bool) -> Self {
        self.config.tool_calling = enabled;
        self
    }

    /// Sets sampling temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config.temperature = Some(temp);
        self
    }

    /// Sets max tokens.
    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.config.max_tokens = Some(tokens);
        self
    }

    /// Configures whether to stop immediately if a tool fails.
    pub fn stop_on_tool_failure(mut self, stop: bool) -> Self {
        self.config.stop_on_tool_failure = stop;
        self
    }

    /// Sets when long conversations are compacted into a summary; `None` never compacts.
    pub fn compaction(mut self, policy: Option<CompactionPolicy>) -> Self {
        self.config.compaction = policy;
        self
    }

    /// Builds the configured [`BuiltinAgent`].
    pub fn build(self) -> BuiltinAgent {
        let memory = self
            .memory
            .or_else(|| self.session_manager.as_ref().map(|sm| sm.memory().clone()))
            .unwrap_or_else(|| Arc::new(InMemory::new()));

        let standalone_writers = self
            .session_manager
            .is_none()
            .then(|| Arc::new(SessionWriters::default()));

        BuiltinAgent {
            name: self.name,
            system_prompt: self.system_prompt,
            provider: self.provider,
            memory,
            session_manager: self.session_manager,
            persona_registry: self.persona_registry,
            tools: self.tools,
            hooks: self.hooks,
            config: self.config,
            standalone_writers,
            compacting: Arc::new(DashSet::new()),
        }
    }
}
