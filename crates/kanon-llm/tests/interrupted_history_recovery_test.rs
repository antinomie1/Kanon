//! Persisted orphan tool calls are closed before a restarted session accepts its next user turn.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse, Role, ToolCall};
use kanon_llm::{
    Agent, BuiltinAgent, GatewayError, LlmProvider, SessionManager, SqliteMemory,
    SqliteSessionStore,
};
use serde_json::json;
use tokio_stream::StreamExt;

#[derive(Default)]
struct RecordingProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(ChatResponse {
            content: Some("continued".to_string()),
            finish_reason: Some("stop".to_string()),
            ..Default::default()
        })
    }
}

fn open(path: &Path) -> Arc<SessionManager> {
    Arc::new(
        SessionManager::new(Arc::new(SqliteMemory::open(path).unwrap()))
            .with_store(Arc::new(SqliteSessionStore::open(path).unwrap()))
            .unwrap(),
    )
}

async fn seed_interrupted_history(path: &Path, completed: usize) -> Vec<ChatMessage> {
    let sessions = open(path);
    sessions.get_or_create("session");
    let mut history = vec![
        ChatMessage::user("older question"),
        ChatMessage::assistant("older answer"),
        ChatMessage::user("interrupted request"),
        ChatMessage::assistant_tool_calls(
            (0..2)
                .map(|index| ToolCall {
                    id: format!("call-{index}"),
                    name: "external_action".to_string(),
                    arguments: json!({"index": index}),
                })
                .collect(),
            None,
        ),
    ];
    for index in 0..completed {
        history.push(ChatMessage::tool_response(
            format!("call-{index}"),
            "already completed",
        ));
    }
    sessions
        .memory()
        .extend_messages("session", history.clone())
        .await
        .unwrap();
    history
}

#[tokio::test]
async fn restarting_closes_only_missing_results_before_normal_or_streaming_turns() {
    for completed in [0, 1] {
        for streaming in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sessions.db");
            let prefix = seed_interrupted_history(&path, completed).await;
            // All previous database handles were dropped; recovery must use the persisted log.
            let sessions = open(&path);
            let provider = Arc::new(RecordingProvider::default());
            let agent = BuiltinAgent::builder("recovery", provider.clone())
                .session_manager(sessions.clone())
                .compaction(None)
                .build();
            if streaming {
                let mut stream = agent
                    .run_stream("session", "new request", &[])
                    .await
                    .unwrap();
                while let Some(chunk) = stream.next().await {
                    chunk.unwrap();
                }
            } else {
                agent.run("session", "new request", &[]).await.unwrap();
            }

            let request = provider.requests.lock().unwrap()[0].clone();
            assert_eq!(&request.messages[..prefix.len()], prefix.as_slice());
            let pending = 2 - completed;
            for index in 0..pending {
                let recovered = &request.messages[prefix.len() + index];
                assert_eq!(recovered.role, Role::Tool);
                assert_eq!(
                    recovered.tool_call_id.as_deref(),
                    Some(format!("call-{}", index + completed).as_str())
                );
            }
            let closing = &request.messages[prefix.len() + pending];
            assert_eq!(closing.role, Role::Assistant);
            assert!(closing.content.as_deref().unwrap().contains("interrupted"));
            assert_eq!(
                request.messages.last(),
                Some(&ChatMessage::user("new request"))
            );
            assert_eq!(request.messages.len(), prefix.len() + pending + 2);

            let before_next = sessions.memory().snapshot("session").await.unwrap();
            agent.run("session", "another request", &[]).await.unwrap();
            let after_next = sessions.memory().snapshot("session").await.unwrap();
            assert_eq!(
                &after_next.messages[..before_next.messages.len()],
                before_next.messages.as_slice()
            );
            assert_eq!(after_next.messages.len(), before_next.messages.len() + 2);
            drop(agent);
            drop(sessions);
            assert_eq!(
                open(&path).memory().snapshot("session").await.unwrap(),
                after_next
            );
        }
    }
}

#[tokio::test]
async fn failed_recovery_keeps_the_interrupted_log_and_never_appends_the_new_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let prefix = seed_interrupted_history(&path, 0).await;
    let control = rusqlite::Connection::open(&path).unwrap();
    control
        .execute_batch(
            "CREATE TRIGGER reject_closing BEFORE INSERT ON messages
         WHEN NEW.role = 'assistant'
         BEGIN SELECT RAISE(ABORT, 'closing write failed'); END;",
        )
        .unwrap();
    let sessions = open(&path);
    let provider = Arc::new(RecordingProvider::default());
    let agent = BuiltinAgent::builder("recovery", provider.clone())
        .session_manager(sessions.clone())
        .compaction(None)
        .build();

    let error = agent.run("session", "new request", &[]).await.unwrap_err();
    assert!(
        error.to_string().contains("closing write failed"),
        "{error}"
    );
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(
        sessions.memory().get_messages("session").await.unwrap(),
        prefix
    );
    assert_eq!(
        open(&path).memory().get_messages("session").await.unwrap(),
        prefix
    );

    control
        .execute_batch("DROP TRIGGER reject_closing;")
        .unwrap();
    agent
        .run("session", "retry after storage repair", &[])
        .await
        .unwrap();
    let history = sessions.memory().get_messages("session").await.unwrap();
    assert_eq!(&history[..prefix.len()], prefix.as_slice());
    assert_eq!(
        history
            .iter()
            .filter(|message| message.role == Role::Tool)
            .count(),
        2
    );
    assert_eq!(
        history[history.len() - 2],
        ChatMessage::user("retry after storage repair")
    );
}
