//! Line formatting and actual FIFO delivery of model replies.

mod common;
#[path = "../src/pipeline/reply.rs"]
mod reply;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::engine::OutboundMessage;
use kanon_core::pipeline::{
    DEFAULT_OUTBOUND_QUEUE_CAPACITY, DeadLetterRecord, DeadLetterWriter, PipelineEngine,
};
use kanon_core::supervisor::Supervisor;
use kanon_core::{AdapterError, PlatformAdapter, ReplyPolicy, ReplyPolicyStore};
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{GatewayError, LlmProvider};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, FileSegment, ImageSegment, IngestEventRequest,
    MessageSegment, PipelineEventRequest, ReplySegment, TextSegment, VideoSegment,
};
use tokio::sync::{Semaphore, mpsc};

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
        reply::split_reply_lines(
            &[text("\nfirst\r\n\r\n \t\n\u{3000}\n  第二行  \nlast\n")],
            usize::MAX
        ),
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
        reply::split_reply_lines(
            &[quote(), text("first\n\nlast"), image(), image()],
            usize::MAX
        ),
        vec![
            vec![quote(), text("first")],
            vec![text("last"), image(), image()]
        ],
    );
}

#[test]
fn blank_text_and_quotes_are_not_sent_but_image_only_replies_survive() {
    assert!(reply::split_reply_lines(&[quote(), text("\n \t\r\n")], usize::MAX).is_empty());
    assert_eq!(
        reply::split_reply_lines(&[quote(), text("\n \t\r\n"), image()], usize::MAX),
        vec![vec![quote(), image()]],
    );
}

/// Provider with one synthetic multiline final answer.
struct MultilineProvider(String);

#[async_trait]
impl LlmProvider for MultilineProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            content: Some(self.0.clone()),
            reasoning_content: Some("private thought\n\n  draft".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Runs the real inbound worker and outbound dispatcher over an in-memory provider/adapter.
async fn delivered_lines(
    node_split: bool,
    instance_split: Option<bool>,
    send_reasoning: bool,
) -> Vec<String> {
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
        .create(
            InstanceDraft {
                name: "line-test".to_string(),
                enabled: true,
                adapters: vec!["line-test".to_string()],
                reply_policy: instance_split.map(|split_lines| ReplyPolicy {
                    split_lines,
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance");
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
            "line-test",
            Arc::new(MultilineProvider(
                "first\r\n\r\n \t\n  second\nlast".to_string(),
            )),
        )
        .memory(memory.clone())
        .model("test-model")
        .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy {
                split_lines: node_split,
                send_reasoning,
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
        .get_messages(
            &instance
                .conversation_session_id("chat:9:line-test:private:u:12:conversation:6:sender"),
        )
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
    assert_eq!(
        history[1].reasoning_content.as_deref(),
        Some("private thought\n\n  draft")
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
        delivered_lines(true, None, false).await,
        ["first", "  second", "last"]
    );
}

#[tokio::test]
async fn disabled_setting_keeps_one_message_and_instance_overrides_work_both_ways() {
    let original = ["first\r\n\r\n \t\n  second\nlast"];
    assert_eq!(delivered_lines(false, None, false).await, original);
    assert_eq!(delivered_lines(true, Some(false), false).await, original);
    assert_eq!(
        delivered_lines(false, Some(true), false).await,
        ["first", "  second", "last"],
    );
}

#[tokio::test]
async fn opted_in_reasoning_is_split_before_the_answer_without_blank_messages() {
    assert_eq!(
        delivered_lines(true, None, true).await,
        ["private thought", "  draft", "first", "  second", "last"],
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
        .create(
            InstanceDraft {
                name: "saved-line-test".to_string(),
                reply_policy: Some(ReplyPolicy {
                    split_lines: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance");
    drop(registry);
    let reopened = InstanceRegistry::open(&path).await.expect("reopen catalog");
    let restored = reopened.get(&instance.id).await.expect("restored instance");
    assert!(restored.reply_policy.expect("stored policy").split_lines);
}

#[test]
fn capped_replies_keep_every_nonblank_line_and_each_attachment_once() {
    let segments = [
        quote(),
        text("first\n\nsecond\r\n  third\n \nlast"),
        image(),
    ];
    assert_eq!(
        reply::split_reply_lines(&segments, 2),
        vec![
            vec![quote(), text("first")],
            vec![text("second\n  third\nlast"), image()]
        ],
    );
    for limit in [0, 1] {
        assert_eq!(
            reply::split_reply_lines(&segments, limit),
            vec![vec![quote(), text("first\nsecond\n  third\nlast"), image()]],
        );
    }
    assert_eq!(reply::split_reply_lines(&[image()], 1), vec![vec![image()]]);
    assert!(reply::split_reply_lines(&[quote(), text(" \n\n")], 1).is_empty());
}

/// Adapter with deterministic backpressure, a split budget, and an optional failed part.
struct BatchAdapter {
    attempted: mpsc::UnboundedSender<DeliverMessageRequest>,
    release: Arc<Semaphore>,
    limit: usize,
    fail_on: Option<String>,
}

#[async_trait]
impl PlatformAdapter for BatchAdapter {
    fn platform(&self) -> &str {
        "line-test"
    }

    fn reply_message_limit(&self, _: &DeliverMessageRequest) -> usize {
        self.limit
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        self.attempted
            .send(request.clone())
            .expect("attempt receiver open");
        self.release
            .acquire()
            .await
            .expect("release semaphore open")
            .forget();
        if self.fail_on.as_deref() == Some(request_text(&request).as_str()) {
            return Err(AdapterError::Delivery {
                platform: self.platform().to_string(),
                reason: "synthetic delivery failure".to_string(),
            });
        }
        Ok(DeliverMessageResponse {
            success: true,
            ..Default::default()
        })
    }
}

/// Reassembles only text segments for assertions about content and order.
fn request_text(request: &DeliverMessageRequest) -> String {
    request
        .segments
        .iter()
        .filter_map(|s| match &s.segment {
            Some(Segment::Text(t)) => Some(t.content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reply fixture retaining its source event.
fn batch_request(content: &str, event_id: &str) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: "line-test".to_string(),
        channel_id: "conversation".to_string(),
        recipient_id: "sender".to_string(),
        event_id: event_id.to_string(),
        segments: vec![text(content)],
    }
}

/// Builds the real model pipeline with isolated dead-letter storage.
async fn batch_engine(
    dir: &std::path::Path,
    supervisor: Arc<Supervisor>,
    content: String,
) -> Arc<PipelineEngine> {
    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(
            InstanceDraft {
                name: "line-test".to_string(),
                adapters: vec!["line-test".to_string()],
                enabled: true,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance");
    let agent = Arc::new(
        BuiltinAgent::builder("line-test", Arc::new(MultilineProvider(content)))
            .model("test-model")
            .build(),
    );
    Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy {
                split_lines: true,
                ..Default::default()
            })))
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_dead_letter(Arc::new(DeadLetterWriter::new(dir.join("dead_letter")))),
    )
}

/// Completes one inbound event without requiring the outbound dispatcher to run.
async fn generate_reply(engine: &Arc<PipelineEngine>) {
    let (tx, rx) = mpsc::channel(1);
    tx.send(IngestEventRequest {
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
    drop(tx);
    tokio::time::timeout(Duration::from_secs(3), engine.clone().start_worker(rx))
        .await
        .expect("worker finishes")
        .expect("worker succeeds");
}

#[tokio::test]
async fn long_answer_uses_one_queue_slot_and_slow_platform_keeps_the_full_batch_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().into()), None));
    let (attempted_tx, mut attempted_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    supervisor
        .adapters()
        .register(Arc::new(BatchAdapter {
            attempted: attempted_tx,
            release: release.clone(),
            limit: usize::MAX,
            fail_on: None,
        }))
        .await
        .unwrap();
    let (other_tx, mut other_rx) = mpsc::channel(1);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared("other", other_tx))
        .await
        .unwrap();
    let lines: Vec<_> = (0..DEFAULT_OUTBOUND_QUEUE_CAPACITY + 10)
        .map(|n| format!("line-{n}"))
        .collect();
    let engine = batch_engine(dir.path(), supervisor, lines.join("\n\n")).await;
    generate_reply(&engine).await;
    assert_eq!(
        engine.outbound_sender().capacity(),
        DEFAULT_OUTBOUND_QUEUE_CAPACITY - 1
    );
    let dispatcher = engine.clone().start_outbound_dispatcher();
    let first = tokio::time::timeout(Duration::from_secs(3), attempted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request_text(&first), lines[0]);
    engine
        .outbound_sender()
        .send(batch_request("next-answer", "next-event").into())
        .await
        .unwrap();
    let mut other = batch_request("other-answer", "other-event");
    other.platform = "other".to_string();
    engine.outbound_sender().send(other.into()).await.unwrap();
    assert_eq!(
        request_text(
            &tokio::time::timeout(Duration::from_secs(3), other_rx.recv())
                .await
                .unwrap()
                .unwrap()
        ),
        "other-answer"
    );
    assert!(
        attempted_rx.try_recv().is_err(),
        "slow platform is still blocked on the first part"
    );
    release.add_permits(lines.len() + 1);
    for expected in &lines[1..] {
        let request = tokio::time::timeout(Duration::from_secs(3), attempted_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request_text(&request), *expected);
        assert_eq!(request.event_id, "source-event");
    }
    let next = tokio::time::timeout(Duration::from_secs(3), attempted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request_text(&next), "next-answer");
    assert_eq!(next.event_id, "next-event");
    engine.drain(tokio::spawn(async {}), dispatcher).await;
    assert!(!dir.path().join("dead_letter").exists());
}

#[tokio::test]
async fn adapter_budget_is_applied_after_dequeueing() {
    let dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().into()), None));
    let (attempted, mut rx) = mpsc::unbounded_channel();
    supervisor
        .adapters()
        .register(Arc::new(BatchAdapter {
            attempted,
            release: Arc::new(Semaphore::new(2)),
            limit: 2,
            fail_on: None,
        }))
        .await
        .unwrap();
    let engine = batch_engine(
        dir.path(),
        supervisor,
        "first\n\nsecond\nthird\nlast".to_string(),
    )
    .await;
    generate_reply(&engine).await;
    let dispatcher = engine.clone().start_outbound_dispatcher();
    engine.drain(tokio::spawn(async {}), dispatcher).await;
    assert_eq!(request_text(&rx.try_recv().unwrap()), "first");
    assert_eq!(request_text(&rx.try_recv().unwrap()), "second\nthird\nlast");
    assert!(rx.try_recv().is_err());
}

/// Reads the durable unsent suffix after the dispatcher has stopped.
fn dead_letter_parts(dir: &std::path::Path) -> Vec<DeadLetterRecord> {
    std::fs::read_dir(dir.join("dead_letter"))
        .unwrap()
        .flat_map(|entry| {
            std::fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<DeadLetterRecord>>()
        })
        .collect()
}

#[tokio::test]
async fn failed_or_shutdown_batch_records_only_the_undelivered_suffix() {
    for shutdown in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(Supervisor::new(Some(dir.path().into()), None));
        let (attempted, mut rx) = mpsc::unbounded_channel();
        supervisor
            .adapters()
            .register(Arc::new(BatchAdapter {
                attempted,
                release: Arc::new(Semaphore::new(if shutdown { 1 } else { 4 })),
                limit: usize::MAX,
                fail_on: (!shutdown).then(|| "second".to_string()),
            }))
            .await
            .unwrap();
        let engine = batch_engine(dir.path(), supervisor, String::new()).await;
        let dispatcher = engine.clone().start_outbound_dispatcher();
        let (receipt, result) = tokio::sync::oneshot::channel();
        engine
            .outbound_sender()
            .send(OutboundMessage {
                request: batch_request("first\nsecond\nthird\nlast", "source-event"),
                split_lines: true,
                receipt: Some(receipt),
            })
            .await
            .unwrap();
        for expected in ["first", "second"] {
            assert_eq!(
                request_text(
                    &tokio::time::timeout(Duration::from_secs(3), rx.recv())
                        .await
                        .unwrap()
                        .unwrap()
                ),
                expected
            );
        }
        engine.drain(tokio::spawn(async {}), dispatcher).await;
        assert!(!result.await.unwrap().success);
        let records = dead_letter_parts(dir.path());
        let parts: Vec<_> = records
            .iter()
            .map(|r| r.segments[0]["content"].as_str().unwrap())
            .collect();
        assert_eq!(parts, ["second", "third", "last"]);
        assert!(records.iter().all(|r| r.event_id == "source-event"));
        assert!(
            rx.try_recv().is_err(),
            "failed suffix is never attempted or retried"
        );
        assert!(records[0].reason.contains(if shutdown {
            "shut down"
        } else {
            "synthetic delivery failure"
        }));
    }
}

#[test]
fn capped_reply_keeps_video_and_file_on_the_last_message_without_duplication() {
    let video = MessageSegment {
        segment: Some(Segment::Video(VideoSegment {
            source: Some(kanon_proto::v1::video_segment::Source::Url(
                "https://example.invalid/clip.mp4".to_string(),
            )),
            ..Default::default()
        })),
    };
    let file = MessageSegment {
        segment: Some(Segment::File(FileSegment {
            name: "report.pdf".to_string(),
            source: Some(kanon_proto::v1::file_segment::Source::Url(
                "https://example.invalid/report.pdf".to_string(),
            )),
            ..Default::default()
        })),
    };
    assert_eq!(
        reply::split_reply_lines(
            &[
                quote(),
                text("first\n\nsecond\nthird\nlast"),
                video.clone(),
                file.clone(),
            ],
            2,
        ),
        vec![
            vec![quote(), text("first")],
            vec![text("second\nthird\nlast"), video, file],
        ],
    );
}
