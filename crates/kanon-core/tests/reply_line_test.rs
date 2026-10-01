//! Line formatting and actual FIFO delivery of model replies.

mod common;
#[path = "../src/pipeline/reply.rs"]
mod reply;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{ReplyPolicy, ReplyPolicyStore};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    ImageSegment, IngestEventRequest, MessageSegment, PipelineEventRequest, ReplySegment,
    TextSegment,
};
use tokio::sync::mpsc;

/// Text segment fixture preserving its bytes.
fn text(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }
}

/// Quote segment fixture.
fn quote() -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Reply(ReplySegment {
            target_message_id: "source-event".to_string(),
            snippet: String::new(),
        })),
    }
}

/// Image segment fixture that never loads a real image.
fn image() -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Image(ImageSegment {
            source: Some(kanon_proto::v1::image_segment::Source::Url(
                "https://example.invalid/card.png".to_string(),
            )),
            mime_type: Some("image/png".to_string()),
            filename: None,
        })),
    }
}

#[test]
fn nonblank_lines_preserve_indentation_and_skip_empty_crlf_and_unicode_blank_lines() {
    assert_eq!(
        reply::split_reply_lines(&[text("\nfirst\r\n\r\n \t\n\u{3000}\n  第二行  \nlast\n")]),
        vec![
            vec![text("first")],
            vec![text("  第二行  ")],
            vec![text("last")]
        ],
    );
}

#[test]
fn quote_is_on_the_first_line_and_images_are_not_duplicated() {
    assert_eq!(
        reply::split_reply_lines(&[quote(), text("first\n\nlast"), image(), image()]),
        vec![
            vec![quote(), text("first")],
            vec![text("last"), image(), image()]
        ],
    );
}

#[test]
fn blank_text_and_quotes_are_not_sent_but_image_only_replies_survive() {
    assert!(reply::split_reply_lines(&[quote(), text("\n \t\r\n")]).is_empty());
    assert_eq!(
        reply::split_reply_lines(&[quote(), text("\n \t\r\n"), image()]),
        vec![vec![quote(), image()]],
    );
}

/// Provider with one synthetic multiline final answer.
struct MultilineProvider;

#[async_trait]
impl LlmProvider for MultilineProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            content: Some("first\r\n\r\n \t\n  second\nlast".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Runs the real inbound worker and outbound dispatcher over an in-memory provider/adapter.
async fn delivered_lines(node_split: bool, instance_split: Option<bool>) -> Vec<String> {
    let dir = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let (delivered_tx, mut delivered_rx) = mpsc::channel(8);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared("line-test", delivered_tx))
        .await
        .expect("register adapter");
    let registry = Arc::new(InstanceRegistry::in_memory());
    let instance = registry
        .create(InstanceDraft {
            name: "line-test".to_string(),
            enabled: true,
            adapters: vec!["line-test".to_string()],
            reply_policy: instance_split.map(|split_lines| ReplyPolicy {
                split_lines,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("create instance");
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder("line-test", Arc::new(MultilineProvider))
            .memory(memory.clone())
            .model("test-model")
            .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy {
                split_lines: node_split,
                ..Default::default()
            })))
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent))),
    );
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let worker = engine.clone().start_worker(inbound_rx);
    let dispatcher = engine.clone().start_outbound_dispatcher();
    inbound_tx
        .send(IngestEventRequest {
            platform: "line-test".to_string(),
            event: Some(PipelineEventRequest {
                event_id: "source-event".to_string(),
                platform: "line-test".to_string(),
                channel_id: "conversation".to_string(),
                sender_id: "sender".to_string(),
                raw_text: "hello".to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect("enqueue event");
    drop(inbound_tx);
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .expect("worker finishes")
        .expect("worker succeeds");
    engine.drain(tokio::spawn(async {}), dispatcher).await;
    let history = memory
        .get_messages(&instance.conversation_session_id("conversation:sender"))
        .await
        .expect("stored history");
    assert_eq!(
        history.len(),
        2,
        "delivery must not add model-history turns"
    );
    assert_eq!(
        history[1].content.as_deref(),
        Some("first\r\n\r\n \t\n  second\nlast")
    );
    let mut lines = Vec::new();
    while let Ok(delivery) = delivered_rx.try_recv() {
        assert_eq!(delivery.event_id, "source-event");
        assert_eq!(delivery.platform, "line-test");
        assert_eq!(delivery.channel_id, "conversation");
        assert_eq!(delivery.recipient_id, "sender");
        assert_eq!(delivery.segments.len(), 1);
        match &delivery.segments[0].segment {
            Some(Segment::Text(text)) => lines.push(text.content.clone()),
            other => panic!("expected text, got {other:?}"),
        }
    }
    lines
}

#[tokio::test]
async fn enabled_node_setting_delivers_nonblank_lines_in_order() {
    assert_eq!(
        delivered_lines(true, None).await,
        ["first", "  second", "last"]
    );
}

#[tokio::test]
async fn disabled_setting_keeps_one_message_and_instance_overrides_work_both_ways() {
    let original = ["first\r\n\r\n \t\n  second\nlast"];
    assert_eq!(delivered_lines(false, None).await, original);
    assert_eq!(delivered_lines(true, Some(false)).await, original);
    assert_eq!(
        delivered_lines(false, Some(true)).await,
        ["first", "  second", "last"],
    );
}

#[tokio::test]
async fn legacy_policies_default_off_and_instance_setting_survives_restart() {
    let legacy: ReplyPolicy =
        serde_json::from_str(r#"{"mode":"always","probability":0.5}"#).expect("legacy policy");
    assert!(!legacy.split_lines);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");
    let registry = InstanceRegistry::open(&path).await.expect("open catalog");
    let instance = registry
        .create(InstanceDraft {
            name: "saved-line-test".to_string(),
            reply_policy: Some(ReplyPolicy {
                split_lines: true,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("create instance");
    drop(registry);
    let reopened = InstanceRegistry::open(&path).await.expect("reopen catalog");
    let restored = reopened.get(&instance.id).await.expect("restored instance");
    assert!(restored.reply_policy.expect("stored policy").split_lines);
}
