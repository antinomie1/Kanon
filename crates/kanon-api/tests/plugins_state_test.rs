//! Tests for plugin enable/disable over the management gateway.
//!
//! Disabling is the operator's "turn this off" switch: the host process must stop (releasing its
//! memory and removing its pre-filters, commands, tools and adapter) while the catalog keeps
//! listing the plugin so it can be enabled again.

mod common;

use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, ToggleStore};
use serde_json::{Value, json};

/// Manifest written to the scratch plugin directory so the catalog can rediscover a stopped host.
const MANIFEST: &str = r#"
[plugin]
id = "org.kanon.plugin.fixture"
name = "Fixture Plugin"
version = "2.1.0"
author = "Kanon Test"
description = "Fixture plugin used by API integration tests"
runtime = "rust"
entrypoint = "target/debug/fixture"
priority = 120
"#;

/// Builds gateway state whose plugin directory mirrors what the supervisor is running, plus a
/// persisted state store the test can inspect.
async fn plugin_state_fixture(config_dir: PathBuf) -> (ApiState, std::sync::Arc<ToggleStore>) {
    let plugins_dir = config_dir.join("plugins").join("fixture");
    std::fs::create_dir_all(&plugins_dir).expect("create plugin dir");
    std::fs::write(plugins_dir.join("plugin.toml"), MANIFEST).expect("write manifest");

    let store = std::sync::Arc::new(
        ToggleStore::open(config_dir.join("toggles.json"))
            .await
            .expect("open state store"),
    );

    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = std::sync::Arc::new(kanon_core::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    common::register_fixture_host(&supervisor).await;

    let state = ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .with_plugins_dir(config_dir.join("plugins"))
        .with_plugin_state(store.clone())
        .build();
    // The startup scan the composition root runs before serving.
    state.rescan_plugins().expect("scan plugin directory");

    (state, store)
}

/// Finds one plugin entry in the catalog body.
fn plugin_entry(body: &Value, plugin_id: &str) -> Value {
    body["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .find(|plugin| plugin["id"] == json!(plugin_id))
        .unwrap_or_else(|| panic!("plugin '{plugin_id}' missing from {body}"))
        .clone()
}

#[tokio::test]
async fn disabling_stops_the_host_and_keeps_the_plugin_listed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, store) = plugin_state_fixture(PathBuf::from(dir.path())).await;
    let app = kanon_api::app(state.clone());

    // The fixture host is running, so the plugin starts enabled.
    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    let view = plugin_entry(&body, common::FIXTURE_PLUGIN_ID);
    assert_eq!(view["enabled"], json!(true));
    assert_eq!(view["status"], json!("running"));

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/plugins/{}/enabled", common::FIXTURE_PLUGIN_ID),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["applied"], json!(true));
    assert_eq!(body["enabled"], json!(false));

    // The host is gone, so nothing can route to the plugin any more…
    assert!(
        state
            .supervisor()
            .find_host_for_plugin(common::FIXTURE_PLUGIN_ID)
            .await
            .is_none(),
        "disabling must stop the host process"
    );
    // …the choice is persisted…
    assert!(
        !store
            .is_enabled(kanon_core::PLUGIN_SECTION, common::FIXTURE_PLUGIN_ID)
            .await
    );
    // …and the catalog still lists it as disabled, so the console can re-enable it.
    let (_, body) = common::send_json(&app, Method::GET, "/api/v1/plugins", None).await;
    let view = plugin_entry(&body, common::FIXTURE_PLUGIN_ID);
    assert_eq!(view["enabled"], json!(false));
    assert_eq!(view["status"], json!("disabled"));

    // Restarting a disabled plugin is a conflict, not a silent re-enable.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{}/restart", common::FIXTURE_PLUGIN_ID),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn disabling_twice_reports_that_nothing_changed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _store) = plugin_state_fixture(PathBuf::from(dir.path())).await;
    let app = kanon_api::app(state);

    let uri = format!("/api/v1/plugins/{}/enabled", common::FIXTURE_PLUGIN_ID);
    let (status, _) =
        common::send_json(&app, Method::PUT, &uri, Some(json!({ "enabled": false }))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) =
        common::send_json(&app, Method::PUT, &uri, Some(json!({ "enabled": false }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!(false), "idempotent call: {body}");
}

#[tokio::test]
async fn an_unknown_plugin_cannot_be_toggled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _store) = plugin_state_fixture(PathBuf::from(dir.path())).await;
    let app = kanon_api::app(state);

    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/plugins/does.not.exist/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn enabling_a_stopped_plugin_reports_a_start_failure_instead_of_pretending() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, store) = plugin_state_fixture(PathBuf::from(dir.path())).await;
    let app = kanon_api::app(state.clone());

    // Disable first so the enabling path must actually launch a process.
    common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/plugins/{}/enabled", common::FIXTURE_PLUGIN_ID),
        Some(json!({ "enabled": false })),
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/plugins/{}/enabled", common::FIXTURE_PLUGIN_ID),
        Some(json!({ "enabled": true })),
    )
    .await;

    // The fixture manifest points at a binary that does not exist, so the start must fail loudly
    // rather than reporting success and leaving the operator with a plugin that never runs.
    assert_eq!(status, StatusCode::BAD_GATEWAY, "unexpected body: {body}");
    assert_eq!(body["error"]["code"], json!("upstream_error"));

    // Intent is recorded even though the launch failed: the operator asked for this plugin to be
    // enabled, and a transient cause (missing runtime, not yet built) must not silently erase that.
    assert!(
        store
            .is_enabled(kanon_core::PLUGIN_SECTION, common::FIXTURE_PLUGIN_ID)
            .await
    );

    // The catalog reflects the mismatch honestly: enabled, but without a running host.
    let (_, catalog) = common::send_json(&app, Method::GET, "/api/v1/plugins", None).await;
    let view = plugin_entry(&catalog, common::FIXTURE_PLUGIN_ID);
    assert_eq!(view["enabled"], json!(true));
    assert_ne!(view["status"], json!("running"));
}

#[tokio::test]
async fn a_plugin_copied_in_by_hand_appears_only_after_a_rescan() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _store) = plugin_state_fixture(PathBuf::from(dir.path())).await;
    let app = kanon_api::app(state);

    let added = dir.path().join("plugins").join("added");
    std::fs::create_dir_all(&added).expect("create plugin dir");
    std::fs::write(
        added.join("plugin.toml"),
        MANIFEST
            .replace("org.kanon.plugin.fixture", "org.kanon.plugin.added")
            .replace("Fixture Plugin", "Added Plugin"),
    )
    .expect("write manifest");

    // Reading the catalog does not touch the directory, so the new folder is not listed yet…
    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&Value> = body["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .map(|plugin| &plugin["id"])
        .collect();
    assert!(
        !ids.contains(&&json!("org.kanon.plugin.added")),
        "listed before a rescan: {body}"
    );

    // …and cannot be enabled, because the node does not know it exists.
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/plugins/org.kanon.plugin.added/enabled",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The operator's rescan finds it and answers with the refreshed catalog.
    let (status, body) =
        common::send_json(&app, Method::POST, "/api/v1/plugins/rescan", None).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    let view = plugin_entry(&body, "org.kanon.plugin.added");
    assert_ne!(view["status"], json!("running"));
    // The running fixture is still there next to it.
    plugin_entry(&body, common::FIXTURE_PLUGIN_ID);
}
