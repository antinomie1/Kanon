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
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{
    AgentFactory, AgentSlot, GatewayError, LlmProvider, ModelSpec, PersonaRegistry, ProviderEntry,
    ProviderRuntime, SessionManager,
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
            reasoning_content: None,
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
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
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
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
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
            context_policy: None,
            plugins: Default::default(),
            skills: Default::default(),
            mcp: Default::default(),
            ..Default::default()
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
        ..Default::default()
    };
    let never = ReplyPolicy {
        mode: ReplyMode::Probability,
        probability: 0.0,
        ..Default::default()
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

#[tokio::test]
async fn a_mention_prefixed_command_is_recognised() {
    // Group platforms render a mention as leading text, so `/model` arrives as `@bot /model`. The
    // core must strip that prefix before parsing, or commands typed in a group never work.
    let registry = Arc::new(InstanceRegistry::in_memory());
    let id = instance(&registry, None).await;
    let engine = factory_harness(registry.clone());

    let result = engine
        .process_event(event("m5", "@黑猪AI /model 1", "group", true))
        .await;

    assert!(
        matches!(result, PipelineResult::ModelSelected { .. }),
        "unexpected result: {result:?}"
    );
    assert_eq!(
        registry.get(&id).await.expect("instance").model.as_deref(),
        Some("local/alt-model")
    );
}

#[tokio::test]
async fn a_mention_prefixed_new_command_rotates_the_session() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, None).await;
    let engine = factory_harness(registry);

    let result = engine
        .process_event(event("m6", "@黑猪AI /new", "group", true))
        .await;

    assert!(
        matches!(result, PipelineResult::SessionRotated { .. }),
        "unexpected result: {result:?}"
    );
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

#[tokio::test]
async fn help_lists_the_builtin_commands() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, None).await;
    let engine = factory_harness(registry);

    match engine
        .process_event(event("h1", "/help", "private", false))
        .await
    {
        PipelineResult::BuiltinReplied { command, replies } => {
            assert_eq!(command, "help");
            let text = reply_text(&replies);
            for expected in ["/new", "/model", "/help", "/info"] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn info_reports_host_time_model_and_adapter() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, None).await;
    let engine = factory_harness(registry);

    match engine
        .process_event(event("i1", "/info", "private", false))
        .await
    {
        PipelineResult::BuiltinReplied { command, replies } => {
            assert_eq!(command, "info");
            let text = reply_text(&replies);
            #[cfg(target_os = "macos")]
            assert!(text.starts_with("系统: macOS "), "{text}");
            #[cfg(not(target_os = "macos"))]
            assert!(text.contains("系统:"), "{text}");
            assert!(text.contains("时间:"), "{text}");
            assert!(text.contains("模型: local/test-model"), "{text}");
            assert!(text.contains("适配器: policy"), "{text}");
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

/// Checks the macOS reply against the OS tools rather than the implementation's APIs.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn info_reports_macos_product_and_kernel_versions() {
    let read_version = |command: &str, argument: &str| {
        let output = std::process::Command::new(command)
            .arg(argument)
            .output()
            .expect("run macOS version command");
        assert!(output.status.success(), "{command}: {output:?}");
        let version = String::from_utf8(output.stdout)
            .expect("UTF-8 version")
            .trim()
            .to_string();
        assert!(!version.is_empty(), "{command} returned an empty version");
        version
    };
    let product_version = read_version("sw_vers", "-productVersion");
    let kernel_version = read_version("uname", "-r");
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance(&registry, None).await;
    let engine = factory_harness(registry);

    match engine
        .process_event(event("i2", "/info", "private", false))
        .await
    {
        PipelineResult::BuiltinReplied { command, replies } => {
            assert_eq!(command, "info");
            let text = reply_text(&replies);
            let architecture = match std::env::consts::ARCH {
                "aarch64" => "ARM64 (aarch64)",
                "x86_64" => "x86-64 (x86_64)",
                architecture => architecture,
            };
            let expected = format!(
                "系统: macOS {product_version} | Kernel: Darwin {kernel_version} | Arch: {architecture}"
            );
            assert_eq!(text.lines().next(), Some(expected.as_str()));
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

/// Provider that records every request so the assembled prompt can be asserted.
struct RecordingProvider {
    requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        Ok(ChatResponse {
            reasoning_content: None,
            content: Some("ok".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Builds a pipeline whose provider records prompts, with a node-wide context policy.
#[allow(clippy::type_complexity)]
fn context_harness(
    registry: Arc<InstanceRegistry>,
    node_context: kanon_core::ContextPolicy,
) -> (Arc<PipelineEngine>, Arc<std::sync::Mutex<Vec<ChatRequest>>>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
            "context-test",
            Arc::new(RecordingProvider {
                requests: requests.clone(),
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
            .with_context_policy(Arc::new(kanon_core::ContextPolicyStore::new(node_context))),
    );

    (engine, requests)
}

/// Creates an enabled instance whose context policy is set explicitly.
async fn instance_with_context(
    registry: &InstanceRegistry,
    context_policy: Option<kanon_core::ContextPolicy>,
) -> String {
    registry
        .create(InstanceDraft {
            name: "Context Bot".to_string(),
            enabled: true,
            adapters: vec!["policy".to_string()],
            persona_id: None,
            system_prompt: None,
            model: None,
            reply_policy: None,
            context_policy,
            plugins: Default::default(),
            skills: Default::default(),
            mcp: Default::default(),
            ..Default::default()
        })
        .await
        .expect("create instance")
        .id
}

/// Renders the user turn of the most recent provider request.
fn last_user_prompt(requests: &Arc<std::sync::Mutex<Vec<ChatRequest>>>) -> String {
    requests
        .lock()
        .expect("requests lock")
        .last()
        .and_then(|request| {
            request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == kanon_llm::Role::User)
        })
        .and_then(|message| message.content.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_node_context_policy_is_applied_from_the_first_event() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance_with_context(&registry, None).await;
    let (engine, requests) = context_harness(
        registry,
        kanon_core::ContextPolicy {
            include_channel_id: true,
            include_sender_id: false,
            include_timestamp: false,
            ..Default::default()
        },
    );

    let result = engine
        .process_event(event("ctx1", "hello", "group", true))
        .await;
    assert!(
        matches!(result, PipelineResult::LlmReplied { .. }),
        "unexpected result: {result:?}"
    );

    let prompt = last_user_prompt(&requests);
    assert!(prompt.contains("[群号: group:1]"), "{prompt}");
    assert!(!prompt.contains("[发送者"), "{prompt}");
    assert!(!prompt.contains("[时间"), "{prompt}");
}

#[tokio::test]
async fn the_instance_context_policy_overrides_the_node_one() {
    let registry = Arc::new(InstanceRegistry::in_memory());
    instance_with_context(
        &registry,
        Some(kanon_core::ContextPolicy {
            include_channel_id: false,
            include_sender_id: true,
            include_timestamp: false,
            ..Default::default()
        }),
    )
    .await;
    // The node policy asks for the opposite; the instance wins.
    let (engine, requests) = context_harness(
        registry,
        kanon_core::ContextPolicy {
            include_channel_id: true,
            include_sender_id: false,
            include_timestamp: false,
            ..Default::default()
        },
    );

    engine
        .process_event(event("ctx2", "hello", "group", true))
        .await;

    let prompt = last_user_prompt(&requests);
    assert!(prompt.contains("[发送者: user:1]"), "{prompt}");
    assert!(!prompt.contains("[群号"), "{prompt}");
}
