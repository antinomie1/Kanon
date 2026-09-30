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
        serde_json::to_value(kanon_core::BashPolicy::default()).unwrap()
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
    let mut expected = update;
    let defaults = serde_json::to_value(kanon_core::BashPolicy::default()).unwrap();
    for key in ["sandbox", "execution_mode", "local"] {
        expected[key] = defaults[key].clone();
    }
    assert_eq!(body, expected);
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

#[tokio::test]
async fn older_permission_clients_preserve_saved_sandbox_network_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(Arc::new(SystemConfigStore::new(
        dir.path().join("system.json"),
    )))
    .build();
    let app = kanon_api::app(state.clone());
    let mut initial = serde_json::to_value(kanon_core::BashPolicy::default()).unwrap();
    initial["sandbox"]["network"] = json!(false);
    initial["sandbox"]["memory_mb"] = json!(256);
    initial["execution_mode"] = json!("local");
    initial["local"]["auto_review"] = json!(false);
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(initial),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, after) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(json!({"mode":"denylist", "allowlist":[], "denylist":[]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["sandbox"]["network"], false);
    assert_eq!(after["sandbox"]["memory_mb"], 256);
    assert_eq!(after["execution_mode"], "local");
    assert_eq!(after["local"]["auto_review"], false);
    assert_eq!(state.bash_policy().get().sandbox.memory_mb, 256);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn endpoint_changes_require_reset_on_the_old_daemon_first() {
    use kanon_llm::AgentTool;
    let dir = tempfile::tempdir().unwrap();
    let caller = BashPrincipal {
        platform: "test".into(),
        user_id: "owner".into(),
    };
    let policy = kanon_core::BashPolicy {
        allowlist: vec![caller.clone()],
        ..Default::default()
    };
    let policy_store = Arc::new(BashPolicyStore::new(policy.clone()));
    let tool =
        Arc::new(kanon_core::BashTool::new(dir.path().join("workspace"), policy_store).unwrap());
    let config = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config.clone())
    .with_node_settings(kanon_api::NodeSettings {
        bash_policy: policy.clone(),
        ..Default::default()
    })
    .with_bash_tool(tool.clone())
    .build();
    kanon_core::with_bash_caller(caller, tool.call("test", json!({"command":"true"})))
        .await
        .unwrap();
    let app = kanon_api::app(state.clone());
    let mut next = serde_json::to_value(policy.clone()).unwrap();
    next["sandbox"]["endpoint"] = json!(format!(
        "unix://{}",
        dir.path().join("another-daemon.sock").display()
    ));
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(next.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        state.bash_policy().get().sandbox.endpoint,
        policy.sandbox.endpoint
    );
    let (status, _) = common::send_json(&app, Method::POST, "/api/v1/tools/bash/reset", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(next.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::to_value(config.load_node_settings().unwrap().bash_policy).unwrap(),
        next
    );
}
