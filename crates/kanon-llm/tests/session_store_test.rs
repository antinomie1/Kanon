//! Tests for durable session metadata: what a session is besides its messages must survive a
//! restart, or a conversation would resume with the wrong persona, counters and status.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kanon_llm::error::MemoryError;
use kanon_llm::gateway::types::ChatMessage;
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::session::{
    SessionKey, SessionManager, SessionMetadata, SessionScope, SessionStatus, SessionStore,
};
use kanon_llm::{SqliteMemory, SqliteSessionStore};

/// A manager over a database file, as the node builds it.
fn open(path: &std::path::Path) -> SessionManager {
    SessionManager::new(Arc::new(SqliteMemory::open(path).expect("memory")))
        .with_store(Arc::new(SqliteSessionStore::open(path).expect("store")))
        .expect("stored sessions load")
}

#[tokio::test]
async fn everything_about_a_session_survives_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");
    let key = "instance:bot:group:1:user:1#2";

    {
        let sessions = open(&db);
        sessions.get_or_create_with_scope(&SessionKey::custom(key));
        sessions.set_persona(key, "pirate");
        sessions.set_variable(key, "lang", "zh-CN");
        sessions.set_variable(key, "tz", "UTC+8");
        sessions.remove_variable(key, "tz");
        sessions.record_turn(key, 120);
        sessions.record_turn(key, 80);
        sessions
            .memory()
            .push_message(key, ChatMessage::user("hello"))
            .await
            .unwrap();
        sessions
            .memory()
            .push_message(key, ChatMessage::assistant("ahoy"))
            .await
            .unwrap();
    }

    // "Restart": a new manager over the same file.
    let sessions = open(&db);
    let record = sessions
        .get_metadata(key)
        .expect("the session was restored");
    assert_eq!(record.session_key, key);
    assert_eq!(record.persona_id.as_deref(), Some("pirate"));
    assert_eq!(record.turn_count, 2);
    assert_eq!(record.total_tokens_used, 200);
    assert_eq!(record.status, SessionStatus::Active);
    assert_eq!(
        record.variables.get("lang").map(String::as_str),
        Some("zh-CN")
    );
    assert!(
        !record.variables.contains_key("tz"),
        "a removed variable stays removed"
    );
    assert_eq!(
        record.scope,
        SessionScope::Custom(key.to_string()),
        "the scope is restored, not reset to the default"
    );

    // The history is there too, so the conversation continues rather than restarting.
    let history = sessions.memory().get_messages(key).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content.as_deref(), Some("ahoy"));
    assert_eq!(sessions.session_count(), 1);
}

#[tokio::test]
async fn every_kind_of_change_is_written_through() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");

    {
        let sessions = open(&db);
        sessions.set_persona("a", "pirate");
        sessions.set_persona("b", "pirate");
        sessions.set_persona("c", "other");
        sessions.close_session("c");
        sessions.record_turn("d", 10);
        sessions
            .memory()
            .push_message("d", ChatMessage::user("x"))
            .await
            .unwrap();

        // Deleting a persona unbinds every session using it; clearing removes one binding.
        assert_eq!(sessions.unbind_persona("pirate"), 2);
        sessions.set_persona("a", "kept");
        sessions.clear_persona("a");
        // A reset zeroes the counters but keeps the persona and variables.
        sessions.set_persona("d", "sticky");
        sessions.set_variable("d", "k", "v");
        sessions.reset_session("d").await.unwrap();
    }

    let sessions = open(&db);
    assert!(sessions.get_persona("a").is_none());
    assert!(sessions.get_persona("b").is_none());
    assert_eq!(sessions.get_persona("c").as_deref(), Some("other"));
    assert_eq!(
        sessions.get_metadata("c").unwrap().status,
        SessionStatus::Closed
    );
    let d = sessions.get_metadata("d").unwrap();
    assert_eq!(d.turn_count, 0);
    assert_eq!(d.persona_id.as_deref(), Some("sticky"));
    assert_eq!(d.variables.get("k").map(String::as_str), Some("v"));
    assert!(
        sessions
            .memory()
            .get_messages("d")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn idle_status_is_persisted() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");
    {
        let sessions = open(&db);
        sessions.record_turn("idle-me", 1);
        assert_eq!(
            sessions.sweep_idle_sessions(std::time::Duration::from_secs(0)),
            1
        );
    }
    assert_eq!(
        open(&db).get_metadata("idle-me").unwrap().status,
        SessionStatus::Idle
    );
}

/// A store that counts its writes.
#[derive(Default)]
struct CountingStore {
    saves: AtomicUsize,
}

impl SessionStore for CountingStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        Ok(Vec::new())
    }

    fn save(&self, _metadata: &SessionMetadata) -> Result<(), MemoryError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn delete(&self, _session_key: &str) -> Result<(), MemoryError> {
        Ok(())
    }
}

#[test]
fn rebinding_the_persona_a_session_already_has_costs_no_write() {
    // The pipeline binds the instance's persona on every message of a conversation.
    let store = Arc::new(CountingStore::default());
    let sessions = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(store.clone())
        .unwrap();

    sessions.set_persona("s", "pirate");
    assert_eq!(store.saves.load(Ordering::SeqCst), 1);
    for _ in 0..100 {
        sessions.set_persona("s", "pirate");
    }
    assert_eq!(
        store.saves.load(Ordering::SeqCst),
        1,
        "a hundred messages must not be a hundred writes"
    );

    sessions.set_persona("s", "other");
    assert_eq!(store.saves.load(Ordering::SeqCst), 2);

    // Reading never writes, and creating a session on first sight writes exactly once.
    sessions.get_metadata("s");
    sessions.get_or_create("s");
    assert_eq!(store.saves.load(Ordering::SeqCst), 2);
    sessions.get_or_create("fresh");
    sessions.get_or_create("fresh");
    assert_eq!(store.saves.load(Ordering::SeqCst), 3);
}

/// A store whose writes always fail.
struct BrokenStore;

impl SessionStore for BrokenStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        Ok(Vec::new())
    }

    fn save(&self, _metadata: &SessionMetadata) -> Result<(), MemoryError> {
        Err(MemoryError::Backend("disk full".to_string()))
    }

    fn delete(&self, _session_key: &str) -> Result<(), MemoryError> {
        Err(MemoryError::Backend("disk full".to_string()))
    }
}

#[test]
fn a_failing_store_never_turns_a_reply_into_an_error() {
    let sessions = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(Arc::new(BrokenStore))
        .unwrap();

    // The write fails (and is logged), but the session keeps working in memory.
    sessions.record_turn("s", 5);
    sessions.set_persona("s", "pirate");
    let record = sessions.get_metadata("s").unwrap();
    assert_eq!(record.turn_count, 1);
    assert_eq!(record.persona_id.as_deref(), Some("pirate"));
}

/// A store that cannot be read.
struct UnreadableStore;

impl SessionStore for UnreadableStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        Err(MemoryError::Backend("corrupt".to_string()))
    }

    fn save(&self, _metadata: &SessionMetadata) -> Result<(), MemoryError> {
        Ok(())
    }

    fn delete(&self, _session_key: &str) -> Result<(), MemoryError> {
        Ok(())
    }
}

#[test]
fn a_store_that_cannot_be_read_stops_startup_instead_of_starting_empty() {
    let result =
        SessionManager::new(Arc::new(InMemory::new())).with_store(Arc::new(UnreadableStore));
    assert!(result.is_err(), "starting empty would look like amnesia");
}

#[test]
fn a_record_that_cannot_be_decoded_is_an_error_not_a_skipped_row() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");
    drop(SqliteSessionStore::open(&db).expect("creates the table"));
    rusqlite::Connection::open(&db)
        .expect("db")
        .execute(
            "INSERT INTO session_meta (session_key, data, updated_at) VALUES ('bad', '{nope', 0)",
            [],
        )
        .expect("seed a corrupt row");

    let store = SqliteSessionStore::open(&db).expect("store");
    let error = store.load_all().expect_err("corrupt row");
    assert!(error.to_string().contains("'bad'"), "{error}");
}

#[tokio::test]
async fn memory_and_session_store_can_share_one_database_file_under_concurrent_writes() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");
    let sessions = Arc::new(open(&db));

    let mut tasks = Vec::new();
    for i in 0..20 {
        let sessions = sessions.clone();
        tasks.push(tokio::spawn(async move {
            let key = format!("s{}", i % 4);
            sessions
                .memory()
                .push_message(&key, ChatMessage::user(format!("m{i}")))
                .await
                .unwrap();
            sessions.record_turn(&key, 1);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    let reopened = open(&db);
    for n in 0..4 {
        let key = format!("s{n}");
        assert_eq!(reopened.get_metadata(&key).unwrap().turn_count, 5);
        assert_eq!(reopened.memory().get_messages(&key).await.unwrap().len(), 5);
    }
}
