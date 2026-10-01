//! Tests for what a chat platform receives when the model's answer carries reasoning.
//!
//! Reasoning tags and tool-call markup must never be delivered. Reasoning itself is delivered only
//! when the reply policy opts in, and then as plain content without any tag markup.

use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{META_CONVERSATION_KIND, ReplyPolicy, ReplyPolicyStore};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types;
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::message_segment::Segment;

/// Provider whose answer leaks a template-opened reasoning block and unparsable tool markup.
struct LeakyProvider;

#[async_trait]
impl LlmProvider for LeakyProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            reasoning_content: Some("native thought".to_string()),
            content: Some(
                "leaked thought</think>\nanswer <tool_call>not a call</tool_call>".to_string(),
            ),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Runs one private message through a pipeline using `policy` and returns the text segments sent.
async fn delivered_texts(policy: ReplyPolicy) -> Vec<String> {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder("reasoning-test", Arc::new(LeakyProvider))
            .memory(memory)
            .model("test-model")
            .build(),
    );
    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_reply_policy(Arc::new(ReplyPolicyStore::new(policy)));

    let metadata = prost_types::Struct {
        fields: [(
            META_CONVERSATION_KIND.to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("private".to_string())),
            },
        )]
        .into_iter()
        .collect(),
    };
    let result = engine
        .process_event(PipelineEventRequest {
            event_id: "e1".to_string(),
            platform: "test".to_string(),
            channel_id: "c2c:1".to_string(),
            sender_id: "user:1".to_string(),
            raw_text: "question".to_string(),
            segments: Vec::new(),
            metadata: Some(metadata),
        })
        .await;
    let PipelineResult::LlmReplied { content, replies } = result else {
        panic!("unexpected result: {result:?}");
    };
    assert_eq!(content, "answer");
    replies
        .into_iter()
        .map(|reply| match reply.segment {
            Some(Segment::Text(text)) => text.content,
            other => panic!("unexpected segment: {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn reasoning_and_tool_markup_are_never_delivered_by_default() {
    assert_eq!(delivered_texts(ReplyPolicy::default()).await, ["answer"]);
}

#[tokio::test]
async fn opted_in_reasoning_is_sent_first_as_plain_content() {
    let policy = ReplyPolicy {
        send_reasoning: true,
        ..ReplyPolicy::default()
    };
    assert_eq!(
        delivered_texts(policy).await,
        ["native thought\n\nleaked thought", "answer"]
    );
}
