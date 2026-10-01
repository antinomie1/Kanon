//! Management API persistence for the delivery-only line-splitting setting.

mod common;

use std::sync::Arc;

use axum::http::Method;
use kanon_api::ApiState;
use kanon_api::llm_config::SystemConfigStore;
use kanon_core::supervisor::Supervisor;
use serde_json::json;

#[tokio::test]
async fn node_reply_line_setting_is_saved_and_reloaded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_config_dir(dir.path().to_path_buf())
    .with_system_config(Arc::new(SystemConfigStore::new(&path)))
    .build();
    let app = kanon_api::app(state.clone());
    let (status, initial) =
        common::send_json(&app, Method::GET, "/api/v1/system/reply-policy", None).await;
    assert_eq!(status, 200);
    assert_eq!(initial["policy"]["split_lines"], false);

    let mut policy = initial["policy"].clone();
    policy["split_lines"] = json!(true);
    let (status, saved) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/system/reply-policy",
        Some(policy),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(saved["policy"]["split_lines"], true);
    assert!(state.reply_policy().get().split_lines);

    let store = SystemConfigStore::new(path);
    assert!(
        store
            .load_node_settings()
            .expect("reload settings")
            .reply_policy
            .split_lines
    );
}
