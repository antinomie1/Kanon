//! Protocol reasoning is delivered only when requested; answer examples remain answer text.

use async_trait::async_trait;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{META_CONVERSATION_KIND, ReplyPolicy, ReplyPolicyStore};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::InMemory;
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types;
use kanon_proto::v1::PipelineEventRequest;
use kanon_proto::v1::message_segment::Segment;
use std::sync::Arc;

/// Synthetic response with independently controlled answer and reasoning channels.
struct Provider(ChatResponse);
#[async_trait]
impl LlmProvider for Provider {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(self.0.clone())
    }
}

/// Runs the actual private-message pipeline and returns its platform text segments.
async fn delivered_texts(
    send_reasoning: bool,
    content: &str,
    reasoning: Option<&str>,
) -> Vec<String> {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    let provider = Provider(ChatResponse {
        content: Some(content.into()),
        reasoning_content: reasoning.map(str::to_string),
        ..ChatResponse::default()
    });
    let agent = Arc::new(
        Agent::builder("fixture", Arc::new(provider))
            .memory(Arc::new(InMemory::new()))
            .compaction(None)
            .build(),
    );
    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy {
            send_reasoning,
            ..ReplyPolicy::default()
        })));
    let metadata = prost_types::Struct {
        fields: [(
            META_CONVERSATION_KIND.to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("private".into())),
            },
        )]
        .into_iter()
        .collect(),
    };
    let result = engine
        .process_event(PipelineEventRequest {
            event_id: "e1".into(),
            platform: "test".into(),
            channel_id: "c2c:1".into(),
            sender_id: "user:1".into(),
            raw_text: "question".into(),
            segments: vec![],
            metadata: Some(metadata),
        })
        .await;
    let PipelineResult::LlmReplied { replies, .. } = result else {
        panic!("{result:?}");
    };
    replies
        .into_iter()
        .map(|reply| match reply.segment {
            Some(Segment::Text(text)) => text.content,
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn protocol_channels_control_display_without_reinterpreting_answer_tags() {
    for text in [
        "Use `<think>literal</think>` in examples",
        "```xml\n<think>outer<think>inner</think>end</think>\n```",
        "<think>an intentional example</think>",
        "ordinary answer <think>nested<think>example</think>end</think>",
        "Explain </think> and </think >",
    ] {
        assert_eq!(
            delivered_texts(false, text, Some("native thought")).await,
            [text]
        );
        assert_eq!(
            delivered_texts(true, text, Some("native thought")).await,
            ["native thought", text]
        );
    }
}

#[tokio::test]
async fn legacy_leading_nested_envelopes_are_separated_before_delivery() {
    let text = "<think>outer <think>inner</think> secret</think>answer";
    assert_eq!(delivered_texts(false, text, None).await, ["answer"]);
    let replies = delivered_texts(true, text, None).await;
    assert_eq!(replies.last().unwrap(), "answer");
    assert!(replies[0].contains("secret"));
}

#[tokio::test]
async fn unrecovered_tool_call_markup_is_delivered_verbatim() {
    // Markup the agent could not parse as a call stays text, so a markup-only answer still
    // produces a reply instead of nothing.
    for text in [
        "answer <tool_call>bad</tool_call>",
        "Models emit `<tool_call>{\"name\": ...}</tool_call>` blocks",
        "<tool_call>not a tool call at all</tool_call>",
    ] {
        assert_eq!(delivered_texts(false, text, None).await, [text]);
    }
}
