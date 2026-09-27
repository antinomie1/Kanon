//! Tests for the bot-instance gate, instance-scoped sessions and the built-in `/new` command.
//!
//! These lock in the operator-visible contract: adapters alone answer nothing, an instance decides
//! who answers, and `/new` rotates a conversation without ever reaching the model.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry, ItemPolicy};
use kanon_core::pipeline::{PipelineEngine, PipelineObserver, PipelineResult, PipelineStage};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_core::toggle::{PLUGIN_SECTION, ToggleStore};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{Memory, SlidingWindowMemory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::v1::{PipelineEventRequest, PluginMeta};

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
) -> (PipelineEngine, Arc<AtomicUsize>, Arc<dyn Memory>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let calls = Arc::new(AtomicUsize::new(0));
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(20));
    let agent = Arc::new(
        Agent::builder(
            "gating-test",
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
        )
        .memory(memory.clone())
        .model("test-model")
        .build(),
    );

    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_instances(registry);

    (engine, calls, memory)
}

/// Observer that records how many hosts survived the per-instance policy filter.
#[derive(Default)]
struct HostCounter {
    host_counts: std::sync::Mutex<Vec<usize>>,
}

impl PipelineObserver for HostCounter {
    fn on_stage(&self, stage: &PipelineStage) {
        if let PipelineStage::PreFilterStarted { host_count, .. } = stage {
            self.host_counts
                .lock()
                .expect("host counts lock")
                .push(*host_count);
        }
    }
}

impl HostCounter {
    /// Returns the host count reported by the last pipeline start.
    fn last(&self) -> Option<usize> {
        self.host_counts
            .lock()
            .expect("host counts lock")
            .last()
            .copied()
    }
}

/// Registers a plugin host with one command, mirroring a real out-of-process host.
async fn register_plugin_host(supervisor: &Supervisor, plugin_id: &str) {
    let host_id = plugin_id.replace('.', "_");
    let host = Arc::new(ManagedHost::new(
        host_id.clone(),
        std::path::PathBuf::from(format!("/tmp/{host_id}.sock")),
        tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy(),
        vec![PluginMeta {
            id: plugin_id.to_string(),
            name: plugin_id.to_string(),
            version: "1.0.0".to_string(),
            author: "Tester".to_string(),
            description: "Policy fixture host".to_string(),
            commands: Vec::new(),
            tools: Vec::new(),
        }],
        100,
    ));
    supervisor.register_managed_host(host).await;
}

/// Inbound event fixture.
fn event(platform: &str, event_id: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: event_id.to_string(),
        platform: platform.to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "user:1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: None,
    }
}

/// Creates an instance in the catalog.
async fn instance(registry: &InstanceRegistry, enabled: bool, adapters: &[&str]) -> String {
    registry
        .create(InstanceDraft {
            name: "Test Bot".to_string(),
            enabled,
            adapters: adapters.iter().map(|a| (*a).to_string()).collect(),
            persona_id: None,
            system_prompt: None,
            model: None,
            reply_policy: None,
            plugins: Default::default(),
            skills: Default::default(),
            mcp: Default::default(),
        })
        .await
        .expect("create instance")
        .id
}

#[tokio::test]
async fn inbound_events_are_dropped_when_no_instance_claims_the_platform() {
    let registry = Arc::new(InstanceRegistry::default());
    // An adapter exists on the node, but no instance serves it yet.
    let (engine, calls, _memory) = harness(registry).await;

    let result = engine
        .process_event(event("qqofficial", "evt-1", "你好"))
        .await;

    assert!(
        matches!(result, PipelineResult::NoInstance { .. }),
        "unclaimed platform must be dropped, got {result:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the model must not be called"
    );
}

#[tokio::test]
async fn a_disabled_instance_still_answers_nothing() {
    let registry = Arc::new(InstanceRegistry::default());
    instance(&registry, false, &["qqofficial"]).await;
    let (engine, calls, _memory) = harness(registry).await;

    let result = engine
        .process_event(event("qqofficial", "evt-2", "你好"))
        .await;

    assert!(matches!(result, PipelineResult::NoInstance { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_enabled_instance_answers_and_namespaces_the_session() {
    let registry = Arc::new(InstanceRegistry::default());
    let id = instance(&registry, true, &["qqofficial"]).await;
    let (engine, calls, memory) = harness(registry).await;

    let result = engine
        .process_event(event("qqofficial", "evt-3", "你好"))
        .await;

    match result {
        PipelineResult::LlmReplied { content, replies } => {
            assert_eq!(content, "模型回复");
            assert_eq!(replies.len(), 1);
        }
        other => panic!("expected an LLM reply, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The conversation lives in a session namespaced by the instance.
    let session_id = format!("instance:{id}:group:1:user:1#0");
    let history = memory
        .get_messages(&session_id)
        .await
        .expect("session history");
    assert!(
        !history.is_empty(),
        "the instance session must hold the turn"
    );
}

#[tokio::test]
async fn new_command_rotates_the_session_without_touching_the_model() {
    let registry = Arc::new(InstanceRegistry::default());
    let id = instance(&registry, true, &["qqofficial"]).await;
    let (engine, calls, memory) = harness(registry).await;

    // Seed the first session.
    engine
        .process_event(event("qqofficial", "evt-4", "你好"))
        .await;
    let first_session = format!("instance:{id}:group:1:user:1#0");
    let first_history = memory
        .get_messages(&first_session)
        .await
        .expect("first session history");
    assert!(!first_history.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // `/new` is handled by the core: the model must not see it.
    let rotated = engine
        .process_event(event("qqofficial", "evt-5", "/new"))
        .await;
    let session_id = match rotated {
        PipelineResult::SessionRotated {
            instance_id,
            session_id,
            replies,
        } => {
            assert_eq!(instance_id, id);
            assert_eq!(replies.len(), 1, "the command confirms the rotation");
            session_id
        }
        other => panic!("expected a session rotation, got {other:?}"),
    };
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the built-in command must never reach the LLM"
    );
    assert_eq!(session_id, format!("instance:{id}:group:1:user:1#1"));

    // The next message continues in the new session, while the old one is retained intact.
    engine
        .process_event(event("qqofficial", "evt-6", "再问一次"))
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let new_history = memory.get_messages(&session_id).await.expect("new session");
    assert!(
        !new_history.is_empty(),
        "the new session must receive the turn"
    );
    let retained = memory
        .get_messages(&first_session)
        .await
        .expect("old session retained");
    assert_eq!(
        retained.len(),
        first_history.len(),
        "the previous session must keep its history unchanged"
    );
}

#[tokio::test]
async fn a_plugin_disabled_for_an_instance_is_removed_before_pre_filter() {
    const PLUGIN_ID: &str = "org.kanon.plugin.noisy";
    let registry = Arc::new(InstanceRegistry::default());
    registry
        .create(InstanceDraft {
            name: "Policy Bot".to_string(),
            enabled: true,
            adapters: vec!["qqofficial".to_string()],
            persona_id: None,
            system_prompt: None,
            model: None,
            reply_policy: None,
            plugins: std::collections::HashMap::from([(
                PLUGIN_ID.to_string(),
                ItemPolicy::Disable,
            )]),
            skills: Default::default(),
            mcp: Default::default(),
        })
        .await
        .expect("create instance");

    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);
    register_plugin_host(&supervisor, PLUGIN_ID).await;

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
    let observer = Arc::new(HostCounter::default());

    // The store is authoritative for the node-wide switch; the instance policy is what removes the
    // host here, so the plugin stays enabled globally.
    let toggles = Arc::new(ToggleStore::in_memory());
    toggles
        .set_enabled(PLUGIN_SECTION, PLUGIN_ID, true)
        .await
        .expect("enable plugin");

    let engine = PipelineEngine::new(supervisor.clone())
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_instances(registry)
        .with_toggles(toggles)
        .with_observer(observer.clone());

    engine
        .process_event(event("qqofficial", "evt-8", "你好"))
        .await;

    assert_eq!(
        observer.last(),
        Some(0),
        "a plugin disabled for this instance must never reach pre-filter"
    );
}

#[tokio::test]
async fn an_unspecified_plugin_policy_keeps_the_host_in_the_pipeline() {
    const PLUGIN_ID: &str = "org.kanon.plugin.quiet";
    let registry = Arc::new(InstanceRegistry::default());
    instance(&registry, true, &["qqofficial"]).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);
    register_plugin_host(&supervisor, PLUGIN_ID).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(
        Agent::builder(
            "policy-test",
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
        )
        .memory(Arc::new(SlidingWindowMemory::new(20)))
        .model("test-model")
        .build(),
    );
    let observer = Arc::new(HostCounter::default());
    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_instances(registry)
        .with_toggles(Arc::new(ToggleStore::in_memory()))
        .with_observer(observer.clone());

    engine
        .process_event(event("qqofficial", "evt-9", "你好"))
        .await;

    assert_eq!(
        observer.last(),
        Some(1),
        "with no override the host must stay available to the instance"
    );
}

#[tokio::test]
async fn plugin_commands_cannot_shadow_the_builtin_new_command() {
    let registry = Arc::new(InstanceRegistry::default());
    instance(&registry, true, &["qqofficial"]).await;
    let (engine, calls, _memory) = harness(registry).await;

    // No plugin registers `new`, so a naive implementation would fall through to the model.
    let result = engine
        .process_event(event("qqofficial", "evt-7", "/NEW"))
        .await;

    assert!(matches!(result, PipelineResult::SessionRotated { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
