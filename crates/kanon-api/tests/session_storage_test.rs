//! Durable session storage as the node wires it: sessions listed in the console and their persona
//! bindings come back after a restart, and unusable storage stops startup.

mod common;

use std::path::Path;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, open_session_manager};
use kanon_llm::ChatMessage;
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
        state.sessions().set_persona("s", "assistant");
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
