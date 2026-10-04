//! Session management and persona route coverage.

mod common;

use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use kanon_llm::ChatMessage;
use serde_json::json;

use common::{empty_state, error_code, fixture_state, send_json};

/// Sessions are listed with pagination, filtering and deterministic ordering.
#[tokio::test]
async fn sessions_are_paginated_and_filtered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;

    for index in 0..3 {
        let key = format!("channel:{index}:user:42");
        state.sessions().record_turn(&key, 10 * (index + 1));
    }
    state.sessions().close_session("channel:1:user:42");

    let app: Router = app(state);

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/sessions?page=1&page_size=2",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 2);
    assert_eq!(body["total_pages"], 2);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(2));

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/sessions?page=2&page_size=2",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));

    let (status, body) = send_json(&app, Method::GET, "/api/v1/sessions?status=closed", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["session_key"], "channel:1:user:42");

    let (status, body) =
        send_json(&app, Method::GET, "/api/v1/sessions?search=user:42", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 3);

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/sessions?scope=channel_user",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 3);
}

/// Invalid pagination and filter values are rejected with actionable messages.
#[tokio::test]
async fn session_listing_validates_query_parameters() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/sessions?page=0", None).await;
    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");

    let (status, _) = send_json(&app, Method::GET, "/api/v1/sessions?page_size=0", None).await;
    assert_eq!(status, 400);

    let (status, _) = send_json(&app, Method::GET, "/api/v1/sessions?page_size=5000", None).await;
    assert_eq!(status, 400);

    let (status, body) =
        send_json(&app, Method::GET, "/api/v1/sessions?status=sleeping", None).await;
    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("sleeping")),
        "error must echo the rejected status: {body}"
    );
}

/// Resetting clears history while preserving persona and session variables.
#[tokio::test]
async fn session_reset_preserves_persona_and_variables() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;

    let key = "channel:9:user:7";
    state.sessions().record_turn(key, 25);
    state.sessions().set_variable(key, "locale", "zh-CN");
    state.sessions().set_persona(key, "coder").unwrap();
    state
        .sessions()
        .memory()
        .push_message(key, ChatMessage::user("hello"))
        .await
        .expect("message stored");

    let app: Router = app(state.clone());
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/sessions/channel:9:user:7/reset",
        None,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["reset"], true);
    assert_eq!(body["turn_count"], 0);
    assert_eq!(body["persona_id"], "coder");
    assert_eq!(body["variables"]["locale"], "zh-CN");

    // History is gone but configuration survives.
    let messages = state
        .sessions()
        .memory()
        .get_messages(key)
        .await
        .expect("history readable");
    assert!(messages.is_empty(), "history must be cleared: {messages:?}");
    assert_eq!(state.sessions().get_persona(key).as_deref(), Some("coder"));
    assert_eq!(
        state.sessions().get_variable(key, "locale").as_deref(),
        Some("zh-CN")
    );
    assert_eq!(
        state
            .sessions()
            .get_metadata(key)
            .map(|meta| meta.total_tokens_used),
        Some(0)
    );
}

/// Resetting an untracked session is reported instead of silently succeeding.
#[tokio::test]
async fn session_reset_unknown_session_returns_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/sessions/does-not-exist/reset",
        None,
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// The persona catalog ships exactly one persona: the read-only base assistant.
#[tokio::test]
async fn persona_catalog_ships_only_the_base_assistant() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/personas", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);
    assert_eq!(body["base_persona_id"], "assistant");

    let personas = body["personas"].as_array().expect("personas array");
    assert_eq!(personas.len(), 1);
    assert_eq!(personas[0]["id"], "assistant");
    assert_eq!(personas[0]["kind"], "builtin");
    assert_eq!(
        personas[0]["prompt"],
        kanon_llm::prompt::BASE_PERSONA_PROMPT
    );
    assert!(
        personas[0].get("template").is_none() && personas[0].get("variables").is_none(),
        "personas are plain static text: {body}"
    );
}

/// Persona switching binds the persona to the session and is visible to the agent hook.
#[tokio::test]
async fn session_persona_switch_binds_persona() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    state
        .personas()
        .register(
            kanon_llm::Persona::custom("coder", "Code Architect", "", "You write Rust.")
                .expect("valid persona"),
        )
        .expect("registered");
    let app: Router = app(state.clone());

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/sessions/webui:debug/persona",
        Some(json!({ "persona_id": "coder" })),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["persona_id"], "coder");
    assert_eq!(body["persona_name"], "Code Architect");
    assert_eq!(
        state.sessions().get_persona("webui:debug").as_deref(),
        Some("coder")
    );

    // Switching persona registers the session so consoles can pre-configure it.
    assert!(state.sessions().get_metadata("webui:debug").is_some());

    // A blank or null identifier removes the binding: the session uses the base assistant again.
    for cleared in [json!({ "persona_id": null }), json!({ "persona_id": "  " })] {
        state
            .sessions()
            .set_persona("webui:debug", "coder")
            .unwrap();
        let (status, body) = send_json(
            &app,
            Method::POST,
            "/api/v1/sessions/webui:debug/persona",
            Some(cleared),
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        assert_eq!(body["persona_id"], serde_json::Value::Null);
        assert_eq!(body["persona_name"], "Assistant");
        assert!(state.sessions().get_persona("webui:debug").is_none());
    }
}

/// Unknown personas are rejected.
#[tokio::test]
async fn session_persona_switch_validates_persona() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/sessions/webui:debug/persona",
        Some(json!({ "persona_id": "wizard" })),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}
