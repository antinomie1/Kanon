//! QQ Official adapter management over HTTP against a real `system.json`: the secret is
//! write-only, survives unrelated edits, and a rejected update changes neither the adapter nor
//! the file.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use kanon_adapter_qqofficial::{QqOfficialAdapter, QqOfficialConfig};
use kanon_api::{ApiState, SystemConfigStore, app};
use kanon_core::supervisor::Supervisor;
use serde_json::{Value, json};

use common::send_json;

mod common;

/// Gateway state hosting an adapter that is registered but not started, so no connection is made.
async fn qq_state(dir: &Path) -> (Router, Arc<QqOfficialAdapter>) {
    let supervisor = Arc::new(Supervisor::new(Some(dir.join("run")), None));
    let adapter = Arc::new(QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap());
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .unwrap();
    let state = ApiState::builder(supervisor)
        .with_config_dir(dir.to_path_buf())
        .with_system_config(Arc::new(SystemConfigStore::new(dir.join("system.json"))))
        .with_qqofficial_adapter(adapter.clone())
        .build();
    (app(state), adapter)
}

fn persisted(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("system.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn secret_is_write_only_and_kept_across_edits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("system.json"),
        r#"{"unrelated":{"keep":true}}"#,
    )
    .unwrap();
    let (router, adapter) = qq_state(dir.path()).await;

    let (status, body) = send_json(
        &router,
        Method::GET,
        "/api/v1/adapters/qqofficial/config",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"]["connection_state"], "disabled");
    assert_eq!(body["status"]["platform"], "qqofficial");

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/qqofficial/config",
        Some(json!({"enabled": true, "app_id": "102030", "secret": "s3cret", "markdown": true})),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body["config"].get("secret").is_none(), "never echoed");
    assert_eq!(body["status"]["secret_configured"], true);
    assert_eq!(body["config"]["markdown"], true);
    let document = persisted(dir.path());
    assert_eq!(document["qqofficial"]["secret"], "s3cret");
    assert_eq!(document["unrelated"]["keep"], true);

    // An edit without the secret keeps the stored one.
    let (status, _) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/qqofficial/config",
        Some(json!({"enabled": false, "app_id": "102030", "secret": ""})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(adapter.config().secret.as_deref(), Some("s3cret"));
    assert_eq!(persisted(dir.path())["qqofficial"]["secret"], "s3cret");
    let restored = SystemConfigStore::new(dir.path().join("system.json"))
        .load_qqofficial()
        .unwrap()
        .unwrap();
    assert_eq!(restored, adapter.config());
}

#[tokio::test]
async fn enabling_without_credentials_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (router, adapter) = qq_state(dir.path()).await;

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/qqofficial/config",
        Some(json!({"enabled": true, "app_id": ""})),
    )
    .await;
    assert_eq!(status, 400, "body: {body}");
    assert!(!adapter.config().enabled);
    assert!(!dir.path().join("system.json").exists());
}
