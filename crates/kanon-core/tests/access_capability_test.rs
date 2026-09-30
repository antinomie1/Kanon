//! Command permissions, adapter capabilities, and the generic adapter calls they gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::manifest::PluginManifest;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    AdapterError, Capability, CommandAccess, CommandPolicy, CommandPolicyStore, EventPolicy,
    EventPolicyStore, META_BOT_MENTIONED, META_CONVERSATION_KIND, META_NOTICE, META_SENDER_ROLE,
    PlatformAdapter, ReplyPolicy, ReplyPolicyStore,
};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, DeliverMessageResponse, PipelineEventRequest};

/// A built-in adapter recording the generic calls the core makes into it.
#[derive(Default)]
struct FakeAdapter {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl PlatformAdapter for FakeAdapter {
    fn platform(&self) -> &str {
        "qq"
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::Acknowledge, Capability::FriendRequests]
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        Ok(DeliverMessageResponse::default())
    }

    async fn acknowledge(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("ack:{}", event.event_id));
        Ok(())
    }

    async fn accept_request(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("accept:{}", event.event_id));
        Ok(())
    }
}

struct Echo;

#[async_trait]
impl LlmProvider for Echo {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            reasoning_content: None,
            content: Some("好的".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

async fn harness(
    commands: CommandPolicy,
    events: EventPolicy,
    reply: ReplyPolicy,
) -> (Arc<PipelineEngine>, Arc<Supervisor>, Arc<FakeAdapter>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);
    let adapter = Arc::new(FakeAdapter::default());
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .expect("register adapter");

    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(InstanceDraft {
            name: "Access Bot".to_string(),
            enabled: true,
            adapters: vec!["qq".to_string()],
            ..Default::default()
        })
        .await
        .expect("create instance");
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder("access-test", Arc::new(Echo))
            .memory(memory)
            .model("test-model")
            .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor.clone())
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_instances(registry)
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(reply)))
            .with_event_policy(Arc::new(EventPolicyStore::new(events)))
            .with_command_policy(Arc::new(CommandPolicyStore::new(commands))),
    );
    (engine, supervisor, adapter)
}

fn event(id: &str, text: &str, kind: &str, fields: &[(&str, Kind)]) -> PipelineEventRequest {
    let mut all = vec![
        (META_CONVERSATION_KIND, Kind::StringValue(kind.into())),
        (META_BOT_MENTIONED, Kind::BoolValue(true)),
    ];
    all.extend(fields.iter().cloned());
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: format!("{kind}:1"),
        sender_id: "u1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: Some(prost_types::Struct {
            fields: all
                .into_iter()
                .map(|(key, kind)| (key.to_string(), prost_types::Value { kind: Some(kind) }))
                .collect(),
        }),
    }
}

#[test]
fn a_command_policy_is_normalized_and_validated() {
    let policy = CommandPolicy {
        admins: vec![" qq:u1 ".into(), "qq:u1".into()],
        group_admins_are_admins: false,
        access: [("/NEW".to_string(), CommandAccess::Admins)].into(),
    }
    .prepare()
    .expect("valid policy");
    assert_eq!(policy.admins, vec!["qq:u1".to_string()]);
    assert_eq!(policy.access.get("new"), Some(&CommandAccess::Admins));

    let missing_platform = CommandPolicy {
        admins: vec!["12345".into()],
        ..CommandPolicy::default()
    };
    assert!(missing_platform.prepare().is_err());
}

#[test]
fn access_levels_follow_the_conversation_and_the_sender() {
    let policy = CommandPolicy::default();
    let member = event(
        "e",
        "/new",
        "group",
        &[(META_SENDER_ROLE, Kind::StringValue("member".into()))],
    );
    let owner = event(
        "e",
        "/new",
        "group",
        &[(META_SENDER_ROLE, Kind::StringValue("owner".into()))],
    );
    let private = event("e", "/new", "private", &[]);

    assert!(
        !policy.allows("new", &member),
        "admins_in_groups: members may not reset a group"
    );
    assert!(
        policy.allows("new", &owner),
        "group owners count as admins by default"
    );
    assert!(
        policy.allows("new", &private),
        "anyone may reset a private chat"
    );
    assert!(
        !policy.allows("model", &private),
        "model switching is admin-only everywhere"
    );
    assert!(
        policy.allows("weather", &member),
        "unlisted commands are open"
    );

    let listed = CommandPolicy {
        admins: vec!["qq:u1".into()],
        group_admins_are_admins: false,
        ..CommandPolicy::default()
    };
    assert!(listed.allows("model", &private));
    let strict = CommandPolicy {
        group_admins_are_admins: false,
        ..CommandPolicy::default()
    };
    assert!(
        !strict.allows("new", &owner),
        "roles only count when the operator allows it"
    );
}

#[tokio::test]
async fn a_denied_command_tells_the_sender_their_id() {
    let (engine, _, _) = harness(
        CommandPolicy::default(),
        EventPolicy::default(),
        ReplyPolicy::default(),
    )
    .await;
    let result = engine
        .process_event(event("e1", "/model", "group", &[]))
        .await;
    let PipelineResult::CommandDenied { command, replies } = result else {
        panic!("expected a denial, got {result:?}");
    };
    assert_eq!(command, "model");
    let Some(Segment::Text(text)) = &replies[0].segment else {
        panic!("the denial is a text reply");
    };
    assert!(text.content.contains("qq:u1"), "{}", text.content);
}

#[tokio::test]
async fn catalogs_report_capabilities_and_calls_follow_the_policies() {
    let accepting = EventPolicy {
        accept_friend_requests: true,
        ..EventPolicy::default()
    };
    let acknowledging = ReplyPolicy {
        acknowledge: true,
        ..ReplyPolicy::default()
    };
    let (engine, supervisor, adapter) =
        harness(CommandPolicy::default(), accepting, acknowledging).await;

    let catalog = supervisor.adapter_catalog().await;
    assert_eq!(
        catalog[0].capabilities,
        vec![Capability::Acknowledge, Capability::FriendRequests]
    );

    let request = event(
        "req1",
        "[friend_request]",
        "private",
        &[(META_NOTICE, Kind::StringValue("friend_request".into()))],
    );
    let result = engine.process_event(request).await;
    assert!(
        matches!(result, PipelineResult::Notice { .. }),
        "{result:?}"
    );
    engine
        .process_event(event("m1", "你好", "private", &[]))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let calls = adapter.calls.lock().unwrap().clone();
    assert!(calls.contains(&"accept:req1".to_string()), "{calls:?}");
    assert!(calls.contains(&"ack:m1".to_string()), "{calls:?}");
}

#[tokio::test]
async fn nothing_is_accepted_or_acknowledged_by_default() {
    let (engine, _, adapter) = harness(
        CommandPolicy::default(),
        EventPolicy::default(),
        ReplyPolicy::default(),
    )
    .await;
    engine
        .process_event(event(
            "req1",
            "[friend_request]",
            "private",
            &[(META_NOTICE, Kind::StringValue("friend_request".into()))],
        ))
        .await;
    engine
        .process_event(event("m1", "你好", "private", &[]))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(adapter.calls.lock().unwrap().is_empty());
}

#[test]
fn plugin_manifests_may_only_declare_capabilities_without_callbacks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let write = |capabilities: &str| {
        let path = dir.path().join("plugin.toml");
        std::fs::write(
            &path,
            format!(
                "[plugin]\nid = \"org.example.bridge\"\nname = \"Bridge\"\nversion = \"0.1.0\"\n\
                 runtime = \"python\"\nentrypoint = \"main.py\"\n\n\
                 [adapter]\nplatform = \"bridge\"\ncapabilities = [{capabilities}]\n"
            ),
        )
        .unwrap();
        PluginManifest::load_from_file(&path)
    };
    let manifest = write("\"quote_reply\", \"sender_name\"").expect("valid capabilities");
    assert_eq!(
        manifest.adapter.unwrap().capabilities,
        vec![Capability::QuoteReply, Capability::SenderName]
    );
    assert!(
        write("\"acknowledge\"").is_err(),
        "plugins cannot be called back"
    );
    assert!(
        write("\"telepathy\"").is_err(),
        "unknown capabilities are rejected"
    );
}
