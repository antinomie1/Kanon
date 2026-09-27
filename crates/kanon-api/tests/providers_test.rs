//! Integration tests for node-level model provider management.
//!
//! These cover the contract the management console depends on: a provider configured through the
//! API is validated, persisted next to the node's data (with the credential protected), and
//! applied to the *running* node — the pipeline and the chat endpoint observe it immediately,
//! without a restart.

mod common;

use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use serde_json::{Value, json};

/// Builds state whose node system configuration lives inside an isolated config directory.
///
/// The node starts with **no** provider, which is what makes the hot-apply assertions meaningful.
async fn provider_state(config_dir: PathBuf) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = std::sync::Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .with_system_config(std::sync::Arc::new(SystemConfigStore::new(
            config_dir.join("system.json"),
        )))
        .build()
}

/// A provider description that needs no network access to be *constructed* (only to be called).
fn offline_provider_body() -> Value {
    json!({
        "protocol": "openai",
        "base_url": "http://127.0.0.1:9/v1",
        "model": "unit-test-model",
        "api_key": "sk-unit-test"
    })
}

#[tokio::test]
async fn get_reports_unconfigured_node() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/providers", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active"]["configured"], json!(false));
    assert_eq!(body["active"]["source"], json!("none"));
    assert!(state.agent().is_none());
}

#[tokio::test]
async fn activate_persists_and_applies_without_restart() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    // Chat is disabled before a provider exists.
    let (before, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "provider-test", "message": "ping" })),
    )
    .await;
    assert_eq!(before, StatusCode::SERVICE_UNAVAILABLE);

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/providers/active",
        Some(offline_provider_body()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!(true));
    assert_eq!(body["active"]["configured"], json!(true));
    assert_eq!(body["active"]["source"], json!("console"));
    assert_eq!(body["active"]["model"], json!("local/unit-test-model"));
    assert_eq!(body["active"]["upstream_model"], json!("unit-test-model"));
    assert_eq!(body["active"]["protocol"], json!("openai"));
    assert_eq!(body["active"]["api_key_configured"], json!(true));

    // The running node observes the provider immediately: the pipeline and IPC gateway read the
    // same slot, so no restart is involved.
    let agent = state.agent().expect("agent installed on the running node");
    assert_eq!(agent.config().default_model, "unit-test-model");

    // Chat is no longer refused for lack of a provider. (The call itself fails because nothing
    // listens on the probe port, which is exactly the point: the provider is now being used.)
    let (after, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "provider-test", "message": "ping" })),
    )
    .await;
    assert_ne!(after, StatusCode::SERVICE_UNAVAILABLE);

    // The choice survives a restart because it is on disk, not in a browser.
    let store = SystemConfigStore::new(config_dir.path().join("system.json"));
    let persisted = store
        .load()
        .expect("load persisted config")
        .expect("provider persisted");
    assert_eq!(persisted.model, "unit-test-model");
    assert_eq!(persisted.api_key.as_deref(), Some("sk-unit-test"));

    // The credential is restricted to the node's own user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.path())
            .expect("stat system config")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "system config must not be world readable");
    }
}

#[tokio::test]
async fn invalid_protocol_is_rejected_before_anything_is_persisted() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/providers/active",
        Some(json!({
            "protocol": "definitely-not-a-protocol",
            "base_url": "https://example.invalid/v1",
            "model": "nope"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Unsupported protocol"),
        "unexpected error body: {body}"
    );

    // Validation happens before persistence and before the live node is touched.
    assert!(
        !config_dir.path().join("system.json").exists(),
        "a rejected provider must not reach disk"
    );
    assert!(
        state.agent().is_none(),
        "a rejected provider must not be applied"
    );
}

#[tokio::test]
async fn base_url_without_scheme_is_rejected() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state);

    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/providers/active",
        Some(json!({
            "protocol": "openai",
            "base_url": "api.deepseek.com/v1",
            "model": "deepseek-chat"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("http://"),
        "unexpected error body: {body}"
    );
}

#[tokio::test]
async fn clear_removes_persisted_provider_and_disables_chat() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    common::send_json(
        &app,
        Method::PUT,
        "/api/v1/providers/active",
        Some(offline_provider_body()),
    )
    .await;
    assert!(state.agent().is_some());

    let (status, body) =
        common::send_json(&app, Method::DELETE, "/api/v1/providers/active", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active"]["configured"], json!(false));
    assert_eq!(body["active"]["source"], json!("none"));
    assert!(
        state.agent().is_none(),
        "cleared provider must leave the node"
    );

    let store = SystemConfigStore::new(config_dir.path().join("system.json"));
    assert!(
        store.load().expect("load after clear").is_none(),
        "cleared provider must not remain persisted"
    );

    let (after, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "provider-test", "message": "ping" })),
    )
    .await;
    assert_eq!(after, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn injected_slot_still_receives_the_builder_provider() {
    // Regression guard: the composition root shares one slot across the gateway, the pipeline and
    // the IPC service. A provider handed to the builder must land *in* that slot — silently
    // ignoring it left the node reporting a provider it could not actually use.
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = std::sync::Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    let slot = std::sync::Arc::new(kanon_llm::AgentSlot::new());
    let state = ApiState::builder(supervisor)
        .with_agent_slot(slot.clone())
        .with_llm_provider(
            "bootstrap",
            std::sync::Arc::new(common::MockProvider::new("bootstrap reply")),
            kanon_api::default_agent_config("bootstrap-model"),
        )
        .build();

    assert!(
        slot.is_configured(),
        "the shared slot must observe the builder's provider"
    );
    let agent = state.agent().expect("state exposes the bootstrapped agent");
    assert_eq!(agent.config().default_model, "bootstrap-model");
}
