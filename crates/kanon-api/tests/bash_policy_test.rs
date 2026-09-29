//! Management policy round-trip, restart and failed-write tests for Bash permissions.
mod common;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use kanon_core::{BashPolicyStore, BashPrincipal, Supervisor};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn bash_policy_round_trips_and_reloads_without_changing_the_tool_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let policy = Arc::new(BashPolicyStore::default());
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config.clone())
    .with_bash_policy(policy.clone())
    .with_native_tools(vec![Arc::new(
        kanon_core::BashTool::new(dir.path(), policy.clone()).unwrap(),
    )])
    .build();
    let app = kanon_api::app(state.clone());
    let (_, before) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    let (_, initial) =
        common::send_json(&app, Method::GET, "/api/v1/tools/bash/policy", None).await;
    assert_eq!(
        initial,
        json!({"mode":"allowlist", "allowlist":[], "denylist":[]})
    );
    let update = json!({"mode":"denylist", "allowlist":[], "denylist":[{"platform":"onebot", "user_id":"42"}]});
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, update);
    assert!(!policy.get().allows(Some(&BashPrincipal {
        platform: "onebot".into(),
        user_id: "42".into()
    })));
    assert!(policy.get().allows(Some(&BashPrincipal {
        platform: "onebot".into(),
        user_id: "43".into()
    })));
    let restored = config.load_node_settings().unwrap();
    assert_eq!(restored.bash_policy, state.bash_policy().get());
    let (_, after) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    assert_eq!(
        before, after,
        "permission changes must not change tool definitions"
    );
    let invalid =
        json!({"mode":"allowlist", "allowlist":[{"platform":"", "user_id":"42"}], "denylist":[]});
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(invalid),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        config.load_node_settings().unwrap().bash_policy,
        restored.bash_policy
    );
}

#[tokio::test]
async fn failed_policy_persistence_does_not_open_the_execution_gate() {
    let dir = tempfile::tempdir().unwrap();
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(Arc::new(SystemConfigStore::new(dir.path())))
    .build();
    let app = kanon_api::app(state.clone());
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(json!({"mode":"denylist", "allowlist":[], "denylist":[]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        state.bash_policy().get().mode,
        kanon_core::BashAccessMode::Allowlist
    );
}
