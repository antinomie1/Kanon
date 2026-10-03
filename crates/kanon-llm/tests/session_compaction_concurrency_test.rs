//! Embedded agents keep one compaction writer while any owner or waiter still needs it.

use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::{Agent, BuiltinAgent, COMPACTION_INSTRUCTION, GatewayError, LlmProvider};
use tokio::sync::{Notify, Semaphore};

struct GatedSummaryProvider {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
}

#[async_trait]
impl LlmProvider for GatedSummaryProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        assert_eq!(
            request.messages.last().unwrap().content.as_deref(),
            Some(COMPACTION_INSTRUCTION)
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        Ok(ChatResponse {
            content: Some(format!("summary-{call}")),
            finish_reason: Some("stop".into()),
            ..Default::default()
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_compactions_serialize_and_do_not_fold_the_same_history_twice() {
    let memory = Arc::new(InMemory::new());
    let provider = Arc::new(GatedSummaryProvider {
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let agent = BuiltinAgent::builder("embedded", provider.clone())
        .memory(memory.clone())
        .build();
    assert!(agent.session_manager().is_none());

    // Repeat after the first pair of owners has gone, exercising a fresh writer for an old key.
    for generation in 1..=2 {
        for _ in 0..2 {
            memory.push_message("session", ChatMessage::user("question"));
            memory.push_message("session", ChatMessage::assistant("answer"));
        }
        let first_agent = agent.clone();
        let first = tokio::spawn(async move { first_agent.compact_session("session", &[]).await });
        tokio::time::timeout(Duration::from_secs(2), provider.entered.notified())
            .await
            .expect("first compaction should reach the provider");

        let mut second = Box::pin(agent.compact_session("session", &[]));
        // Poll the second invocation while the first is inside the provider. Waiting only for a
        // spawned task to start would not prove that it actually attempted to acquire the lock.
        poll_fn(|cx| {
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            generation,
            "the waiter must not summarize the same snapshot concurrently"
        );

        provider.release.add_permits(1);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), first)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        assert!(
            !tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .unwrap()
                .unwrap(),
            "the waiter must read the already compacted history and skip it"
        );
        let snapshot = memory.snapshot("session").await.unwrap();
        assert_eq!(snapshot.summary, Some(format!("summary-{generation}")));
        assert!(snapshot.messages.is_empty());
        assert_eq!(provider.calls.load(Ordering::SeqCst), generation);
    }
}
