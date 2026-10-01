//! A model turn that fails tells the sender once and is never re-run; a turn nobody asked for
//! fails quietly.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    META_BOT_MENTIONED, META_CONVERSATION_KIND, ReplyMode, ReplyPolicy, ReplyPolicyStore,
};
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::InMemory;
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{GatewayError, LlmProvider};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{IngestEventRequest, PipelineEventRequest};
use tokio::sync::mpsc;

const PLATFORM: &str = "failure-test";

/// Provider that rejects every request, the way it rejects a session holding an expired image.
#[derive(Default)]
struct Rejecting {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmProvider for Rejecting {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GatewayError::ApiStatus {
            status: 400,
            message: "failed to download or process media content".into(),
        })
    }
}

/// A message; `group` makes it a group message, mentioning the bot or not.
fn message(id: &str, group: Option<bool>) -> IngestEventRequest {
    let metadata = group.map(|mentioned| prost_types::Struct {
        fields: [
            (META_CONVERSATION_KIND, Kind::StringValue("group".into())),
            (META_BOT_MENTIONED, Kind::BoolValue(mentioned)),
        ]
        .into_iter()
        .map(|(key, kind)| (key.to_string(), prost_types::Value { kind: Some(kind) }))
        .collect(),
    });
    IngestEventRequest {
        platform: PLATFORM.to_string(),
        event: Some(PipelineEventRequest {
            event_id: id.to_string(),
            platform: PLATFORM.to_string(),
            channel_id: "conversation".to_string(),
            sender_id: "user".to_string(),
            raw_text: "hello".to_string(),
            metadata,
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn a_failed_turn_is_reported_once_to_whoever_asked_and_never_retried() {
    let dir = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let (delivered_tx, mut delivered) = mpsc::channel(8);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared(PLATFORM, delivered_tx))
        .await
        .expect("register adapter");
    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(InstanceDraft {
            name: "failure-test".to_string(),
            enabled: true,
            adapters: vec![PLATFORM.to_string()],
            ..Default::default()
        })
        .await
        .expect("create instance");
    let provider = Arc::new(Rejecting::default());
    let agent = Arc::new(
        BuiltinAgent::builder("failure-test", provider.clone())
            .memory(Arc::new(InMemory::new()))
            .model("test-model")
            .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            // Answer every group message, so an unaddressed one reaches the model too.
            .with_reply_policy(Arc::new(ReplyPolicyStore::new(ReplyPolicy {
                mode: ReplyMode::Always,
                ..Default::default()
            }))),
    );
    let (inbound, inbound_rx) = mpsc::channel(8);
    let worker = engine.clone().start_worker(inbound_rx);
    let dispatcher = engine.clone().start_outbound_dispatcher();

    for (id, group) in [
        ("private", None),
        ("unaddressed", Some(false)),
        ("mentioned", Some(true)),
    ] {
        inbound.send(message(id, group)).await.unwrap();
    }
    drop(inbound);
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .expect("worker finishes")
        .expect("worker succeeds");
    engine.drain(tokio::spawn(async {}), dispatcher).await;

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "one request per message: a failed turn is not retried"
    );
    let mut notices = Vec::new();
    while let Ok(delivery) = delivered.try_recv() {
        let [segment] = &delivery.segments[..] else {
            panic!("expected one segment, got {:?}", delivery.segments);
        };
        let Some(Segment::Text(text)) = &segment.segment else {
            panic!("expected text, got {segment:?}");
        };
        notices.push((delivery.event_id.clone(), text.content.clone()));
    }
    let notice = "这次没能回复：模型服务返回错误（HTTP 400）。不会自动重试，可以稍后再发。";
    assert_eq!(
        notices,
        [
            ("private".to_string(), notice.to_string()),
            ("mentioned".to_string(), notice.to_string()),
        ],
        "the unaddressed group message fails quietly"
    );
}
