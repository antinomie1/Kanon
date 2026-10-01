//! Notices (joins, pokes, recalls) and quoted replies through the real pipeline.
//!
//! The contract: a notice is answered only when the event policy asks for it, and then regardless
//! of the reply policy; a recall becomes a note on the next turn only for a message the model saw;
//! a group reply quotes its message only when the reply policy says so.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    EventPolicy, EventPolicyStore, META_BOT_MENTIONED, META_CONVERSATION_KIND, META_NOTICE,
    META_NOTICE_ACTOR, META_NOTICE_TARGET, ReplyMode, ReplyPolicy, ReplyPolicyStore,
};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::message_segment::Segment;

/// Provider recording the text of the current user turn of every request.
struct RecordingProvider {
    turns: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let turn = request
            .messages
            .last()
            .and_then(|message| message.content.clone())
            .unwrap_or_default();
        self.turns.lock().unwrap().push(turn);
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
    reply: ReplyPolicy,
    events: EventPolicy,
) -> (Arc<PipelineEngine>, Arc<Mutex<Vec<String>>>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(InstanceDraft {
            name: "Notice Bot".to_string(),
            enabled: true,
            adapters: vec!["qq".to_string()],
            persona_id: None,
            system_prompt: None,
            model: None,
            reply_policy: None,
            context_policy: None,
            plugins: Default::default(),
            skills: Default::default(),
            mcp: Default::default(),
            ..Default::default()
        })
        .await
        .expect("create instance");

    let turns = Arc::new(Mutex::new(Vec::new()));
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder(
            "notice-test",
            Arc::new(RecordingProvider {
                turns: turns.clone(),
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
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(reply)))
            .with_event_policy(Arc::new(EventPolicyStore::new(events))),
    );
    (engine, turns)
}

fn metadata(fields: &[(&str, Kind)]) -> prost_types::Struct {
    prost_types::Struct {
        fields: fields
            .iter()
            .map(|(key, kind)| {
                (
                    key.to_string(),
                    prost_types::Value {
                        kind: Some(kind.clone()),
                    },
                )
            })
            .collect(),
    }
}

fn text(value: &str) -> Kind {
    Kind::StringValue(value.to_string())
}

fn message(event_id: &str, sender: &str, content: &str, mentioned: bool) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: event_id.to_string(),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: sender.to_string(),
        raw_text: content.to_string(),
        segments: Vec::new(),
        metadata: Some(metadata(&[
            (META_CONVERSATION_KIND, text("group")),
            (META_BOT_MENTIONED, Kind::BoolValue(mentioned)),
        ])),
    }
}

fn notice(kind: &str, sender: &str, extra: &[(&str, Kind)]) -> PipelineEventRequest {
    let mut fields = vec![
        (META_CONVERSATION_KIND, text("group")),
        (META_NOTICE, text(kind)),
    ];
    fields.extend(extra.iter().cloned());
    PipelineEventRequest {
        event_id: format!("notice-{kind}-{sender}"),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: sender.to_string(),
        raw_text: format!("[{kind}]"),
        segments: Vec::new(),
        metadata: Some(metadata(&fields)),
    }
}

#[tokio::test]
async fn notices_are_ignored_unless_the_event_policy_answers_them() {
    let (engine, turns) = harness(ReplyPolicy::default(), EventPolicy::default()).await;
    let result = engine
        .process_event(notice("poke", "u1", &[(META_NOTICE_ACTOR, text("小明"))]))
        .await;
    assert!(
        matches!(result, PipelineResult::Notice { kind: "poke", .. }),
        "{result:?}"
    );
    assert!(
        turns.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

#[tokio::test]
async fn an_enabled_notice_is_answered_even_under_a_mention_only_policy() {
    let events = EventPolicy {
        reply_to_poke: true,
        welcome_members: true,
        ..EventPolicy::default()
    };
    let (engine, turns) = harness(ReplyPolicy::new(ReplyMode::Mention), events).await;

    let poke = engine
        .process_event(notice("poke", "u1", &[(META_NOTICE_ACTOR, text("小明"))]))
        .await;
    assert!(
        matches!(poke, PipelineResult::LlmReplied { .. }),
        "{poke:?}"
    );
    let join = engine
        .process_event(notice(
            "member_join",
            "u2",
            &[(META_NOTICE_ACTOR, text("小红"))],
        ))
        .await;
    assert!(
        matches!(join, PipelineResult::LlmReplied { .. }),
        "{join:?}"
    );

    let turns = turns.lock().unwrap();
    assert_eq!(turns[0], "[事件] 小明 戳了戳你");
    assert_eq!(turns[1], "[事件] 小红 加入了群聊");
}

#[tokio::test]
async fn a_recall_of_a_seen_message_is_noted_on_the_next_turn_only() {
    let (engine, turns) = harness(ReplyPolicy::default(), EventPolicy::default()).await;

    engine
        .process_event(message("m1", "u1", "我的密码是 hunter2", true))
        .await;
    let recall = engine
        .process_event(notice(
            "recall",
            "u1",
            &[
                (META_NOTICE_TARGET, text("m1")),
                (META_NOTICE_ACTOR, text("小明")),
            ],
        ))
        .await;
    assert!(
        matches!(recall, PipelineResult::Notice { kind: "recall", .. }),
        "{recall:?}"
    );

    // Another member's turn does not receive the note; the recaller's next turn does, once.
    engine
        .process_event(message("m2", "u2", "路过", true))
        .await;
    engine
        .process_event(message("m3", "u1", "刚才发错了", true))
        .await;
    engine
        .process_event(message("m4", "u1", "在吗", true))
        .await;

    let turns = turns.lock().unwrap();
    assert_eq!(turns.len(), 4, "the recall itself is not a turn: {turns:?}");
    assert_eq!(turns[1], "路过");
    assert_eq!(
        turns[2],
        "[通知] 小明撤回了之前的消息「我的密码是 hunter2」 刚才发错了"
    );
    assert_eq!(turns[3], "在吗");
}

#[tokio::test]
async fn a_recall_of_an_unseen_message_reveals_nothing() {
    let (engine, turns) =
        harness(ReplyPolicy::new(ReplyMode::Mention), EventPolicy::default()).await;

    // Suppressed by the mention policy: the model never saw it.
    engine
        .process_event(message("m1", "u1", "悄悄话", false))
        .await;
    let recall = engine
        .process_event(notice("recall", "u1", &[(META_NOTICE_TARGET, text("m1"))]))
        .await;
    assert!(
        matches!(recall, PipelineResult::Notice { .. }),
        "{recall:?}"
    );
    engine
        .process_event(message("m2", "u1", "@bot 你好", true))
        .await;

    assert_eq!(*turns.lock().unwrap(), vec!["@bot 你好".to_string()]);
}

#[tokio::test]
async fn group_replies_quote_their_message_only_when_asked() {
    let quoting = ReplyPolicy {
        quote_message: true,
        ..ReplyPolicy::default()
    };
    let (engine, _) = harness(quoting, EventPolicy::default()).await;
    let PipelineResult::LlmReplied { replies, .. } = engine
        .process_event(message("m1", "u1", "你好", true))
        .await
    else {
        panic!("expected a model reply");
    };
    match &replies[0].segment {
        Some(Segment::Reply(reply)) => assert_eq!(reply.target_message_id, "m1"),
        other => panic!("the reply should quote m1 first, got {other:?}"),
    }

    let mut private = message("m2", "u1", "你好", false);
    private.metadata = Some(metadata(&[(META_CONVERSATION_KIND, text("private"))]));
    let PipelineResult::LlmReplied { replies, .. } = engine.process_event(private).await else {
        panic!("expected a model reply");
    };
    assert!(
        matches!(replies[0].segment, Some(Segment::Text(_))),
        "private chats are never quoted"
    );

    let (engine, _) = harness(ReplyPolicy::default(), EventPolicy::default()).await;
    let PipelineResult::LlmReplied { replies, .. } = engine
        .process_event(message("m3", "u1", "你好", true))
        .await
    else {
        panic!("expected a model reply");
    };
    assert_eq!(replies.len(), 1, "no quote by default");
}
