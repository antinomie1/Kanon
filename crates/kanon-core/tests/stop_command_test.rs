//! `/stop` reaches a turn that is still running: the worker reads it past the turn it is busy
//! with, an administrator's `/stop` ends that turn without a reply, and the chat goes on.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{CommandPolicy, CommandPolicyStore};
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{GatewayError, LlmProvider, SessionManager};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, IngestEventRequest, PipelineEventRequest};
use tokio::sync::{Notify, mpsc};

const PLATFORM: &str = "stop-test";

/// Model that never answers `work`, the way a turn stuck in a loop never does, and answers
/// anything else at once.
struct Stuck {
    waiting: Arc<Notify>,
}

#[async_trait]
impl LlmProvider for Stuck {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let last = request.messages.last().and_then(|m| m.content.as_deref());
        if last.is_some_and(|text| text.starts_with("work")) {
            self.waiting.notify_one();
            return std::future::pending().await;
        }
        Ok(ChatResponse {
            content: Some("done".into()),
            finish_reason: Some("stop".into()),
            ..ChatResponse::default()
        })
    }
}

fn message(id: &str, sender: &str, text: &str) -> IngestEventRequest {
    IngestEventRequest {
        platform: PLATFORM.to_string(),
        event: Some(PipelineEventRequest {
            event_id: id.to_string(),
            platform: PLATFORM.to_string(),
            channel_id: "conversation".to_string(),
            sender_id: sender.to_string(),
            raw_text: text.to_string(),
            ..Default::default()
        }),
    }
}

/// The text of the next delivery, failing the test if none comes.
async fn next_text(delivered: &mut mpsc::Receiver<DeliverMessageRequest>) -> String {
    let delivery = tokio::time::timeout(Duration::from_secs(3), delivered.recv())
        .await
        .expect("a delivery arrives")
        .expect("adapter channel open");
    match &delivery.segments[..] {
        [segment] => match &segment.segment {
            Some(Segment::Text(text)) => text.content.clone(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected one segment, got {other:?}"),
    }
}

#[tokio::test]
async fn an_admins_stop_ends_the_running_turn_and_the_chat_goes_on() {
    let dir = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let (delivered_tx, mut delivered) = mpsc::channel(8);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared(PLATFORM, delivered_tx))
        .await
        .expect("register adapter");
    let registry = Arc::new(InstanceRegistry::in_memory());
    let instance = registry
        .create(
            InstanceDraft {
                name: "stop-test".to_string(),
                enabled: true,
                adapters: vec![PLATFORM.to_string()],
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance");
    let waiting = Arc::new(Notify::new());
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
            "stop-test",
            Arc::new(Stuck {
                waiting: waiting.clone(),
            }),
        )
        .memory(memory.clone())
        .model("test-model")
        .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_command_policy(Arc::new(CommandPolicyStore::new(CommandPolicy {
                admins: vec![format!("{PLATFORM}:admin")],
                ..Default::default()
            }))),
    );
    let (inbound, inbound_rx) = mpsc::channel(8);
    let worker = engine.clone().start_worker(inbound_rx);
    let dispatcher = engine.clone().start_outbound_dispatcher();

    inbound.send(message("e1", "user", "work")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), waiting.notified())
        .await
        .expect("the turn reaches the model");
    // Queued behind the stuck turn: answered only after it ends.
    inbound.send(message("e2", "user", "hello")).await.unwrap();

    // Stopping cuts off every chat of the instance, so members may not.
    inbound.send(message("e3", "user", "/stop")).await.unwrap();
    assert_eq!(
        next_text(&mut delivered).await,
        format!("/stop 仅限管理员使用（你的 ID：{PLATFORM}:user）")
    );

    inbound.send(message("e4", "admin", "/stop")).await.unwrap();
    assert_eq!(
        next_text(&mut delivered).await,
        "已停止 1 个正在运行的任务。"
    );
    // The stopped turn sends nothing; the next delivery is the answer to the queued message.
    assert_eq!(next_text(&mut delivered).await, "done");

    inbound.send(message("e5", "admin", "/stop")).await.unwrap();
    assert_eq!(next_text(&mut delivered).await, "当前没有正在运行的任务。");

    drop(inbound);
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .expect("worker finishes")
        .expect("worker succeeds");
    engine.drain(tokio::spawn(async {}), dispatcher).await;

    let history = memory
        .get_messages(
            &instance.conversation_session_id("chat:9:stop-test:private:u:12:conversation:4:user"),
        )
        .await
        .expect("stored history");
    let texts: Vec<Option<&str>> = history
        .iter()
        .map(|message| message.content.as_deref())
        .collect();
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(
        texts[0].is_some_and(|text| text.contains("work")),
        "{texts:?}"
    );
    // The stopped turn is closed, so the model answering `hello` does not take `work` up again.
    assert!(
        texts[1].is_some_and(|text| text.contains("stopped by the user")),
        "{texts:?}"
    );
    assert!(
        texts[2].is_some_and(|text| text.contains("hello")),
        "{texts:?}"
    );
    assert_eq!(texts[3], Some("done"));
}

/// A turn waiting for a compaction or console writer is canceled before it touches memory.
#[tokio::test]
async fn stop_cancels_a_turn_waiting_for_the_session_writer() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(InstanceRegistry::in_memory());
    let instance = registry
        .create(
            InstanceDraft {
                name: "waiting-writer".into(),
                enabled: true,
                adapters: vec![PLATFORM.into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let agent = Arc::new(
        BuiltinAgent::builder(
            "waiting-writer",
            Arc::new(Stuck {
                waiting: Arc::new(Notify::new()),
            }),
        )
        .session_manager(sessions.clone())
        .build(),
    );
    let engine = PipelineEngine::new(Arc::new(Supervisor::new(
        Some(dir.path().to_path_buf()),
        None,
    )))
    .with_instances(registry)
    .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
    .with_command_policy(Arc::new(CommandPolicyStore::new(CommandPolicy {
        admins: vec![format!("{PLATFORM}:admin")],
        ..Default::default()
    })));
    let session_id =
        instance.conversation_session_id("chat:9:stop-test:private:u:12:conversation:4:user");
    let writer = sessions.try_write(&session_id).unwrap();
    let mut turn =
        Box::pin(engine.process_event(message("waiting", "user", "work").event.unwrap()));
    // With no plugins or media, the writer is the first pending operation. Poll directly so
    // the stop command runs after registration without depending on sleeps or task scheduling.
    let polled = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(turn.as_mut(), cx))
    })
    .await;
    assert!(polled.is_pending());
    let stopped = engine
        .process_event(message("stop", "admin", "/stop").event.unwrap())
        .await;
    let PipelineResult::BuiltinReplied { replies, .. } = stopped else {
        panic!("expected a stop acknowledgement");
    };
    assert!(matches!(
        &replies[0].segment,
        Some(Segment::Text(text)) if text.content == "已停止 1 个正在运行的任务。"
    ));
    // Make both the writer and stop signal ready: cancellation must win this boundary too.
    drop(writer);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), turn)
            .await
            .expect("the stopped turn does not enter the stuck model"),
        PipelineResult::Passed(_)
    ));
    assert!(memory.get_messages(&session_id).is_empty());
    assert!(sessions.try_write(&session_id).is_ok());
    assert!(matches!(
        engine
            .process_event(message("next", "user", "hello").event.unwrap())
            .await,
        PipelineResult::LlmReplied { .. }
    ));
}

/// Saturated lanes keep bounded read-ahead and still admit an administrator's stop command.
#[tokio::test]
async fn stop_bypasses_a_full_waiting_queue_and_concurrency_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let (delivered_tx, mut delivered) = mpsc::channel(8);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared(PLATFORM, delivered_tx))
        .await
        .unwrap();
    let registry = Arc::new(InstanceRegistry::in_memory());
    registry
        .create(
            InstanceDraft {
                name: "bounded".into(),
                enabled: true,
                adapters: vec![PLATFORM.into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let waiting = Arc::new(Notify::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
            "bounded",
            Arc::new(Stuck {
                waiting: waiting.clone(),
            }),
        )
        .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_instances(registry)
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_dead_letter(Arc::new(kanon_core::pipeline::DeadLetterWriter::new(
                dir.path().join("dead_letter"),
            )))
            .with_command_policy(Arc::new(CommandPolicyStore::new(CommandPolicy {
                admins: vec![format!("{PLATFORM}:admin")],
                ..Default::default()
            }))),
    );
    let (tx, rx) = mpsc::channel(2);
    let worker = engine.clone().start_worker(rx);
    let dispatcher = engine.clone().start_outbound_dispatcher();
    let limit = kanon_core::pipeline::engine::MAX_CONCURRENT_CHATS;
    for index in 0..limit {
        tx.send(message(
            &format!("active-{index}"),
            &format!("user-{index}"),
            "work",
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), waiting.notified())
            .await
            .unwrap();
    }
    // Two wait; two are explicitly dead-lettered. No extra provider call may start.
    for index in 0..4 {
        tx.send(message(
            &format!("queued-{index}"),
            &format!("queued-{index}"),
            "work",
        ))
        .await
        .unwrap();
    }
    tx.send(message("stop", "admin", "/stop")).await.unwrap();
    assert_eq!(
        next_text(&mut delivered).await,
        format!("已停止 {limit} 个正在运行的任务。")
    );
    let mut ids = Vec::new();
    for file in std::fs::read_dir(dir.path().join("dead_letter")).unwrap() {
        for line in std::fs::read_to_string(file.unwrap().path())
            .unwrap()
            .lines()
        {
            let record: kanon_core::pipeline::DeadLetterRecord =
                serde_json::from_str(line).unwrap();
            assert!(record.reason.contains("waiting queue is full"));
            ids.push(record.event_id);
        }
    }
    assert_eq!(ids, ["queued-2", "queued-3"]);
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    engine.drain(tokio::spawn(async {}), dispatcher).await;
}
