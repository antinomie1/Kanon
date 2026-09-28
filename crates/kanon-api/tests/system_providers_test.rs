//! Integration tests for system configuration, providers catalog/test, and embedded WebUI serving.

mod common;

use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use serde_json::Value;

use common::{empty_state, fixture_state, send_json, send_raw};

/// Verifies that GET /api/v1/system/config reports valid node runtime parameters.
#[tokio::test]
async fn system_config_reports_runtime_parameters() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;
    let app: Router = app(state);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/system/config", None).await;

    assert_eq!(status, 200);
    assert!(body["version"].as_str().is_some());
    assert!(body["ipc_socket_path"].as_str().is_some());
    assert!(body["run_dir"].as_str().is_some());
    assert!(body["data_dir"].as_str().is_some());
    assert_eq!(body["memory_window"], 40);

    assert_eq!(body["llm"]["configured"], true);
    assert_eq!(body["llm"]["model"], "mock-model");
    assert!(body["environment"]["os"].as_str().is_some());
}

/// Verifies that GET /api/v1/providers returns active provider details and protocol presets.
#[tokio::test]
async fn providers_catalog_reports_active_and_presets() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;
    let app: Router = app(state);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/providers", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["active"]["configured"], true);
    assert_eq!(body["active"]["model"], "mock-model");

    let protocols = body["available_protocols"]
        .as_array()
        .expect("available_protocols array");
    assert!(protocols.iter().any(|p| p["id"] == "openai"));
    assert!(protocols.iter().any(|p| p["id"] == "anthropic"));

    let presets = body["presets"].as_array().expect("presets array");
    assert!(presets.iter().any(|p| p["id"] == "openai"));
    assert!(presets.iter().any(|p| p["id"] == "deepseek"));
    assert!(presets.iter().any(|p| p["id"] == "ollama"));
    for preset in presets {
        assert!(preset.get("default_model").is_none());
    }
}

/// Verifies that POST /api/v1/providers/test tests connectivity against the active provider.
#[tokio::test]
async fn providers_test_executes_against_active_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = fixture_state(PathBuf::from(dir.path()), true).await;
    let app: Router = app(state);

    let payload = serde_json::json!({
        "prompt": "hello test"
    });
    let (status, body) =
        send_json(&app, Method::POST, "/api/v1/providers/test", Some(payload)).await;

    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["reply"], "fixture reply");
    assert_eq!(body["model"], "mock-model");
    assert!(body["error"].is_null());
}

/// Verifies that POST /api/v1/providers/test rejects requests when no provider is available and no params given.
#[tokio::test]
async fn providers_test_rejects_empty_without_active_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    let app: Router = app(state);

    let payload = serde_json::json!({});
    let (status, body) =
        send_json(&app, Method::POST, "/api/v1/providers/test", Some(payload)).await;

    assert_eq!(status, 400);
    assert_eq!(common::error_code(&body), "bad_request");
}

/// Verifies that the gateway serves embedded WebUI static files and SPA fallback.
#[tokio::test]
async fn webui_static_files_and_spa_fallback_served() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    let app: Router = app(state);

    // Root path serves index.html
    let (status, body, content_type) = send_raw(&app, Method::GET, "/").await;
    assert_eq!(status, 200);
    assert!(
        content_type.contains("text/html"),
        "expected text/html, got {content_type}"
    );
    assert!(body.contains("<html") || body.contains("<!doctype html>"));

    // SPA client-side route falls back to index.html
    let (status, body, content_type) = send_raw(&app, Method::GET, "/sessions").await;
    assert_eq!(status, 200);
    assert!(content_type.contains("text/html"));
    assert!(body.contains("<html") || body.contains("<!doctype html>"));

    // Static asset (e.g. favicon.svg) is served with proper MIME type
    let (status, body, content_type) = send_raw(&app, Method::GET, "/favicon.svg").await;
    assert_eq!(status, 200);
    assert!(
        content_type.contains("svg"),
        "expected svg content-type, got {content_type}"
    );
    assert!(body.contains("<svg"));

    // Unknown API path must NOT fall back to SPA index.html; it must return JSON 404
    let (status, body): (_, Value) =
        send_json(&app, Method::GET, "/api/v1/does-not-exist", None).await;
    assert_eq!(status, 404);
    assert_eq!(common::error_code(&body), "not_found");
}

/// Verifies that POST /api/v1/providers/models rejects empty base_url.
#[tokio::test]
async fn fetch_models_rejects_empty_base_url() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    let app: Router = app(state);

    let payload = serde_json::json!({
        "base_url": "   "
    });
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/providers/models",
        Some(payload),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(common::error_code(&body), "bad_request");
}

/// Verifies that POST /api/v1/providers/models queries remote endpoint and extracts model IDs.
#[tokio::test]
async fn fetch_models_queries_endpoint_and_extracts_models() {
    let mock_app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "data": [
                    { "id": "model-alpha" },
                    { "id": "model-beta" }
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock_app).await;
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    let app: Router = app(state);

    let payload = serde_json::json!({
        "protocol": "openai",
        "base_url": format!("http://{addr}/v1")
    });
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/providers/models",
        Some(payload),
    )
    .await;

    assert_eq!(status, 200);
    let models = body["models"].as_array().expect("models array");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0], "model-alpha");
    assert_eq!(models[1], "model-beta");
}

/// Verifies that POST /api/v1/providers/models handles Anthropic protocol with authentication headers.
#[tokio::test]
async fn fetch_models_anthropic_queries_endpoint_with_headers() {
    let mock_app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(
                headers.get("x-api-key").and_then(|v| v.to_str().ok()),
                Some("sk-ant-test")
            );
            assert_eq!(
                headers
                    .get("anthropic-version")
                    .and_then(|v| v.to_str().ok()),
                Some("2023-06-01")
            );
            axum::Json(serde_json::json!({
                "data": [
                    { "id": "claude-test-custom" }
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock_app).await;
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let state = empty_state(PathBuf::from(dir.path())).await;
    let app: Router = app(state);

    let payload = serde_json::json!({
        "protocol": "anthropic",
        "base_url": format!("http://{addr}/v1"),
        "api_key": "sk-ant-test"
    });
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/providers/models",
        Some(payload),
    )
    .await;

    assert_eq!(status, 200);
    let models = body["models"].as_array().expect("models array");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0], "claude-test-custom");
}
