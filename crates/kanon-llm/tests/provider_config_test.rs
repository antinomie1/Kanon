//! Contract tests for the shared model-provider factory and the hot-swappable agent slot.

use std::sync::Arc;

use async_trait::async_trait;
use kanon_llm::{
    AgentSlot, BuiltinAgent, ChatRequest, ChatResponse, GatewayError, LlmProvider,
    SUPPORTED_PROTOCOLS, build_provider,
};

/// Minimal provider stub used to observe slot semantics.
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

fn stub_agent(model: &str) -> Arc<BuiltinAgent> {
    Arc::new(
        BuiltinAgent::builder("slot-test", Arc::new(StubProvider))
            .model(model)
            .build(),
    )
}

#[test]
fn build_provider_accepts_every_documented_protocol() {
    for protocol in SUPPORTED_PROTOCOLS {
        // Construction is pure dispatch: no network access happens until a completion is sent,
        // so an unreachable host is fine here and keeps the test hermetic.
        build_provider(protocol, "https://example.invalid/v1", None, "m")
            .unwrap_or_else(|err| panic!("protocol '{protocol}' must be supported: {err}"));
    }

    // The legacy alias must keep working so existing deployments do not break on upgrade.
    build_provider("openai_chat", "https://example.invalid/v1", None, "m")
        .expect("openai_chat alias must stay supported");
}

#[test]
fn only_the_documented_endpoint_automatically_enables_reasoning_replay() {
    use kanon_llm::OpenAiChatProvider;
    for url in [
        "https://api.deepseek.com",
        "https://api.deepseek.com/v1/",
        "https://api.deepseek.com/v1/chat/completions",
    ] {
        let provider = OpenAiChatProvider::new(url, None, "any-model");
        assert!(provider.replays_reasoning_content(), "{url}");
        assert!(
            !provider
                .with_reasoning_content(false)
                .replays_reasoning_content()
        );
    }
    for url in [
        "https://api.openai.com/v1",
        "http://127.0.0.1:1234/v1",
        "https://api.deepseek.com.example.invalid/v1",
        "https://example.invalid/api.deepseek.com",
        "https://api.deepseek.com@example.invalid/v1",
        "http://api.deepseek.com/v1",
        "https://api.deepseek.com:8443/v1",
    ] {
        let provider = OpenAiChatProvider::new(url, None, "deepseek-flash");
        assert!(!provider.replays_reasoning_content(), "{url}");
        assert!(
            provider
                .with_reasoning_content(true)
                .replays_reasoning_content()
        );
    }
}

#[test]
fn build_provider_rejects_unknown_protocol_and_blank_base_url() {
    // `.err()` rather than `.expect_err()`: the success type is a trait object without `Debug`.
    let err = build_provider("grpc-ish", "https://example.invalid/v1", None, "m")
        .err()
        .expect("unknown protocol must be rejected");
    assert!(
        err.contains("Unsupported protocol"),
        "unexpected error: {err}"
    );

    let err = build_provider("openai", "   ", None, "m")
        .err()
        .expect("blank base URL must be rejected");
    assert!(err.contains("base URL"), "unexpected error: {err}");
}

#[test]
fn agent_slot_starts_empty_and_follows_swaps() {
    let slot = AgentSlot::new();
    assert!(!slot.is_configured());
    assert!(slot.current().is_none());

    slot.set(Some(stub_agent("first")));
    assert!(slot.is_configured());
    assert_eq!(
        slot.current()
            .expect("agent installed")
            .config()
            .default_model,
        "first"
    );

    // A later provider replaces the previous one outright: there is exactly one active agent.
    slot.set(Some(stub_agent("second")));
    assert_eq!(
        slot.current()
            .expect("agent replaced")
            .config()
            .default_model,
        "second"
    );

    slot.set(None);
    assert!(!slot.is_configured());
    assert!(slot.current().is_none());
}

#[test]
fn agent_slot_with_agent_is_configured_immediately() {
    let slot = AgentSlot::with_agent(stub_agent("seeded"));
    assert!(slot.is_configured());
    assert_eq!(
        slot.current().expect("seeded agent").config().default_model,
        "seeded"
    );
}

#[test]
fn old_provider_settings_default_to_replay_without_changing_explicit_preferences() {
    let old = serde_json::json!({"name":"fixture", "protocol":"openai", "base_url":"https://example.invalid/v1"});
    let mut entry: kanon_llm::ProviderEntry = serde_json::from_value(old).unwrap();
    assert!(entry.replay_reasoning);
    entry.replay_reasoning = false;
    let loaded: kanon_llm::ProviderEntry =
        serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    assert!(!loaded.replay_reasoning);
}
