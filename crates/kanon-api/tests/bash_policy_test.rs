//! Management round-trip, restart and failed-write tests for the Bash settings.
mod common;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use kanon_core::instance::InstanceRegistry;
use kanon_core::{
    BashCaller, BashPolicy, BashPolicyStore, BashTool, CommandPolicyStore, Supervisor,
};
use serde_json::json;
use std::sync::Arc;

/// A Bash tool with default settings and no administrators, as the node assembles it.
fn bash_tool(root: &std::path::Path) -> Arc<BashTool> {
    Arc::new(
        BashTool::new(
            root.join("workspace"),
            Arc::new(BashPolicyStore::default()),
            Arc::new(CommandPolicyStore::default()),
            Arc::new(InstanceRegistry::in_memory()),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn bash_policy_round_trips_and_reloads_without_changing_the_tool_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let tool = bash_tool(dir.path());
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config.clone())
    .with_bash_tool(tool.clone())
    .build();
    let app = kanon_api::app(state.clone());
    let (_, before) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    let (_, initial) =
        common::send_json(&app, Method::GET, "/api/v1/tools/bash/policy", None).await;
    assert_eq!(
        initial,
        serde_json::to_value(BashPolicy::default()).unwrap()
    );
    assert_eq!(initial["enabled"], false, "Bash is opt-in");

    let mut update = initial.clone();
    update["enabled"] = json!(true);
    update["execution_mode"] = json!("local");
    update["sandbox"]["network"] = json!(false);
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, update);
    assert_eq!(
        tool.policy().get(),
        state.bash_policy().get(),
        "the console must publish into the store the tool enforces"
    );
    let restored = config.load_node_settings().unwrap();
    assert_eq!(restored.bash_policy, state.bash_policy().get());
    let (_, after) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    assert_eq!(
        before, after,
        "settings changes must not change tool definitions"
    );

    let mut invalid = update;
    invalid["local"]["review_model"] = json!("no-provider");
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
async fn bash_command_and_notice_updates_preserve_each_other_and_adapter_settings() {
    let dir = tempfile::tempdir().unwrap();
    let config = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let adapter = kanon_adapter_qqofficial::QqOfficialConfig {
        app_id: "test-app".into(),
        secret: Some("test-secret".into()),
        ..Default::default()
    };
    config.save_qqofficial(&adapter).unwrap();
    let tool = bash_tool(dir.path());
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config.clone())
    .with_bash_tool(tool.clone())
    .build();
    let app = kanon_api::app(state.clone());
    let notice = json!({"welcome_members":true, "reply_to_poke":true, "note_recalls":false});
    let mut bash = serde_json::to_value(BashPolicy::default()).unwrap();
    bash["enabled"] = json!(true);
    let commands = json!({"admins":["qqofficial:owner"]});
    // Every endpoint writes the same system.json document; none may drop another's section.
    for (route, body) in [
        ("/api/v1/system/event-policy", notice.clone()),
        ("/api/v1/tools/bash/policy", bash),
        ("/api/v1/system/command-policy", commands),
        ("/api/v1/system/event-policy", notice),
    ] {
        let (status, response) = common::send_json(&app, Method::PUT, route, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
    }
    // Administrators edited in the console are the ones Bash enforces.
    assert_eq!(tool.command_policy().get().admins, ["qqofficial:owner"]);
    let restored = config.load_node_settings().unwrap();
    assert_eq!(restored.bash_policy, state.bash_policy().get());
    assert_eq!(restored.command_policy, state.command_policy().get());
    assert!(restored.event_policy.welcome_members);
    assert!(!restored.event_policy.note_recalls);
    assert_eq!(config.load_qqofficial().unwrap(), Some(adapter));

    // Startup must repopulate the stores of a freshly assembled tool from the persisted settings.
    let restarted_tool = bash_tool(dir.path());
    let restarted = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config)
    .with_node_settings(restored)
    .with_bash_tool(restarted_tool.clone())
    .build();
    assert_eq!(restarted.event_policy().get(), state.event_policy().get());
    assert!(restarted_tool.policy().get().enabled);
    assert!(
        restarted_tool
            .command_policy()
            .get()
            .admins
            .iter()
            .any(|admin| admin == "qqofficial:owner")
    );
    let anonymous = kanon_core::with_bash_caller(None, restarted_tool.availability()).await;
    assert!(
        anonymous.contains("no single verified sender"),
        "{anonymous}"
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
    let mut enabled = serde_json::to_value(BashPolicy::default()).unwrap();
    enabled["enabled"] = json!(true);
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/tools/bash/policy",
        Some(enabled),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!state.bash_policy().get().enabled);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn endpoint_changes_require_reset_on_the_old_daemon_first() {
    use kanon_llm::AgentTool;
    let dir = tempfile::tempdir().unwrap();
    let policy = BashPolicy {
        enabled: true,
        ..Default::default()
    };
    let tool = bash_tool(dir.path());
    let config = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(config.clone())
    .with_node_settings(kanon_api::NodeSettings {
        bash_policy: policy.clone(),
        command_policy: kanon_core::CommandPolicy {
            admins: vec!["test:owner".into()],
            ..Default::default()
        },
        ..Default::default()
    })
    .with_bash_tool(tool.clone())
    .build();
    kanon_core::with_bash_caller(
        Some(BashCaller::new("test:owner")),
        tool.call("test", json!({"command":"true"})),
    )
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
