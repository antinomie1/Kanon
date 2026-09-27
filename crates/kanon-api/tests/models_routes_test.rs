//! Integration tests for the named-provider directory, the model catalog and the reply policy.
//!
//! These cover the console contract: providers are addressable by name, a model is referenced as
//! `<provider>/<model-id>`, per-model settings survive a round trip, and the reply policy is
//! configurable both node-wide and per instance.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use serde_json::{Value, json};

/// Builds state whose node system configuration lives inside an isolated directory.
///
/// Every test here mutates persistent settings, so the store must point at a temporary file rather
/// than the node's real `data/system.json`.
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

/// A provider endpoint that needs no network access to be constructed.
fn offline_provider(name: &str) -> Value {
    json!({
        "name": name,
        "protocol": "openai",
        "base_url": "http://127.0.0.1:9/v1",
        "api_key": "sk-unit-test"
    })
}

#[tokio::test]
async fn a_named_provider_becomes_the_default_with_a_qualified_model_reference() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    let mut body = offline_provider("xiaomi");
    body["make_default"] = json!(true);
    body["model"] = json!("mimo-v2.6-flash");

    let (status, payload) =
        common::send_json(&app, Method::POST, "/api/v1/providers", Some(body)).await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {payload}");
    assert_eq!(payload["default_provider"], json!("xiaomi"));
    assert_eq!(
        payload["default_model"],
        json!("xiaomi/mimo-v2.6-flash"),
        "a bare model id is qualified with the provider that serves it"
    );
    assert_eq!(payload["active"]["model"], json!("xiaomi/mimo-v2.6-flash"));
    assert_eq!(
        payload["active"]["upstream_model"],
        json!("mimo-v2.6-flash"),
        "the wire request must carry the bare id"
    );
    assert_eq!(payload["providers"][0]["name"], json!("xiaomi"));
    assert_eq!(payload["providers"][0]["is_default"], json!(true));
    assert_eq!(payload["providers"][0]["api_key_configured"], json!(true));

    // The running node observes the new endpoint immediately.
    let agent = state.agent().expect("agent installed");
    assert_eq!(agent.config().default_model, "mimo-v2.6-flash");
    assert_eq!(agent.config().provider.as_deref(), Some("xiaomi"));
}

#[tokio::test]
async fn a_model_catalog_entry_round_trips_and_can_be_deleted() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    // Seed a provider so the reference is meaningful.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(offline_provider("local")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/models",
        Some(json!({
            "provider": "local",
            "model": "vision-model",
            "context_length": 131072,
            "max_output_tokens": 8192,
            "capabilities": {
                "vision": true,
                "audio": false,
                "video": false,
                "tool_calling": true,
                "reasoning": true
            },
            "temperature": 0.4
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["models"][0]["model"], json!("vision-model"));
    assert_eq!(body["models"][0]["context_length"], json!(131072));
    assert_eq!(body["models"][0]["capabilities"]["vision"], json!(true));
    assert_eq!(
        body["models"][0]["source"],
        json!("manual"),
        "an operator-provided value is marked manual so discovery cannot overwrite it"
    );

    // Listing reflects the stored entry.
    let (status, listed) = common::send_json(&app, Method::GET, "/api/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["models"][0]["provider"], json!("local"));

    // Deleting it removes exactly that entry.
    let (status, after) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/models/delete",
        Some(json!({ "reference": "local/vision-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["total"], json!(0));

    // Deleting it again is reported rather than silently accepted.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/models/delete",
        Some(json!({ "reference": "local/vision-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_node_wide_reply_policy_round_trips() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/system/reply-policy",
        Some(json!({ "mode": "probability", "probability": 0.25 })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["policy"]["mode"], json!("probability"));
    assert_eq!(body["policy"]["probability"], json!(0.25));
    assert_eq!(state.reply_policy().get().probability, 0.25);

    let (status, body) =
        common::send_json(&app, Method::GET, "/api/v1/system/reply-policy", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policy"]["mode"], json!("probability"));

    // An out-of-range probability is rejected before anything is stored.
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/system/reply-policy",
        Some(json!({ "mode": "probability", "probability": 5.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_instance_carries_its_reply_policy_and_inherits_the_node_one() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    common::send_json(
        &app,
        Method::PUT,
        "/api/v1/system/reply-policy",
        Some(json!({ "mode": "mention", "probability": 0.5 })),
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "Policy Bot",
            "enabled": true,
            "adapters": ["policy"],
            "reply_policy": { "mode": "never", "probability": 0.5 }
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["instance"]["reply_policy"]["mode"], json!("never"));

    let (status, listed) = common::send_json(&app, Method::GET, "/api/v1/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["node_reply_policy"]["mode"],
        json!("mention"),
        "the node-wide policy is reported so the console can show what an instance inherits"
    );
    assert_eq!(
        listed["instances"][0]["reply_policy"]["mode"],
        json!("never")
    );
}

#[tokio::test]
async fn deleting_a_provider_removes_its_models() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(offline_provider("local")),
    )
    .await;
    common::send_json(
        &app,
        Method::PUT,
        "/api/v1/models",
        Some(json!({ "provider": "local", "model": "some-model" })),
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({ "name": "local" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    let (_, listed) = common::send_json(&app, Method::GET, "/api/v1/models", None).await;
    assert_eq!(
        listed["total"],
        json!(0),
        "a deleted endpoint must not leave catalog entries pointing at it"
    );
}

#[tokio::test]
async fn the_context_policy_round_trips_and_instances_override_it() {
    let dir = tempfile::tempdir().expect("config dir");
    let state = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/system/context-policy",
        Some(json!({ "include_sender_id": true, "include_timestamp": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["policy"]["include_sender_id"], json!(true));
    assert!(state.context_policy().get().include_sender_id);

    // The node value is visible on the system config payload as well.
    let (_, config) = common::send_json(&app, Method::GET, "/api/v1/system/config", None).await;
    assert_eq!(config["context_policy"]["include_sender_id"], json!(true));

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "Context Bot",
            "enabled": true,
            "adapters": ["context"],
            "context_policy": { "include_sender_id": false, "include_timestamp": true }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(
        body["instance"]["context_policy"]["include_timestamp"],
        json!(true)
    );

    let (_, listed) = common::send_json(&app, Method::GET, "/api/v1/instances", None).await;
    assert_eq!(
        listed["node_context_policy"]["include_sender_id"],
        json!(true),
        "the node-wide policy is reported so the console can show what an instance inherits"
    );
    assert_eq!(
        listed["instances"][0]["context_policy"]["include_sender_id"],
        json!(false)
    );
}
