//! Tests for the reply-policy gate and the built-in `/model` command.
//!
//! These lock in the operator-visible contract: a group/channel conversation is answered only when
//! the instance's policy allows it, a private conversation is always answered, and `/model` lists
//! and persists the model of the instance that issued it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    META_BOT_MENTIONED, META_CONVERSATION_KIND, ReplyMode, ReplyPolicy, ReplyPolicyStore,
};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{Memory, SlidingWindowMemory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{
    Agent, AgentFactory, AgentSlot, GatewayError, LlmProvider, ModelSpec, PersonaRegistry,
    ProviderEntry, ProviderRuntime, SessionManager,
};
use kanon_proto::prost_types;
use kanon_proto::v1::PipelineEventRequest;

/// Provider that answers every turn and counts how often it was asked.
struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LlmProvider for CountingProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ChatResponse {
            content: Some("模型回复".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Builds a pipeline over an in-memory instance catalog and a counting provider.
async fn harness(
    registry: Arc<InstanceRegistry>,
    node_policy: ReplyPolicy,
) -> (Arc<PipelineEngine>, Arc<AtomicUsize>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let calls = Arc::new(AtomicUsize::new(0));
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(20));
    let agent = Arc::new(
        Agent::builder(
            "policy-test",
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
        )
        .memory(memory)
        .model("test-model")
        .build(),
    );

    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_instances(registry)
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(node_policy))),
    );

    (engine, calls)
}

/// Builds a pipeline whose agent factory knows a two-entry model catalog.
///
/// The endpoints point at a closed port: these tests never reach the model, they only observe the
/// routing and persistence decisions made before any request would be sent.
fn factory_harness(registry: Arc<InstanceRegistry>) -> Arc<PipelineEngine> {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let slot = Arc::new(AgentSlot::new());
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(20));
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let personas = Arc::new(PersonaRegistry::default());
    let factory = Arc::new(AgentFactory::new(
        "model-test",
        slot,
        memory,
        sessions,
        personas,
        Vec::new(),
        Vec::new(),
    ));

    factory
        .configure(
            "model-test",
            ProviderRuntime {
                providers: vec![ProviderEntry::new(
                    "local",
                    "openai",
                    "http://127.0.0.1:9/v1",
                )],
                default_provider: Some("local".to_string()),
                default_model: Some("local/test-model".to_string()),
                models: vec![
                    ModelSpec::new("local", "test-model"),
                    ModelSpec::new("local", "alt-model"),
                ],
            },
        )
        .expect("factory configured");

    Arc::new(
        PipelineEngine::new(supervisor)
            .with_agent_factory(factory)
            .with_instances(registry),
    )
}

/// Builds metadata describing the conversation kind and whether the bot was addressed.
fn metadata(kind: &str, mentioned: bool) -> prost_types::Struct {
    let string = |value: &str| prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue(value.to_string())),
    };
    let boolean = |value: bool| prost_types::Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    };

    prost_types::Struct {
        fields: [
            (META_CONVERSATION_KIND.to_string(), string(kind)),
            (META_BOT_MENTIONED.to_string(), boolean(mentioned)),
        ]
        .into_iter()
        .collect(),
    }
}

/// Inbound event fixture.
fn event(event_id: &str, text: &str, kind: &str, mentioned: bool) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: event_id.to_string(),
        platform: "policy".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "user:1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: Some(metadata(kind, mentioned)),
    }
}

/// Creates an enabled instance claiming the fixture platform.
async fn instance(registry: &InstanceRegistry, policy: Option<ReplyPolicy>) -> String {
    registry
        .create(InstanceDraft {
            name: "Policy Bot".to_string(),
            enabled: true,
            adapters: vec!["policy".to_string()],
            persona_id: None,
            system_prompt: None,
            model: None,
            reply_policy: policy,
            plugins: Default::default(),
            skills: Default::default(),
            mcp: Default::default(),
        })
        .await
        .expect("create instance")
        .id
}

#[tokio::test]
async fn mention_only_suppresses_an_unaddressed_group_message() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(ReplyPolicy::new(ReplyMode::Mention))).await;
    let (engine, calls) = harness(registry, ReplyPolicy::default()).await;

    let result = engine
        .process_event(event("e1", "随便聊聊", "group", false))
        .await;

    assert!(
        matches!(result, PipelineResult::ReplySuppressed { .. }),
        "unexpected result: {result:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the model must not be called"
    );
}

#[tokio::test]
async fn mention_only_answers_an_addressed_group_message() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(ReplyPolicy::new(ReplyMode::Mention))).await;
    let (engine, calls) = harness(registry, ReplyPolicy::default()).await;

    let result = engine
        .process_event(event("e2", "@bot 你好", "group", true))
        .await;

    assert!(
        matches!(result, PipelineResult::LlmReplied { .. }),
        "unexpected result: {result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_private_conversation_is_answered_even_when_the_policy_is_mention_only() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(ReplyPolicy::new(ReplyMode::Mention))).await;
    let (engine, calls) = harness(registry, ReplyPolicy::default()).await;

    let result = engine
        .process_event(event("e3", "你好", "private", false))
        .await;

    assert!(
        matches!(result, PipelineResult::LlmReplied { .. }),
        "unexpected result: {result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn never_suppresses_groups_but_never_a_private_conversation() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(ReplyPolicy::new(ReplyMode::Never))).await;
    let (engine, _calls) = harness(registry, ReplyPolicy::default()).await;

    assert!(matches!(
        engine
            .process_event(event("e4", "@bot hi", "group", true))
            .await,
        PipelineResult::ReplySuppressed { .. }
    ));
    assert!(matches!(
        engine
            .process_event(event("e5", "hi", "private", false))
            .await,
        PipelineResult::LlmReplied { .. }
    ));
}

#[tokio::test]
async fn probability_mode_is_deterministic_at_the_extremes() {
    let always = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: 1.0,
    };
    let never = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: 0.0,
    };

    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(always)).await;
    let (engine, _calls) = harness(registry.clone(), ReplyPolicy::default()).await;
    assert!(matches!(
        engine
            .process_event(event("e6", "hello", "group", false))
            .await,
        PipelineResult::LlmReplied { .. }
    ));

    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(never)).await;
    let (engine, _calls) = harness(registry, ReplyPolicy::default()).await;
    assert!(matches!(
        engine
            .process_event(event("e7", "hello", "group", false))
            .await,
        PipelineResult::ReplySuppressed { .. }
    ));
}

#[tokio::test]
async fn the_instance_policy_overrides_the_node_policy() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, Some(ReplyPolicy::new(ReplyMode::Always))).await;
    let (engine, _calls) = harness(registry, ReplyPolicy::new(ReplyMode::Never)).await;

    assert!(matches!(
        engine
            .process_event(event("e8", "hello", "group", false))
            .await,
        PipelineResult::LlmReplied { .. }
    ));
}

#[tokio::test]
async fn the_node_policy_governs_an_instance_without_an_override() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, None).await;
    let (engine, _calls) = harness(registry, ReplyPolicy::new(ReplyMode::Never)).await;

    assert!(matches!(
        engine
            .process_event(event("e9", "hello", "group", true))
            .await,
        PipelineResult::ReplySuppressed { .. }
    ));
}

#[tokio::test]
async fn model_command_lists_the_catalog() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    let id = instance(&registry, None).await;
    let engine = factory_harness(registry.clone());

    let result = engine
        .process_event(event("m1", "/model", "private", false))
        .await;

    match result {
        PipelineResult::ModelListed { count, replies, .. } => {
            assert_eq!(count, 2, "both catalog models must be listed");
            let text = reply_text(&replies);
            assert!(text.contains("local/test-model"), "listing: {text}");
            assert!(text.contains("local/alt-model"), "listing: {text}");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Listing must not change the instance.
    assert!(registry.get(&id).await.expect("instance").model.is_none());
}

#[tokio::test]
async fn model_command_switches_and_persists_the_instance_model() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    let id = instance(&registry, None).await;
    let engine = factory_harness(registry.clone());

    // The catalog lists models ordered by canonical reference, so index 1 is `local/alt-model`.
    let result = engine
        .process_event(event("m2", "/model 1", "private", false))
        .await;

    match result {
        PipelineResult::ModelSelected { model, replies, .. } => {
            assert_eq!(model, "local/alt-model");
            assert!(reply_text(&replies).contains("local/alt-model"));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    assert_eq!(
        registry.get(&id).await.expect("instance").model.as_deref(),
        Some("local/alt-model"),
        "the choice must survive a restart"
    );
}

#[tokio::test]
async fn model_command_with_an_out_of_range_index_lists_instead_of_switching() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    let id = instance(&registry, None).await;
    let engine = factory_harness(registry.clone());

    let result = engine
        .process_event(event("m3", "/model 99", "private", false))
        .await;

    assert!(
        matches!(result, PipelineResult::ModelListed { .. }),
        "unexpected result: {result:?}"
    );
    assert!(registry.get(&id).await.expect("instance").model.is_none());
}

/// Renders the text of the first reply segment.
fn reply_text(replies: &[kanon_proto::v1::MessageSegment]) -> String {
    replies
        .iter()
        .find_map(|segment| match segment.segment.as_ref() {
            Some(kanon_proto::v1::message_segment::Segment::Text(text)) => {
                Some(text.content.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}
