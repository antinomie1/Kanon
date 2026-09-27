//! Node-wide agent construction, shared by every consumer that needs a model runtime.
//!
//! # Why a factory instead of a single captured agent
//! One node runs several bot instances, and each instance may pick its own model while sharing
//! the node's conversation memory, session manager, persona registry and trace bus. Rebuilding an
//! [`Agent`] for such an override is cheap (it clones `Arc`s) but must not diverge from the node's
//! constructor, so every agent — the node default and every per-instance override — is built here.
//!
//! # Why models are routed through a provider directory
//! A model is addressed as `<provider>/<model-id>`. Resolving that reference is the factory's job:
//! it asks the [`ProviderRegistry`] which endpoint serves the provider, looks the model up in the
//! [`ModelCatalog`] for its context window and modalities, and only then builds the agent. The
//! node's default agent lives in an [`AgentSlot`] because it is replaced whenever the operator
//! changes the default provider; per-model agents are cached under their canonical
//! `<provider>/<model-id>` name and dropped wholesale whenever the directory changes, so an
//! override can never outlive the credential it was built for.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::agent::{Agent, AgentConfig, AgentHook, AgentTool};
use crate::gateway::LlmProvider;
use crate::memory::Memory;
use crate::model::{ModelCatalog, ModelRef, ModelSpec};
use crate::prompt::PersonaRegistry;
use crate::provider::{ProviderEntry, ProviderRegistry};
use crate::session::SessionManager;
use crate::slot::AgentSlot;

/// Everything the node needs to route a model reference to an endpoint.
///
/// This is the single description the management layer persists and applies: the endpoints'
/// definitions, which one is the default, which model the default agent uses, and the per-model
/// settings catalog.
#[derive(Debug, Clone, Default)]
pub struct ProviderRuntime {
    /// Configured endpoints.
    pub providers: Vec<ProviderEntry>,
    /// Name of the endpoint used when a model reference carries no provider.
    pub default_provider: Option<String>,
    /// Canonical `<provider>/<model-id>` the node answers with by default.
    pub default_model: Option<String>,
    /// Per-model settings.
    pub models: Vec<ModelSpec>,
}

/// A directly installed provider, used by embedded deployments and tests that hand the node a
/// client without a persisted directory.
#[derive(Clone)]
struct DirectProvider {
    /// Name the direct provider answers to when a reference names it.
    name: String,
    /// The client itself.
    provider: Arc<dyn LlmProvider>,
}

/// Builds every agent the node runs, so they all share one memory, session manager, persona
/// registry and trace bus.
pub struct AgentFactory {
    /// Name given to the node's default agent.
    name: String,
    /// The node's default agent, replaced when the operator changes the provider directory.
    slot: Arc<AgentSlot>,
    /// Conversation memory shared by every agent.
    memory: Arc<dyn Memory>,
    /// Session lifecycle manager shared by every agent.
    sessions: Arc<SessionManager>,
    /// Persona catalog shared by every agent.
    personas: Arc<PersonaRegistry>,
    /// Lifecycle hooks shared by every agent: trace publishing, skill catalogs, ...
    hooks: Vec<Arc<dyn AgentHook>>,
    /// Native in-process tools available to every agent (e.g. `read_skill`).
    tools: Vec<Arc<dyn AgentTool>>,
    /// Named provider endpoints and the `provider/model` routing they enable.
    providers: Arc<ProviderRegistry>,
    /// Per-model settings shared with the pipeline and the console.
    models: Arc<ModelCatalog>,
    /// Canonical reference of the node's default model, when configured.
    default_model: RwLock<Option<String>>,
    /// Provider installed directly rather than through the directory, when any.
    direct: RwLock<Option<DirectProvider>>,
    /// Agents built for per-model overrides, keyed by canonical model reference.
    ///
    /// Bounded by the number of distinct models operators configure, and dropped wholesale when
    /// the directory changes so an override can never outlive the provider it was built for.
    overrides: RwLock<HashMap<String, Arc<Agent>>>,
}

impl std::fmt::Debug for AgentFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentFactory")
            .field("name", &self.name)
            .field("slot", &self.slot)
            .field("providers", &self.providers)
            .finish_non_exhaustive()
    }
}

impl AgentFactory {
    /// Creates a factory bound to one node's shared runtime parts.
    pub fn new(
        name: impl Into<String>,
        slot: Arc<AgentSlot>,
        memory: Arc<dyn Memory>,
        sessions: Arc<SessionManager>,
        personas: Arc<PersonaRegistry>,
        hooks: Vec<Arc<dyn AgentHook>>,
        tools: Vec<Arc<dyn AgentTool>>,
    ) -> Self {
        Self {
            name: name.into(),
            slot,
            memory,
            sessions,
            personas,
            hooks,
            tools,
            providers: Arc::new(ProviderRegistry::new()),
            models: Arc::new(ModelCatalog::new()),
            default_model: RwLock::new(None),
            direct: RwLock::new(None),
            overrides: RwLock::new(HashMap::new()),
        }
    }

    /// The slot holding the node's default agent.
    pub fn slot(&self) -> &Arc<AgentSlot> {
        &self.slot
    }

    /// Named provider directory shared with the console and the pipeline.
    pub fn providers(&self) -> &Arc<ProviderRegistry> {
        &self.providers
    }

    /// Per-model settings catalog shared with the pipeline and the console.
    pub fn models(&self) -> &Arc<ModelCatalog> {
        &self.models
    }

    /// Native in-process tools shared by every agent (e.g. `read_skill`).
    ///
    /// Exposed so the management gateway can list every tool the node offers, including the ones
    /// that never come from a plugin host.
    pub fn native_tools(&self) -> &[Arc<dyn AgentTool>] {
        &self.tools
    }

    /// Conversation memory shared by every agent.
    pub fn memory(&self) -> &Arc<dyn Memory> {
        &self.memory
    }

    /// Session manager shared by every agent.
    pub fn sessions(&self) -> &Arc<SessionManager> {
        &self.sessions
    }

    /// Persona catalog shared by every agent.
    pub fn personas(&self) -> &Arc<PersonaRegistry> {
        &self.personas
    }

    /// Canonical reference of the node's default model, when configured.
    pub fn default_model(&self) -> Option<String> {
        self.default_model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Installs a provider directory and rebuilds the node's default agent.
    ///
    /// Order is *validate → publish → build*: the registry rejects a malformed directory before any
    /// state changes, so a failed configuration leaves the node serving its previous provider.
    pub fn configure(
        &self,
        name: impl Into<String>,
        runtime: ProviderRuntime,
    ) -> Result<(), String> {
        self.providers
            .replace(runtime.providers, runtime.default_provider)?;
        self.models.replace(runtime.models);
        *self
            .direct
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let default_model = runtime
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);
        *self
            .default_model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = default_model.clone();

        self.overrides
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        let Some(default_model) = default_model else {
            // No default model: the node has no conversational runtime, which is the same
            // reportable state as no provider at all.
            self.slot.set(None);
            return Ok(());
        };

        let reference = ModelRef::parse(&default_model);
        let resolved = self.providers.resolve(&reference)?;
        let spec = self.models.settings_for(&ModelRef::new(
            resolved.provider_name.clone(),
            resolved.model.clone(),
        ));
        let config = self.agent_config_for(&resolved.provider_name, &resolved.model, &spec);
        let agent = Arc::new(self.build_agent_named(name.into(), resolved.provider, config));
        self.slot.set(Some(agent));
        Ok(())
    }

    /// Installs a provider client directly and returns the resulting default agent.
    ///
    /// Used by embedded deployments and tests that hold a client without a persisted directory; the
    /// directory stays authoritative whenever [`AgentFactory::configure`] has been called.
    pub fn install(
        &self,
        name: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
        config: AgentConfig,
    ) -> Arc<Agent> {
        *self
            .direct
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(DirectProvider {
            name: config
                .provider
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            provider: provider.clone(),
        });
        *self
            .default_model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(config.model_ref());

        let agent = Arc::new(self.build_agent_named(name.into(), provider, config));
        self.overrides
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.slot.set(Some(agent.clone()));
        agent
    }

    /// Clears the node's provider together with every derived override.
    pub fn clear(&self) {
        self.overrides
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        *self
            .direct
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *self
            .default_model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let _ = self.providers.replace(Vec::new(), None);
        self.models.replace(Vec::new());
        self.slot.set(None);
    }

    /// Agent for the node's configured provider.
    pub fn node_agent(&self) -> Option<Arc<Agent>> {
        self.slot.current()
    }

    /// Agent that should serve a conversation using an optional model reference.
    ///
    /// `None`, a blank reference, or the node's own default model all resolve to the default agent;
    /// anything else produces (and caches) an agent that shares everything except the endpoint,
    /// model id and model-specific tuning.
    pub fn agent_for_model(&self, model: Option<&str>) -> Option<Arc<Agent>> {
        match model.map(str::trim).filter(|model| !model.is_empty()) {
            Some(requested) => self.agent_for_reference(&ModelRef::parse(requested)),
            // No override: the node's own agent serves the conversation.
            None => self.node_agent(),
        }
    }

    /// Agent that should serve a conversation for one parsed model reference.
    pub fn agent_for_reference(&self, reference: &ModelRef) -> Option<Arc<Agent>> {
        let default_agent = self.slot.current()?;
        if reference.is_empty() {
            return Some(default_agent);
        }

        // A bare reference naming the node's own model is the node agent, even when a provider
        // directory is configured: rebuilding it would drop the node's own tuning for no reason.
        if reference.provider().is_none()
            && reference.model() == default_agent.config().default_model
        {
            return Some(default_agent);
        }

        let resolved = match self.providers.resolve(reference) {
            Ok(resolved) => resolved,
            Err(registry_error) => {
                // No directory entry matched: fall back to a directly installed provider, which is
                // how an embedded node (and the tests) hand over one client.
                let direct = self
                    .direct
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()?;
                let upstream = match reference.provider() {
                    // A prefix the direct provider does not own is part of an aggregator model id.
                    Some(prefix) if prefix != direct.name => reference.canonical(),
                    _ => reference.model().to_string(),
                };
                tracing::debug!(
                    requested = %reference.canonical(),
                    provider = %direct.name,
                    error = %registry_error,
                    "Model reference resolved against the directly installed provider"
                );
                crate::provider::ResolvedProvider {
                    provider_name: direct.name,
                    provider: direct.provider,
                    model: upstream,
                }
            }
        };

        let canonical = format!("{}/{}", resolved.provider_name, resolved.model);
        if canonical == default_agent.config().model_ref() {
            return Some(default_agent);
        }

        if let Some(cached) = self
            .overrides
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&canonical)
        {
            return Some(cached.clone());
        }

        let model_reference = ModelRef::new(resolved.provider_name.clone(), resolved.model.clone());
        let spec = self.models.settings_for(&model_reference);
        let mut config = self.agent_config_for(&resolved.provider_name, &resolved.model, &spec);
        // Preserve the base tuning the node's default agent was given, so a per-instance override
        // only changes what the model itself dictates.
        if spec.temperature.is_none() {
            config.temperature = default_agent.config().temperature;
        }
        if spec.max_output_tokens.is_none() {
            config.max_tokens = default_agent.config().max_tokens;
        }

        let agent = Arc::new(self.build_agent(resolved.provider, config));
        self.overrides
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(canonical, agent.clone());
        Some(agent)
    }

    /// Builds an agent for an explicitly supplied provider, sharing this node's runtime parts.
    ///
    /// Used by the console sandbox, which may run a one-off request against credentials the
    /// operator typed without persisting them; such an agent must still see the same memory,
    /// sessions, personas and trace bus as the node's own.
    pub fn build_with(&self, provider: Arc<dyn LlmProvider>, config: AgentConfig) -> Arc<Agent> {
        Arc::new(self.build_agent(provider, config))
    }

    /// Derives the agent configuration for one resolved model.
    ///
    /// Model-specific settings win over endpoint-level defaults, which in turn win over the
    /// built-in agent defaults. `default_model` always carries the *upstream* id (the provider
    /// prefix stripped), because that is what the wire request must contain.
    fn agent_config_for(
        &self,
        provider_name: &str,
        upstream_model: &str,
        spec: &ModelSpec,
    ) -> AgentConfig {
        let endpoint = self.providers.get(provider_name);
        let context_length = spec.context_length;
        let temperature = spec
            .temperature
            .or_else(|| endpoint.as_ref().and_then(|entry| entry.temperature));
        let max_tokens = spec
            .max_output_tokens
            .or_else(|| endpoint.as_ref().and_then(|entry| entry.max_tokens));

        AgentConfig {
            provider: Some(provider_name.to_string()),
            default_model: upstream_model.to_string(),
            context_length,
            temperature,
            max_tokens,
            ..AgentConfig::default()
        }
    }

    /// Builds one agent sharing this factory's memory, sessions, personas and hooks.
    fn build_agent(&self, provider: Arc<dyn LlmProvider>, config: AgentConfig) -> Agent {
        self.build_agent_named(self.name.clone(), provider, config)
    }

    /// Builds one agent under an explicit name, sharing this factory's runtime parts.
    fn build_agent_named(
        &self,
        name: String,
        provider: Arc<dyn LlmProvider>,
        config: AgentConfig,
    ) -> Agent {
        // The builder exposes fluent setters rather than a whole-config setter, so optional
        // sampling knobs are applied only when configured.
        let mut builder = Agent::builder(name, provider)
            .memory(self.memory.clone())
            .session_manager(self.sessions.clone())
            .persona_registry(self.personas.clone())
            .model(config.default_model.clone())
            .provider(config.provider.clone())
            .context_length(config.context_length)
            .max_iterations(config.max_iterations)
            .stop_on_tool_failure(config.stop_on_tool_failure);

        for hook in &self.hooks {
            builder = builder.hook_arc(hook.clone());
        }
        for tool in &self.tools {
            builder = builder.tool_arc(tool.clone());
        }
        if let Some(temperature) = config.temperature {
            builder = builder.temperature(temperature);
        }
        if let Some(max_tokens) = config.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }

        builder.build()
    }
}
