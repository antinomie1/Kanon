//! Session writers span model waits and streaming producers, and cannot be inherited by tools.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_llm::gateway::{ChatChunk, ChatChunkStream, ChatRequest, ChatResponse};
use kanon_llm::memory::InMemory;
use kanon_llm::{
    Agent, AgentError, AgentHook, BuiltinAgent, GatewayError, LlmProvider, MemoryError,
    SessionManager,
};
use tokio::sync::{Notify, mpsc};
use tokio_stream::StreamExt;

struct WaitingModel {
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl LlmProvider for WaitingModel {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ChatResponse {
            content: Some("answer".into()),
            ..Default::default()
        })
    }
}

/// A nested callback must not gain reentrant ownership of the outer turn's writer.
struct NestedMutation(Arc<SessionManager>);

#[async_trait]
impl AgentHook for NestedMutation {
    async fn on_user_message(
        &self,
        sid: &str,
        _: &mut kanon_llm::ChatMessage,
    ) -> Result<(), AgentError> {
        assert!(matches!(
            self.0.reset_session(sid).await,
            Err(MemoryError::Busy(_))
        ));
        Ok(())
    }
}

#[tokio::test]
async fn turn_excludes_reset_delete_and_second_turn_including_delegated_callbacks() {
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let model = Arc::new(WaitingModel {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let agent = Arc::new(
        BuiltinAgent::builder("writer", model.clone())
            .session_manager(sessions.clone())
            .hook(NestedMutation(sessions.clone()))
            .build(),
    );
    let writing = sessions.try_write("chat").unwrap();
    let task = tokio::spawn({
        let agent = agent.clone();
        async move { writing.scope(agent.run("chat", "hello", &[])).await }
    });
    model.entered.notified().await;
    assert!(matches!(
        sessions.reset_session("chat").await,
        Err(MemoryError::Busy(_))
    ));
    assert!(matches!(
        sessions.delete_session("chat").await,
        Err(MemoryError::Busy(_))
    ));
    assert!(matches!(
        agent.run("chat", "racing", &[]).await,
        Err(AgentError::Busy(_))
    ));
    assert!(sessions.try_write("other").is_ok());
    assert_eq!(memory.get_messages("chat").len(), 1);
    model.release.notify_one();
    task.await.unwrap().unwrap();
    sessions.reset_session("chat").await.unwrap();
    assert!(memory.get_messages("chat").is_empty());
}

struct StreamingModel(Mutex<Option<mpsc::Receiver<Result<ChatChunk, GatewayError>>>>);

#[async_trait]
impl LlmProvider for StreamingModel {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        unreachable!("stream only")
    }
    async fn chat_stream(&self, _: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            self.0.lock().unwrap().take().unwrap(),
        )))
    }
}

#[tokio::test]
async fn dropped_stream_keeps_writer_until_background_producer_finishes() {
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let (tx, rx) = mpsc::channel(4);
    let agent = BuiltinAgent::builder("stream", Arc::new(StreamingModel(Mutex::new(Some(rx)))))
        .session_manager(sessions.clone())
        .build();
    let mut stream = agent.run_stream("chat", "hello", &[]).await.unwrap();
    tx.send(Ok(ChatChunk {
        delta_text: "answer".into(),
        ..Default::default()
    }))
    .await
    .unwrap();
    stream.next().await.unwrap().unwrap();
    drop(stream);
    // The producer is still waiting for the model. Dropping SSE is not the write commit point.
    assert!(matches!(
        sessions.reset_session("chat").await,
        Err(MemoryError::Busy(_))
    ));
    tx.send(Ok(ChatChunk::done(Some("stop".into()))))
        .await
        .unwrap();
    let writer = tokio::time::timeout(Duration::from_secs(2), sessions.write("chat"))
        .await
        .unwrap();
    writer.scope(sessions.reset_session("chat")).await.unwrap();
    assert!(memory.get_messages("chat").is_empty());
}
