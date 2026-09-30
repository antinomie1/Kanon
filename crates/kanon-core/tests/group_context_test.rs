//! Group context: shared sessions, speaker labels and observation of unanswered group messages.
//!
//! The invariants: an observed line reaches a session exactly once (as leading text of that
//! session's next turn), the bot's own replies count as seen by the session that produced them,
//! and every request's message list is the previous one plus the new turn — the prefix a provider
//! caches never changes.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    META_BOT_MENTIONED, META_CONVERSATION_KIND, META_SENDER_NAME, ReplyMode, ReplyPolicy,
    ReplyPolicyStore, SessionScope,
};
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::PipelineEventRequest;

/// Provider recording every request's full message list.
struct RecordingProvider {
    requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.messages.clone());
        Ok(ChatResponse {
            content: Some("好的".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

type Requests = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

async fn harness(scope: SessionScope, observe: bool) -> (Arc<PipelineEngine>, Requests) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(InstanceDraft {
            name: "Group Bot".to_string(),
            enabled: true,
            adapters: vec!["qq".to_string()],
            session_scope: scope,
            observe_group: observe,
            ..Default::default()
        })
        .await
        .expect("create instance");

    let requests: Requests = Arc::default();
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder(
            "group-test",
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
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy::new(
                ReplyMode::Mention,
            )))),
    );
    (engine, requests)
}

fn group_message(
    id: &str,
    sender: &str,
    name: &str,
    text: &str,
    mentioned: bool,
) -> PipelineEventRequest {
    let value = |kind: Kind| prost_types::Value { kind: Some(kind) };
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: sender.to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: Some(prost_types::Struct {
            fields: [
                (
                    META_CONVERSATION_KIND.to_string(),
                    value(Kind::StringValue("group".into())),
                ),
                (
                    META_BOT_MENTIONED.to_string(),
                    value(Kind::BoolValue(mentioned)),
                ),
                (
                    META_SENDER_NAME.to_string(),
                    value(Kind::StringValue(name.into())),
                ),
            ]
            .into_iter()
            .collect(),
        }),
    }
}

fn last_user_text(messages: &[ChatMessage]) -> String {
    messages
        .last()
        .and_then(|message| message.content.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn an_observing_shared_session_sees_each_line_once_and_keeps_its_prefix() {
    let (engine, requests) = harness(SessionScope::Group, true).await;

    for (id, sender, name, text) in [
        ("m1", "u2", "小红", "今天吃什么"),
        ("m2", "u3", "小刚", "火锅"),
    ] {
        let result = engine
            .process_event(group_message(id, sender, name, text, false))
            .await;
        assert!(
            matches!(result, PipelineResult::ReplySuppressed { .. }),
            "{result:?}"
        );
    }
    engine
        .process_event(group_message("m3", "u1", "小明", "@bot 你们在聊啥", true))
        .await;
    engine
        .process_event(group_message("m4", "u2", "小红", "好耶", false))
        .await;
    engine
        .process_event(group_message("m5", "u1", "小明", "再说说", true))
        .await;

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        last_user_text(&requests[0]),
        "[群聊记录]\n小红: 今天吃什么\n小刚: 火锅\n[当前消息] 小明: @bot 你们在聊啥"
    );
    // Only what happened since: not m3 (already history), not the bot's own reply.
    assert_eq!(
        last_user_text(&requests[1]),
        "[群聊记录]\n小红: 好耶\n[当前消息] 小明: 再说说"
    );
    // Append-only: the second request is the first plus the reply and the new turn.
    let first = &requests[0];
    assert_eq!(&requests[1][..first.len()], first.as_slice());
    assert_eq!(requests[1].len(), first.len() + 2);
}

#[tokio::test]
async fn per_member_sessions_each_catch_up_on_what_they_missed() {
    let (engine, requests) = harness(SessionScope::User, true).await;

    engine
        .process_event(group_message("m1", "u2", "小红", "有人吗", false))
        .await;
    engine
        .process_event(group_message("m2", "u1", "小明", "@bot 在吗", true))
        .await;
    engine
        .process_event(group_message("m3", "u2", "小红", "@bot 我也在", true))
        .await;

    let requests = requests.lock().unwrap();
    assert_eq!(
        last_user_text(&requests[0]),
        "[群聊记录]\n小红: 有人吗\n[当前消息] 小明: @bot 在吗"
    );
    // 小红's own session never saw 小明's turn or the bot's answer to it; her own m1 is included
    // because her session never saw it either.
    assert_eq!(
        last_user_text(&requests[1]),
        "[群聊记录]\n小红: 有人吗\n小明: @bot 在吗\n你: 好的\n[当前消息] 小红: @bot 我也在"
    );
    assert_eq!(
        requests[1].len(),
        requests[0].len(),
        "separate sessions, separate histories"
    );
}

#[tokio::test]
async fn a_shared_session_without_observation_labels_speakers() {
    let (engine, requests) = harness(SessionScope::Group, false).await;

    engine
        .process_event(group_message("m1", "u1", "小明", "@bot 你好", true))
        .await;
    engine
        .process_event(group_message("m2", "u2", "小红", "@bot 他是谁", true))
        .await;
    engine
        .process_event(group_message("m3", "u3", "小刚", "无人理我", false))
        .await;

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "unaddressed and unobserved: not a turn");
    assert_eq!(last_user_text(&requests[0]), "小明: @bot 你好");
    assert_eq!(last_user_text(&requests[1]), "小红: @bot 他是谁");
    assert_eq!(
        &requests[1][..requests[0].len()],
        requests[0].as_slice(),
        "both members speak in one session"
    );
}
