//! Integration tests for agent selection: the node's default agent and the per-instance override.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use serde_json::json;

/// Builds state whose node system configuration lives inside an isolated directory.
fn isolated_state(config_dir: PathBuf) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .with_system_config(Arc::new(SystemConfigStore::new(
            config_dir.join("system.json"),
        )))
        .build()
}

#[tokio::test]
async fn the_default_agent_is_selectable_persisted_and_validated() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/agents", None).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(
        body["agents"],
        if cfg!(feature = "dsh") {
            json!(["builtin", "dsh"])
        } else {
            json!(["builtin"])
        }
    );
    assert_eq!(body["default_agent"], json!("builtin"));

    // An engine the node cannot run is refused before anything reaches disk.
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/agents/default",
        Some(json!({ "agent": "dify" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
    assert!(!dir.path().join("system.json").exists());

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/agents/default",
        Some(json!({ "agent": "builtin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("system.json")).expect("saved document"),
    )
    .expect("json");
    assert_eq!(saved["default_agent"], json!("builtin"));
}

#[tokio::test]
async fn an_instance_overrides_the_agent_only_with_a_selectable_one() {
    let dir = tempfile::tempdir().expect("config dir");
    let app = kanon_api::app(isolated_state(dir.path().to_path_buf()));

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({ "name": "Agent Bot", "enabled": false, "agent": "builtin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["instance"]["agent"], json!("builtin"));

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({ "name": "Other Bot", "enabled": false, "agent": "dify" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
}
