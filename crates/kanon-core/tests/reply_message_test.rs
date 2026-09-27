//! Delivery receipts must retain the source event and never confuse admission with delivery.
use async_trait::async_trait;
use kanon_core::adapter::{AdapterError, EventIngress, PlatformAdapter};
use kanon_core::pipeline::dead_letter::DeadLetterWriter;
use kanon_core::{CoreApiService, PipelineEngine, Supervisor};
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::{DeliverMessageRequest, DeliverMessageResponse};
use std::{sync::Arc, time::Duration};
use tempfile::tempdir;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tonic::Request;

struct ControlledAdapter {
    seen: Arc<Mutex<Vec<DeliverMessageRequest>>>,
    release: Arc<Semaphore>,
    success: bool,
}

#[async_trait]
impl PlatformAdapter for ControlledAdapter {
    fn platform(&self) -> &str {
        "reply_test"
    }
    fn display_name(&self) -> &str {
        "Reply test"
    }
    async fn start(&self, _: EventIngress) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        self.seen.lock().await.push(request);
        let _permit = self.release.acquire().await.unwrap();
        Ok(DeliverMessageResponse {
            success: self.success,
            message_id: if self.success {
                "platform-message".into()
            } else {
                String::new()
            },
            error_message: if self.success {
                String::new()
            } else {
                "platform rejected reply".into()
            },
        })
    }
}

fn reply(id: &str) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: "reply_test".into(),
        channel_id: "group:1".into(),
        recipient_id: "sender".into(),
        event_id: id.into(),
        segments: vec![],
    }
}

#[tokio::test]
async fn receipt_waits_for_adapter_and_preserves_original_event() {
    check_delivery(true).await;
    check_delivery(false).await;
}

async fn check_delivery(success: bool) {
    let tmp = tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(tmp.path().into()), None));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(Semaphore::new(0));
    supervisor
        .adapters()
        .register(Arc::new(ControlledAdapter {
            seen: seen.clone(),
            release: release.clone(),
            success,
        }))
        .await
        .unwrap();
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_dead_letter(Arc::new(DeadLetterWriter::new(tmp.path().join("dead")))),
    );
    let dispatcher = engine.clone().start_outbound_dispatcher().unwrap();
    let (tx, _rx) = mpsc::channel(1);
    let api = CoreApiService::new(tx).with_outbound_sender(engine.outbound_sender());
    let call = tokio::spawn(async move {
        api.reply_message(Request::new(reply("original-platform-event")))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !call.is_finished(),
        "queue admission cannot acknowledge delivery"
    );
    assert_eq!(seen.lock().await[0].event_id, "original-platform-event");
    assert_eq!(seen.lock().await[0].recipient_id, "sender");
    release.add_permits(1);
    let response = call.await.unwrap().unwrap().into_inner();
    assert_eq!(response.success, success);
    if success {
        assert_eq!(response.message_id, "platform-message");
    } else {
        assert!(response.error_message.contains("platform rejected"));
    }
    dispatcher.abort();
}

#[tokio::test]
async fn rejects_missing_context_and_queue_backpressure() {
    let (tx, _rx) = mpsc::channel(1);
    let (out, mut pending) = mpsc::channel(1);
    let api = CoreApiService::new(tx).with_outbound_sender(out.clone());
    assert_eq!(
        api.reply_message(Request::new(reply("")))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    out.try_send(reply("already-queued").into()).unwrap();
    assert_eq!(
        api.reply_message(Request::new(reply("full")))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::ResourceExhausted
    );
    pending.recv().await.unwrap();
    drop(pending);
    assert_eq!(
        api.reply_message(Request::new(reply("closed")))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
}

#[tokio::test]
async fn canceled_waiter_does_not_send_a_queued_reply() {
    let tmp = tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(tmp.path().into()), None));
    let seen = Arc::new(Mutex::new(Vec::new()));
    supervisor
        .adapters()
        .register(Arc::new(ControlledAdapter {
            seen: seen.clone(),
            release: Arc::new(Semaphore::new(1)),
            success: true,
        }))
        .await
        .unwrap();
    let engine = Arc::new(PipelineEngine::new(supervisor));
    let sender = engine.outbound_sender();
    let (tx, _rx) = mpsc::channel(1);
    let api = CoreApiService::new(tx).with_outbound_sender(sender.clone());
    let call =
        tokio::spawn(async move { api.reply_message(Request::new(reply("canceled"))).await });
    while sender.capacity() == sender.max_capacity() {
        tokio::task::yield_now().await;
    }
    call.abort();
    let _ = call.await;
    let dispatcher = engine.clone().start_outbound_dispatcher().unwrap();
    // A normal follow-up in the same FIFO proves that the canceled item was skipped.
    let (tx, _rx) = mpsc::channel(1);
    let api = CoreApiService::new(tx).with_outbound_sender(sender);
    api.reply_message(Request::new(reply("following")))
        .await
        .unwrap();
    assert_eq!(seen.lock().await.len(), 1);
    assert_eq!(seen.lock().await[0].event_id, "following");
    dispatcher.abort();
}
