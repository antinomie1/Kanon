//! Sandbox chat completion coverage (JSON and Server-Sent Events).

mod common;

use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use serde_json::json;

use common::{empty_state, error_code, fixture_state, send_json, send_raw_json};

/// Non-streaming completions return the agent answer and register the session.
#[tokio::test]
async fn chat_completion_returns_json_answer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;
    let app: Router = app(state.clone());

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "webui:demo", "message": "hello" })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["session_id"], "webui:demo");
    assert_eq!(body["content"], "fixture reply");
    assert_eq!(body["turns"], 1);
    assert_eq!(body["finish_reason"], "stop");
    assert_eq!(body["executed_tools"].as_array().map(Vec::len), Some(0));

    // The turn is recorded in session metadata, so the console sees it immediately.
    let metadata = state
        .sessions()
        .get_metadata("webui:demo")
        .expect("session recorded");
    assert_eq!(metadata.turn_count, 1);
    assert!(metadata.total_tokens_used > 0);
}

/// The tool-enabled branch runs the same turn without registered hosts.
#[tokio::test]
async fn chat_completion_with_tools_flag_succeeds_without_hosts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let without_provider: Router = app(empty_state(PathBuf::from(dir.path())).await);

    // Without a configured provider the endpoint must refuse rather than fall back.
    let (status, body) = send_json(
        &without_provider,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "webui:demo", "message": "hello", "tools": true })),
    )
    .await;

    assert_eq!(status, 503);
    assert_eq!(error_code(&body), "unavailable");

    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), true).await);
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "webui:demo", "message": "hello", "tools": true })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["content"], "fixture reply");
}

/// Streaming completions emit SSE delta and terminal frames.
#[tokio::test]
async fn chat_completion_streams_server_sent_events() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), true).await);

    let (status, body, content_type) = common::send_raw_json(
        &app,
        "/api/v1/chat/completions",
        json!({ "session_id": "webui:stream", "message": "stream please", "stream": true }),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert!(
        content_type.starts_with("text/event-stream"),
        "unexpected content type: {content_type}"
    );
    assert!(body.contains(r#""type":"delta""#), "body: {body}");
    assert!(body.contains("fixture reply"), "body: {body}");
    assert!(body.contains(r#""type":"done""#), "body: {body}");
    assert!(body.contains(r#""finish_reason":"stop""#), "body: {body}");
}

/// Chat requests are validated before the agent is invoked.
#[tokio::test]
async fn chat_completion_validates_request() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), true).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "  ", "message": "hello" })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");

    let (status, _) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "webui:demo", "message": "   " })),
    )
    .await;
    assert_eq!(status, 400);

    // An instance journal cannot discard its target and inherit node-wide tool permissions.
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "instance:bot:webui:chat#0", "message": "hello", "tools": true
        })),
    )
    .await;
    assert_eq!(
        status, 400,
        "unscoped instance journal was accepted: {body}"
    );
    assert_eq!(error_code(&body), "bad_request");

    // Partial explicit coordinates must not fall back to the configured provider; an endpoint
    // also needs a real model instead of an invented "default" sent to the remote service.
    for fields in [
        json!({"protocol": "openai"}),
        json!({"base_url": "http://127.0.0.1:9/v1"}),
        json!({"api_key": "replacement"}),
        json!({"protocol": "openai", "base_url": ""}),
        json!({"protocol": "openai", "base_url": "http://127.0.0.1:9/v1"}),
    ] {
        let mut request = json!({"session_id": "webui:invalid", "message": "must not be sent"});
        request
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let (status, body) = send_json(
            &app,
            Method::POST,
            "/api/v1/chat/completions",
            Some(request),
        )
        .await;
        assert_eq!(status, 400, "invalid endpoint was accepted: {body}");
    }

    // An unknown persona override must not silently fall back to the default persona.
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "webui:demo", "message": "hi", "persona_id": "wizard" })),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// A persona override applies to the session before the turn runs.
#[tokio::test]
async fn chat_completion_applies_persona_override() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;
    state
        .personas()
        .register(
            kanon_llm::Persona::custom("concise", "Concise", "", "Answer in one sentence.")
                .expect("valid persona"),
        )
        .expect("registered");
    let app: Router = app(state.clone());

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "webui:persona",
            "message": "hello",
            "persona_id": "concise"
        })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["persona_id"], "concise");
    assert_eq!(
        state.sessions().get_persona("webui:persona").as_deref(),
        Some("concise")
    );
}

/// A completion request specifying dynamic provider parameters executes even when
/// no default provider is configured on the node core.
#[tokio::test]
async fn chat_completion_with_dynamic_provider_succeeds_without_node_agent() {
    let mock_app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(
            |axum::Json(request): axum::Json<serde_json::Value>| async move {
                assert_eq!(
                    request["model"], "anthropic/claude",
                    "explicit endpoint model IDs must reach upstream intact"
                );
                axum::Json(serde_json::json!({
                    "id": "chatcmpl-test",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "dynamic provider reply"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                }))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock_app).await;
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let without_provider: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(
        &without_provider,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "webui:dynamic",
            "message": "hello dynamic",
            "protocol": "openai",
            "base_url": format!("http://{addr}/v1"),
            "model": "anthropic/claude"
        })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["session_id"], "webui:dynamic");
    assert_eq!(body["content"], "dynamic provider reply");
    assert_eq!(body["finish_reason"], "stop");
}

/// A streaming completion request with a reasoning-capable provider emits both reasoning and delta chunks.
#[tokio::test]
async fn chat_stream_with_reasoning_yields_reasoning_and_delta() {
    let mock_app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            let sse_body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":null,\"reasoning_content\":\"Thinking step 1...\"},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Final answer text.\",\"reasoning_content\":null},\"finish_reason\":\"stop\"}]}\n\n\
                            data: [DONE]\n\n";
            ([("content-type", "text/event-stream")], sse_body)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock_app).await;
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let without_provider: Router = app(empty_state(PathBuf::from(dir.path())).await);

    let (status, body, content_type) = send_raw_json(
        &without_provider,
        "/api/v1/chat/completions",
        json!({
            "session_id": "webui:reasoning_stream",
            "message": "reason about this",
            "stream": true,
            "protocol": "openai",
            "base_url": format!("http://{addr}/v1"),
            "model": "deepseek/deepseek-reasoner"
        }),
    )
    .await;

    assert_eq!(status, 200);
    assert!(
        content_type.starts_with("text/event-stream"),
        "content_type: {content_type}"
    );
    assert!(
        body.contains("Thinking step 1..."),
        "body missing reasoning: {body}"
    );
    assert!(
        body.contains("Final answer text."),
        "body missing delta: {body}"
    );
    assert!(
        body.contains("\"type\":\"done\""),
        "body missing done event: {body}"
    );
}

/// Every console write uses the same writer as a running pipeline or streaming producer.
#[tokio::test]
async fn busy_session_rejects_chat_reset_and_persona_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), true).await;
    state.sessions().get_or_create("busy");
    let writing = state.sessions().try_write("busy").unwrap();
    let app = app(state.clone());
    for (path, body) in [
        (
            "/api/v1/chat/completions",
            json!({"session_id":"busy","message":"hello"}),
        ),
        (
            "/api/v1/chat/completions",
            json!({"session_id":"busy","message":"hello","stream":true}),
        ),
        ("/api/v1/sessions/busy/reset", json!({})),
        ("/api/v1/sessions/busy/persona", json!({"persona_id":null})),
    ] {
        let (status, response) = send_json(&app, Method::POST, path, Some(body)).await;
        assert_eq!(status, 409, "{path}: {response}");
    }
    assert_eq!(state.sessions().get_metadata("busy").unwrap().turn_count, 0);
    drop(writing);
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({"session_id":"busy","message":"hello"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}
