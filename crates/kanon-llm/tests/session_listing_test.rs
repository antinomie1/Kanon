//! Listing a chat's sessions by key prefix and deleting whole sessions, on both memory backends.
//!
//! Conversations of one chat share a key prefix (`instance:<id>:<chat>#`), so the listing is what
//! `/ls` is built on; deleting a whole session is the only removal besides compaction and must take
//! the history, the summary and the session record with it.

use std::sync::Arc;

use kanon_llm::error::MemoryError;
use kanon_llm::gateway::types::{ChatMessage, ToolCall};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::session::{SessionManager, SessionMetadata, SessionStore};
use kanon_llm::{SqliteMemory, SqliteSessionStore};

/// One complete turn with a tool call in the middle, as the agent stores it.
fn turn(question: &str, answer: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::user(question),
        ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "call-1".to_string(),
                name: "lookup".to_string(),
                arguments: serde_json::json!({}),
            }],
            None,
        ),
        ChatMessage::tool_response("call-1", "result"),
        ChatMessage::assistant(answer),
    ]
}

async fn listing_contract(memory: &dyn Memory) {
    memory
        .extend_messages("instance:a:chat#0", turn("first question", "first answer"))
        .await
        .unwrap();
    memory
        .extend_messages("instance:a:chat#1", turn("second", "answer"))
        .await
        .unwrap();
    // Another chat of the same bot, and the same chat key under another bot.
    memory
        .push_message("instance:a:chat:other#0", ChatMessage::user("elsewhere"))
        .await
        .unwrap();
    memory
        .push_message("instance:b:chat#0", ChatMessage::user("another bot"))
        .await
        .unwrap();

    let listed = memory.list_sessions("instance:a:chat#").await.unwrap();
    let keys: Vec<&str> = listed.iter().map(|s| s.session_key.as_str()).collect();
    assert_eq!(keys, ["instance:a:chat#0", "instance:a:chat#1"]);
    // Tool results are not conversation messages; the tool-calling assistant message is.
    assert_eq!(listed[0].message_count, 3);
    assert_eq!(
        listed[0].first_user_message.as_deref(),
        Some("first question")
    );

    // A compacted session reports what is left, not what was summarized.
    memory
        .compact_history("instance:a:chat#0", 4, "summary".to_string())
        .await
        .unwrap();
    let listed = memory.list_sessions("instance:a:chat#0").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].message_count, 0);
    assert_eq!(listed[0].first_user_message, None);
}

#[tokio::test]
async fn in_memory_lists_sessions_by_prefix() {
    listing_contract(&InMemory::new()).await;
}

#[tokio::test]
async fn sqlite_lists_sessions_by_prefix() {
    let memory = SqliteMemory::open_in_memory().unwrap();
    listing_contract(&memory).await;

    // The database records when messages were written.
    let listed = memory.list_sessions("instance:a:chat#1").await.unwrap();
    assert!(listed[0].last_message_at.is_some_and(|at| at > 0));
}

#[tokio::test]
async fn sqlite_prefix_is_literal_not_a_like_pattern() {
    let memory = SqliteMemory::open_in_memory().unwrap();
    memory
        .push_message("chat_1#0", ChatMessage::user("a"))
        .await
        .unwrap();
    memory
        .push_message("chatX1#0", ChatMessage::user("b"))
        .await
        .unwrap();
    let listed = memory.list_sessions("chat_1#").await.unwrap();
    assert_eq!(listed.len(), 1, "`_` must not match any character");
}

#[tokio::test]
async fn deleting_a_session_removes_history_summary_and_record_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    let open = || {
        SessionManager::new(Arc::new(SqliteMemory::open(&db).unwrap()))
            .with_store(Arc::new(SqliteSessionStore::open(&db).unwrap()))
            .unwrap()
    };
    let doomed = "instance:a:chat#0";
    let kept = "instance:a:chat#1";
    {
        let sessions = open();
        for key in [doomed, kept] {
            sessions
                .memory()
                .extend_messages(key, turn("q", "a"))
                .await
                .unwrap();
            sessions.record_turn(key, 10);
            sessions.set_persona(key, "pirate").unwrap();
        }
        sessions
            .memory()
            .compact_history(doomed, 2, "summary".to_string())
            .await
            .unwrap();
        sessions.delete_session(doomed).await.unwrap();

        let listed = sessions
            .sessions_with_prefix("instance:a:chat#")
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_key, kept);
    }

    let sessions = open();
    assert!(sessions.get_metadata(doomed).is_none());
    let snapshot = sessions.memory().snapshot(doomed).await.unwrap();
    assert!(snapshot.messages.is_empty() && snapshot.summary.is_none());
    let kept_snapshot = sessions.memory().snapshot(kept).await.unwrap();
    assert_eq!(
        kept_snapshot.messages.len(),
        4,
        "other sessions are untouched"
    );
    assert_eq!(sessions.get_persona(kept).as_deref(), Some("pirate"));
}

#[tokio::test]
async fn a_session_record_without_messages_is_listed_without_a_last_turn() {
    let sessions = SessionManager::new(Arc::new(InMemory::new()));
    sessions.set_persona("instance:a:chat#3", "pirate").unwrap();
    sessions.record_turn("instance:a:chat#4", 1);

    let listed = sessions
        .sessions_with_prefix("instance:a:chat#")
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].message_count, 0);
    assert_eq!(
        listed[0].last_active_at, 0,
        "a persona binding is not a turn"
    );
    assert!(listed[1].last_active_at > 0);
}

/// A store whose deletions fail.
struct UndeletableStore;

impl SessionStore for UndeletableStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        Ok(Vec::new())
    }

    fn save(&self, _metadata: &SessionMetadata) -> Result<(), MemoryError> {
        Ok(())
    }

    fn delete(&self, _session_key: &str) -> Result<(), MemoryError> {
        Err(MemoryError::Backend("read-only disk".to_string()))
    }
}

#[tokio::test]
async fn a_failed_record_deletion_is_reported_and_the_record_kept() {
    let sessions = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(Arc::new(UndeletableStore))
        .unwrap();
    sessions.record_turn("s", 1);

    assert!(sessions.delete_session("s").await.is_err());
    assert!(
        sessions.get_metadata("s").is_some(),
        "memory and disk must agree after a failed deletion"
    );
}
