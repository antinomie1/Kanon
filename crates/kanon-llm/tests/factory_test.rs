//! Tests for the node agent factory: default agent, per-instance model overrides and their cache.

use std::sync::Arc;

use async_trait::async_trait;
use kanon_llm::{
    AgentConfig, AgentFactory, AgentSlot, ChatRequest, ChatResponse, GatewayError, InMemory,
    LlmProvider, Memory, PersonaRegistry, ProviderEntry, ProviderRuntime, SessionManager,
};

/// Provider stub; identity matters more than behaviour in these tests.
struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            reasoning_content: None,
            content: Some("stub".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Builds a factory over fresh runtime parts, returning them for sharing assertions.
fn factory() -> (
    AgentFactory,
    Arc<dyn LlmProvider>,
    Arc<dyn Memory>,
    Arc<SessionManager>,
    Arc<PersonaRegistry>,
) {
    let slot = Arc::new(AgentSlot::new());
    let provider: Arc<dyn LlmProvider> = Arc::new(StubProvider);
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let personas = Arc::new(PersonaRegistry::default());

    let factory = AgentFactory::new(
        "node",
        slot,
        memory.clone(),
        sessions.clone(),
        personas.clone(),
        Vec::new(),
        Vec::new(),
    );

    (factory, provider, memory, sessions, personas)
}

fn config(model: &str) -> AgentConfig {
    AgentConfig {
        default_model: model.to_string(),
        ..AgentConfig::default()
    }
}

#[test]
fn without_a_provider_every_lookup_is_empty() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();

    assert!(factory.node_agent().is_none());
    assert!(factory.agent_for_model(None).is_none());
    assert!(factory.agent_for_model(Some("any-model")).is_none());
}

#[test]
fn blank_and_default_models_resolve_to_the_node_agent() {
    let (factory, provider, _memory, _sessions, _personas) = factory();
    let node = factory.install("node", provider, config("default-model"));

    for requested in [None, Some(""), Some("   "), Some("default-model")] {
        let resolved = factory
            .agent_for_model(requested)
            .unwrap_or_else(|| panic!("{requested:?} must resolve to the node agent"));
        assert!(
            Arc::ptr_eq(&resolved, &node),
            "{requested:?} must reuse the node agent rather than rebuild it"
        );
    }
}

#[test]
fn a_different_model_yields_a_shared_runtime_agent_and_is_cached() {
    let (factory, provider, memory, _sessions, _personas) = factory();
    factory.install("node", provider.clone(), config("default-model"));

    let override_agent = factory
        .agent_for_model(Some("other-model"))
        .expect("override agent");
    assert_eq!(override_agent.config().default_model, "other-model");
    // Everything except the model tag is shared with the node's runtime.
    assert!(Arc::ptr_eq(override_agent.memory(), &memory));
    assert!(Arc::ptr_eq(override_agent.provider(), &provider));

    // Repeated lookups reuse the cached agent instead of rebuilding one per message.
    let again = factory
        .agent_for_model(Some("other-model"))
        .expect("cached override agent");
    assert!(Arc::ptr_eq(&override_agent, &again));
}

#[test]
fn changing_the_provider_drops_overrides_built_for_the_previous_one() {
    let (factory, first_provider, _memory, _sessions, _personas) = factory();
    factory.install("node", first_provider, config("default-model"));
    let stale = factory
        .agent_for_model(Some("other-model"))
        .expect("override agent");

    let second_provider: Arc<dyn LlmProvider> = Arc::new(StubProvider);
    factory.install("node", second_provider.clone(), config("default-model"));

    let rebuilt = factory
        .agent_for_model(Some("other-model"))
        .expect("override agent after provider change");
    assert!(
        !Arc::ptr_eq(&stale, &rebuilt),
        "an override must not survive the provider it was built for"
    );
    assert!(Arc::ptr_eq(rebuilt.provider(), &second_provider));
}

#[test]
fn clearing_the_provider_removes_the_node_and_its_overrides() {
    let (factory, provider, _memory, _sessions, _personas) = factory();
    factory.install("node", provider, config("default-model"));
    assert!(factory.agent_for_model(Some("other-model")).is_some());

    factory.clear();

    assert!(factory.node_agent().is_none());
    assert!(factory.agent_for_model(Some("other-model")).is_none());
}

#[test]
fn build_with_keeps_the_node_runtime_parts() {
    let (factory, _provider, memory, sessions, _personas) = factory();
    factory.install("node", Arc::new(StubProvider), config("default-model"));

    let ephemeral_provider: Arc<dyn LlmProvider> = Arc::new(StubProvider);
    let agent = factory.build_with(ephemeral_provider.clone(), config("sandbox-model"));

    assert_eq!(agent.config().default_model, "sandbox-model");
    assert!(Arc::ptr_eq(agent.memory(), &memory));
    assert!(Arc::ptr_eq(agent.provider(), &ephemeral_provider));
    // The sandbox agent must still see the node's sessions.
    assert!(Arc::ptr_eq(
        agent.session_manager().expect("session manager"),
        &sessions
    ));
}

/// A directory of two endpoints, neither of which is "the default": the default is a model.
fn two_providers(default_model: Option<&str>) -> ProviderRuntime {
    ProviderRuntime {
        providers: vec![
            ProviderEntry::new("alpha", "openai", "http://127.0.0.1:9/v1"),
            ProviderEntry::new("beta", "openai", "http://127.0.0.1:10/v1"),
        ],
        default_model: default_model.map(str::to_string),
        models: Vec::new(),
    }
}

#[test]
fn the_global_default_model_picks_the_node_agent_and_its_provider() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();
    factory
        .configure("node", two_providers(Some("beta/model-b")))
        .expect("configured");

    let node = factory.node_agent().expect("node agent");
    assert_eq!(node.config().default_model, "model-b");
    assert_eq!(node.config().provider.as_deref(), Some("beta"));
    assert_eq!(node.config().model_ref(), "beta/model-b");

    // A per-instance override may name any configured endpoint, not only the default's.
    let other = factory
        .agent_for_model(Some("alpha/model-a"))
        .expect("override agent");
    assert_eq!(other.config().provider.as_deref(), Some("alpha"));
    assert_eq!(other.config().default_model, "model-a");
}

#[test]
fn configuring_without_a_default_model_leaves_the_node_unable_to_answer() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();
    factory
        .configure("node", two_providers(Some("alpha/model-a")))
        .expect("configured");
    assert!(factory.node_agent().is_some());

    factory
        .configure("node", two_providers(None))
        .expect("directory without a default is valid");
    assert!(
        factory.node_agent().is_none(),
        "no default model means no conversational runtime, not a guessed one"
    );
    assert!(
        factory.agent_for_model(Some("alpha/model-a")).is_none(),
        "overrides need the node to be configured as well"
    );
}

#[test]
fn a_default_model_naming_an_unknown_provider_is_rejected_and_changes_nothing() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();
    factory
        .configure("node", two_providers(Some("alpha/model-a")))
        .expect("configured");

    let error = factory
        .configure("node", two_providers(Some("gamma/model-g")))
        .expect_err("unknown provider");
    assert!(error.contains("gamma"), "unexpected error: {error}");

    // The previous configuration keeps serving.
    let node = factory.node_agent().expect("node agent survives");
    assert_eq!(node.config().model_ref(), "alpha/model-a");
    assert_eq!(factory.providers().names(), vec!["alpha", "beta"]);
}

#[test]
fn a_reference_that_resolves_to_no_provider_yields_no_agent() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();
    factory
        .configure("node", two_providers(Some("alpha/model-a")))
        .expect("configured");

    assert!(factory.agent_for_model(Some("gamma/model-g")).is_none());
    assert!(
        factory.agent_for_model(Some("model-a")).is_none(),
        "a bare default model still requires a provider"
    );
    assert!(
        factory
            .agent_for_model(Some("model-without-prefix"))
            .is_none(),
        "a bare id is not guessed onto some endpoint"
    );
}

#[test]
fn arbitrary_model_overrides_are_bounded_and_recent_models_are_reused() {
    let (factory, provider, _memory, _sessions, _personas) = factory();
    factory.install("node", provider, config("default-model"));
    let hot = factory.agent_for_model(Some("hot-model")).unwrap();
    let mut retained = Vec::new();

    for index in 0..256 {
        let agent = factory
            .agent_for_model(Some(&format!("arbitrary-{index}")))
            .unwrap();
        retained.push(Arc::downgrade(&agent));
        let reused = factory.agent_for_model(Some("hot-model")).unwrap();
        assert!(
            Arc::ptr_eq(&hot, &reused),
            "recently used models stay cached"
        );
    }

    assert!(
        retained[0].upgrade().is_none(),
        "old model agents are released"
    );
    assert!(retained.last().unwrap().upgrade().is_some());
    assert_eq!(
        retained
            .iter()
            .filter(|agent| agent.upgrade().is_some())
            .count(),
        63
    );
    let rebuilt = factory.agent_for_model(Some("arbitrary-0")).unwrap();
    assert_eq!(rebuilt.config().default_model, "arbitrary-0");
}

/// Pauses caller-owned conversion so a concurrent lookup can observe the old configuration.
struct PausingName {
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

impl From<PausingName> for String {
    fn from(name: PausingName) -> Self {
        name.entered.send(()).unwrap();
        name.resume
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        "node".to_string()
    }
}

#[test]
fn an_override_cannot_cache_a_mixture_of_old_and_new_configuration() {
    let (factory, _provider, _memory, _sessions, _personas) = factory();
    let factory = Arc::new(factory);
    let mut old = two_providers(Some("alpha/default"));
    old.providers[0].temperature = Some(0.2);
    factory.configure("node", old).unwrap();
    let previous = factory.node_agent().unwrap();
    let mut replacement = two_providers(Some("alpha/default"));
    replacement.providers[0].temperature = Some(0.9);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();

    let configuring = {
        let factory = factory.clone();
        std::thread::spawn(move || {
            factory.configure(
                PausingName {
                    entered: entered_tx,
                    resume: resume_rx,
                },
                replacement,
            )
        })
    };
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let (looked_up_tx, looked_up_rx) = std::sync::mpsc::channel();
    let lookup = {
        let factory = factory.clone();
        std::thread::spawn(move || {
            let agent = factory.agent_for_model(Some("alpha/override")).unwrap();
            looked_up_tx.send(()).unwrap();
            agent
        })
    };

    // Caller code runs before publication. This lookup must see the complete old state;
    // the old implementation had already replaced the provider and mixed it with old tuning.
    looked_up_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let agent = lookup.join().unwrap();
    resume_tx.send(()).unwrap();
    configuring.join().unwrap().unwrap();
    assert_eq!(agent.config().temperature, Some(0.2));
    assert!(Arc::ptr_eq(agent.provider(), previous.provider()));
    let cached = factory.agent_for_model(Some("alpha/override")).unwrap();
    assert!(!Arc::ptr_eq(&agent, &cached));
    assert_eq!(cached.config().temperature, Some(0.9));
    assert!(Arc::ptr_eq(
        cached.provider(),
        factory.node_agent().unwrap().provider()
    ));
}

#[test]
fn overrides_use_their_own_endpoint_tuning_and_model_specific_settings() {
    let (factory, _, _, _, _) = factory();
    let mut runtime = two_providers(Some("alpha/default"));
    runtime.providers[0].temperature = Some(0.2);
    runtime.providers[0].max_tokens = Some(8192);
    runtime.providers[1].temperature = Some(1.3);
    runtime.providers[1].max_tokens = Some(1024);
    let mut tuned = kanon_llm::ModelSpec::new("beta", "tuned");
    tuned.temperature = Some(0.7);
    tuned.max_output_tokens = Some(512);
    tuned.capabilities.tool_calling = false;
    runtime.models.push(tuned);
    factory.configure("node", runtime.clone()).unwrap();
    let default = factory.agent_for_model(Some("beta/untuned")).unwrap();
    assert_eq!(default.config().temperature, Some(1.3));
    assert_eq!(default.config().max_tokens, Some(1024));
    let tuned = factory.agent_for_model(Some("beta/tuned")).unwrap();
    assert_eq!(tuned.config().temperature, Some(0.7));
    assert_eq!(tuned.config().max_tokens, Some(512));
    assert!(!tuned.config().tool_calling);

    runtime.providers[1].temperature = None;
    runtime.providers[1].max_tokens = None;
    factory.configure("node", runtime).unwrap();
    let unspecified = factory.agent_for_model(Some("beta/untuned")).unwrap();
    assert_eq!(unspecified.config().temperature, None);
    assert_eq!(unspecified.config().max_tokens, None);
}

#[test]
fn direct_install_replaces_the_directory_and_keeps_its_execution_policy_for_overrides() {
    let (factory, provider, _, _, _) = factory();
    let mut runtime = two_providers(Some("alpha/default"));
    runtime
        .models
        .push(kanon_llm::ModelSpec::new("alpha", "default"));
    factory.configure("node", runtime).unwrap();
    let base = AgentConfig {
        max_iterations: 2,
        stop_on_tool_failure: true,
        compaction: None,
        temperature: Some(0.8),
        max_tokens: Some(777),
        tool_calling: false,
        ..config("direct-default")
    };
    factory.install("direct", provider.clone(), base);
    assert!(factory.providers().names().is_empty());
    assert!(factory.models().is_empty());
    let override_agent = factory.agent_for_model(Some("other")).unwrap();
    assert!(Arc::ptr_eq(override_agent.provider(), &provider));
    let config = override_agent.config();
    assert_eq!(config.max_iterations, 2);
    assert!(config.stop_on_tool_failure);
    assert!(config.compaction.is_none());
    assert_eq!(config.temperature, Some(0.8));
    assert_eq!(config.max_tokens, Some(777));
    assert!(!config.tool_calling);
}
