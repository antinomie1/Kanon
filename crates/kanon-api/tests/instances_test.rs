//! Integration tests for bot-instance management over the management gateway.
//!
//! The instance catalog is what turns "an adapter is configured" into "a bot answers": these tests
//! cover creation, owner uniqueness per adapter, persona publication and the delete path.

mod common;

use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

/// Instance payload fixture: `adapters` and `enabled` are the fields under test.
fn instance_body(name: &str, enabled: bool, adapters: &[&str]) -> Value {
    json!({
        "name": name,
        "enabled": enabled,
        "adapters": adapters,
    })
}

/// Lists instances through the API.
async fn list(app: &axum::Router) -> Value {
    let (status, body) = common::send_json(app, Method::GET, "/api/v1/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn empty_catalog_reports_the_gate_as_closed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    let body = list(&app).await;

    assert_eq!(body["total"], json!(0));
    assert_eq!(body["enabled"], json!(0));
    assert_eq!(body["instances"], json!([]));
}

#[tokio::test]
async fn create_reports_live_adapter_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("QQ Bot", true, &["fixture_platform"])),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!(true));

    let instance = &body["instance"];
    assert_eq!(instance["enabled"], json!(true));
    let adapters = instance["adapter_status"]
        .as_array()
        .expect("adapter status");
    assert_eq!(adapters.len(), 1);
    assert_eq!(adapters[0]["known"], json!(true));
    assert_eq!(adapters[0]["kind"], json!("plugin"));

    let listed = list(&app).await;
    assert_eq!(listed["total"], json!(1));
    assert_eq!(listed["enabled"], json!(1));
}

#[tokio::test]
async fn an_adapter_cannot_be_enabled_by_two_instances() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("First", true, &["fixture_platform"])),
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("Second", true, &["fixture_platform"])),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already enabled"),
        "unexpected error body: {body}"
    );
    assert_eq!(list(&app).await["total"], json!(1));

    // A disabled instance may hold the adapter, because it serves nothing.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("Second", false, &["fixture_platform"])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_persona_is_rejected_before_anything_is_stored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "Typo Bot",
            "enabled": true,
            "adapters": ["fixture_platform"],
            "persona_id": "does-not-exist",
        })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not exist"),
        "unexpected error body: {body}"
    );
    assert_eq!(list(&app).await["total"], json!(0));
}

#[tokio::test]
async fn custom_prompt_is_published_as_a_persona_and_survives_deletion_cleanup() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state.clone());

    let (status, created) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "黑猪AI",
            "enabled": true,
            "adapters": ["fixture_platform"],
            "system_prompt": "你是一只叫黑猪AI的猪，用中文回答。",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let instance_id = created["instance"]["id"].as_str().expect("id").to_string();

    // The instance prompt shows up in the persona catalog, so prompt composition needs no special
    // case for instances.
    let (_, personas) = common::send_json(&app, Method::GET, "/api/v1/personas", None).await;
    let expected_persona = format!("instance:{instance_id}");
    assert!(
        personas["personas"]
            .as_array()
            .expect("personas")
            .iter()
            .any(|persona| persona["id"] == json!(expected_persona)),
        "instance persona missing from {personas}"
    );

    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/instances/{instance_id}"),
        Some(json!({"name": "Bot", "enabled": true, "model": "unqualified-model"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        state
            .instances()
            .get(&instance_id)
            .await
            .unwrap()
            .model
            .is_none()
    );

    // An update may also reshape the fields the console edits.
    let (status, updated) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/instances/{instance_id}"),
        Some(json!({
            "name": "黑猪AI",
            "enabled": true,
            "adapters": ["fixture_platform"],
            "system_prompt": "你是一只叫黑猪AI的猪。",
            "model": "deepseek/deepseek-flash",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        updated["instance"]["model"],
        json!("deepseek/deepseek-flash")
    );

    // Explicit selections of a generated persona are also unbound, but never during a busy turn.
    state
        .sessions()
        .set_persona("debug", &expected_persona)
        .unwrap();
    let writer = state.sessions().try_write("debug").unwrap();
    let (status, _) = common::send_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/instances/{instance_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(state.instances().get(&instance_id).await.is_some());
    drop(writer);

    // Deleting the instance removes its generated persona again.
    let (status, deleted) = common::send_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/instances/{instance_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["applied"], json!(true));
    assert_eq!(list(&app).await["total"], json!(0));
    assert!(state.sessions().get_persona("debug").is_none());

    let (_, personas) = common::send_json(&app, Method::GET, "/api/v1/personas", None).await;
    assert!(
        !personas["personas"]
            .as_array()
            .expect("personas")
            .iter()
            .any(|persona| persona["id"] == json!(expected_persona)),
        "stale instance persona survived deletion: {personas}"
    );
}

#[tokio::test]
async fn clearing_an_instance_prompt_rejects_surviving_references_before_unbinding() {
    let dir = tempfile::tempdir().unwrap();
    let state = common::empty_state(dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    for body in [
        json!({"name":"Owner", "system_prompt":"owned prompt"}),
        json!({"name":"Reader", "persona_id":"instance:owner"}),
    ] {
        let (status, body) =
            common::send_json(&app, Method::POST, "/api/v1/instances", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    state
        .sessions()
        .set_persona("debug", "instance:owner")
        .unwrap();
    for (method, body) in [
        (Method::DELETE, None),
        (Method::PUT, Some(json!({"name":"Owner"}))),
    ] {
        let (status, body) = common::send_json(&app, method, "/api/v1/instances/owner", body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            state.sessions().get_persona("debug").as_deref(),
            Some("instance:owner")
        );
    }
    let (status, _) =
        common::send_json(&app, Method::DELETE, "/api/v1/instances/reader", None).await;
    assert_eq!(status, StatusCode::OK);

    // A surviving self-reference is invalid too: clearing the prompt would delete its target.
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/instances/owner",
        Some(json!({"name":"Owner", "persona_id":"instance:owner"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/instances/owner",
        Some(json!({"name":"Owner"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        state
            .instances()
            .get("owner")
            .await
            .unwrap()
            .system_prompt
            .is_none()
    );
    assert!(state.personas().get("instance:owner").is_none());
    assert!(state.sessions().get_persona("debug").is_none());
}

#[tokio::test]
async fn updating_a_missing_instance_is_a_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/instances/nope",
        Some(instance_body("Nope", true, &["fixture_platform"])),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_reports_instance_counts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("Live", true, &["fixture_platform"])),
    )
    .await;
    common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(instance_body("Idle", false, &[])),
    )
    .await;

    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/health", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["instances"]["total"], json!(2));
    assert_eq!(body["instances"]["enabled"], json!(1));
}

#[tokio::test]
async fn command_permissions_and_bash_scope_are_stored_per_instance() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = common::fixture_state(PathBuf::from(dir.path()), true).await;
    let app = kanon_api::app(state);

    // A malformed administrator is refused before anything reaches the catalog.
    let mut invalid = instance_body("Ops Bot", false, &[]);
    invalid["command_policy"] = json!({"admins": ["no-platform"]});
    let (status, _) =
        common::send_json(&app, Method::POST, "/api/v1/instances", Some(invalid)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(list(&app).await["total"], json!(0));

    // A valid override is stored as the node enforces it: trimmed admins, bare command names.
    let mut body = instance_body("Ops Bot", false, &[]);
    body["command_policy"] = json!({
        "admins": [" onebot:1 "],
        "group_admins_are_admins": false,
        "access": {"/Weather": "admins"},
    });
    body["bash"] = json!("shared_context");
    let (status, created) =
        common::send_json(&app, Method::POST, "/api/v1/instances", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    let instance = &created["instance"];
    assert_eq!(instance["bash"], json!("shared_context"));
    assert_eq!(
        instance["command_policy"],
        json!({
            "admins": ["onebot:1"],
            "group_admins_are_admins": false,
            "access": {"weather": "admins"},
        })
    );

    // The listing carries what an inheriting instance would get, for the console's hints.
    let listed = list(&app).await;
    assert_eq!(listed["node_command_policy"]["admins"], json!([]));
    assert_eq!(listed["node_bash_enabled"], json!(false));
    assert_eq!(
        listed["instances"][0]["command_policy"],
        instance["command_policy"]
    );
}
