//! Durable session storage as the node wires it: sessions listed in the console and their persona
//! bindings come back after a restart, and unusable storage stops startup.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, open_session_manager};
use kanon_llm::ChatMessage;
use kanon_llm::error::MemoryError;
use kanon_llm::memory::InMemory;
use kanon_llm::session::{SessionManager, SessionMetadata, SessionStore};
use serde_json::json;

use common::send_json;

/// State over the session database at `db`, like the composition root builds it.
fn node(db: &Path) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    ApiState::builder(supervisor)
        .with_sessions(open_session_manager(db).expect("session storage opens"))
        .build()
}

#[tokio::test]
async fn sessions_persona_bindings_and_history_come_back_after_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("nested").join("sessions.db");

    {
        let state = node(&db);
        state
            .personas()
            .register(
                kanon_llm::Persona::custom("pirate", "Pirate", "", "Arr.").expect("valid persona"),
            )
            .expect("registered");
        let app = kanon_api::app(state.clone());

        let (status, body) = send_json(
            &app,
            Method::POST,
            "/api/v1/sessions/channel:1:user:2/persona",
            Some(json!({ "persona_id": "pirate" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");

        state.sessions().record_turn("channel:1:user:2", 42);
        state
            .sessions()
            .memory()
            .push_message("channel:1:user:2", ChatMessage::user("ahoy"))
            .await
            .expect("history stored");
    }

    // A fresh node over the same file lists the session with everything the console showed.
    let state = node(&db);
    let app = kanon_api::app(state.clone());
    let (status, body) = send_json(&app, Method::GET, "/api/v1/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    let session = &body["items"][0];
    assert_eq!(session["session_key"], "channel:1:user:2");
    assert_eq!(session["persona_id"], "pirate");
    assert_eq!(session["turn_count"], 1);
    assert_eq!(session["total_tokens_used"], 42);

    let history = state
        .sessions()
        .memory()
        .get_messages("channel:1:user:2")
        .await
        .expect("history readable");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content.as_deref(), Some("ahoy"));
}

#[tokio::test]
async fn resetting_a_session_clears_its_history_durably_but_keeps_its_persona() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("sessions.db");
    {
        let state = node(&db);
        state.sessions().set_persona("s", "assistant").unwrap();
        state.sessions().record_turn("s", 5);
        state
            .sessions()
            .memory()
            .push_message("s", ChatMessage::user("hi"))
            .await
            .unwrap();

        let app = kanon_api::app(state);
        let (status, _) = send_json(&app, Method::POST, "/api/v1/sessions/s/reset", None).await;
        assert_eq!(status, StatusCode::OK);
    }

    let state = node(&db);
    let record = state.sessions().get_metadata("s").expect("session kept");
    assert_eq!(record.turn_count, 0);
    assert_eq!(record.persona_id.as_deref(), Some("assistant"));
    assert!(
        state
            .sessions()
            .memory()
            .get_messages("s")
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn storage_that_cannot_be_opened_is_an_error_not_an_empty_manager() {
    let dir = tempfile::tempdir().expect("dir");

    // A file that is not a database.
    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"this is definitely not sqlite, just some text").expect("seed");
    let error = open_session_manager(&corrupt)
        .err()
        .expect("a corrupt database must stop startup");
    assert!(
        error.contains("Failed to open session storage") && error.contains("corrupt.db"),
        "unexpected error: {error}"
    );

    // A parent that cannot be a directory.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file").expect("seed");
    let error = open_session_manager(blocker.join("sessions.db"))
        .err()
        .expect("an impossible location must fail");
    assert!(
        error.contains("Failed to create"),
        "unexpected error: {error}"
    );
}

/// A durable-state fixture whose writes can fail after an initial binding is saved.
#[derive(Default)]
struct RefusingStore {
    rows: Mutex<HashMap<String, SessionMetadata>>,
    reject: AtomicBool,
}

impl SessionStore for RefusingStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        Ok(self.rows.lock().unwrap().values().cloned().collect())
    }

    fn save(&self, metadata: &SessionMetadata) -> Result<(), MemoryError> {
        if self.reject.load(Ordering::SeqCst) {
            return Err(MemoryError::Backend("disk full".to_string()));
        }
        self.rows
            .lock()
            .unwrap()
            .insert(metadata.session_key.clone(), metadata.clone());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), MemoryError> {
        self.rows.lock().unwrap().remove(key);
        Ok(())
    }
}

#[tokio::test]
async fn rejected_persona_changes_preserve_live_and_durable_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(RefusingStore::default());
    let sessions = Arc::new(
        SessionManager::new(Arc::new(InMemory::new()))
            .with_store(store.clone())
            .unwrap(),
    );
    sessions.set_persona("existing", "assistant").unwrap();
    let original = serde_json::to_value(sessions.get_metadata("existing")).unwrap();
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(dir.path().to_path_buf()),
        None,
    ));
    let state = ApiState::builder(supervisor)
        .with_sessions(sessions.clone())
        .with_llm_provider(
            "test-agent",
            Arc::new(common::MockProvider::new("unused")),
            kanon_api::default_agent_config("mock-model"),
        )
        .build();
    state
        .personas()
        .register(kanon_llm::Persona::custom("pirate", "Pirate", "", "Arr.").unwrap())
        .unwrap();
    let app = kanon_api::app(state);
    store.reject.store(true, Ordering::SeqCst);

    for (key, persona) in [
        ("existing", json!("pirate")),
        ("existing", json!(null)),
        ("new", json!("pirate")),
    ] {
        let (status, body) = send_json(
            &app,
            Method::POST,
            &format!("/api/v1/sessions/{key}/persona"),
            Some(json!({ "persona_id": persona })),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(
            serde_json::to_value(sessions.get_metadata("existing")).unwrap(),
            original,
        );
        assert!(sessions.get_metadata("new").is_none());
        assert_eq!(
            serde_json::to_value(store.rows.lock().unwrap().get("existing")).unwrap(),
            original,
        );
    }

    let (status, _) = send_json(
        &app,
        Method::POST,
        "/api/v1/sessions/invalid/persona",
        Some(json!({ "persona_id": "unknown" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(sessions.get_metadata("invalid").is_none());

    // A chat override must fail before any model turn or history append takes place.
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "chat", "message": "hi", "persona_id": "pirate" })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(sessions.get_metadata("chat").is_none());
    assert!(
        sessions
            .memory()
            .get_messages("chat")
            .await
            .unwrap()
            .is_empty()
    );

    // Recovering storage makes the same switch and clear durable, without a restart to retry.
    store.reject.store(false, Ordering::SeqCst);
    for persona in [Some("pirate"), None] {
        let (status, body) = send_json(
            &app,
            Method::POST,
            "/api/v1/sessions/existing/persona",
            Some(json!({ "persona_id": persona })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let restored = SessionManager::new(Arc::new(InMemory::new()))
            .with_store(store.clone())
            .unwrap();
        assert_eq!(sessions.get_persona("existing").as_deref(), persona);
        assert_eq!(restored.get_persona("existing").as_deref(), persona);
    }
}

#[tokio::test]
async fn failed_persona_deletion_reports_the_failure_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(RefusingStore::default());
    let sessions = Arc::new(
        SessionManager::new(Arc::new(InMemory::new()))
            .with_store(store.clone())
            .unwrap(),
    );
    let personas = Arc::new(kanon_llm::PersonaStore::new(
        dir.path().join("personas.json"),
    ));
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(dir.path().join("run")),
        None,
    ));
    let state = ApiState::builder(supervisor)
        .with_sessions(sessions.clone())
        .with_persona_store(personas.clone())
        .build();
    personas
        .create(
            state.personas(),
            kanon_llm::Persona::custom("pirate", "Pirate", "", "Arr.").unwrap(),
        )
        .unwrap();
    sessions.set_persona("s", "pirate").unwrap();
    let app = kanon_api::app(state.clone());

    store.reject.store(true, Ordering::SeqCst);
    let (status, body) = send_json(&app, Method::DELETE, "/api/v1/personas/pirate", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(state.personas().get("pirate").is_some());
    assert_eq!(personas.load().unwrap()[0].id, "pirate");
    assert_eq!(sessions.get_persona("s").as_deref(), Some("pirate"));
    assert_eq!(
        store.rows.lock().unwrap()["s"].persona_id.as_deref(),
        Some("pirate")
    );

    // A later JSON failure leaves already committed unbindings in place and keeps the persona.
    store.reject.store(false, Ordering::SeqCst);
    let saved = dir.path().join("saved.json");
    std::fs::rename(personas.path(), &saved).unwrap();
    std::fs::create_dir(personas.path()).unwrap();
    let (status, body) = send_json(&app, Method::DELETE, "/api/v1/personas/pirate", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(state.personas().get("pirate").is_some());
    assert!(sessions.get_persona("s").is_none());
    assert!(store.rows.lock().unwrap()["s"].persona_id.is_none());

    std::fs::remove_dir(personas.path()).unwrap();
    std::fs::rename(saved, personas.path()).unwrap();
    let (status, body) = send_json(&app, Method::DELETE, "/api/v1/personas/pirate", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(state.personas().get("pirate").is_none());
    assert!(personas.load().unwrap().is_empty());
}
