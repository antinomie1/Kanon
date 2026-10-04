//! Integration tests for node-level model provider management.
//!
//! These cover the contract the management console depends on. Providers are plain endpoints;
//! the node answers with exactly one *global default model*, set separately. A provider or default
//! configured through the API is validated, persisted next to the node's data (with the credential
//! protected), and applied to the *running* node — the pipeline and the chat endpoint observe it
//! immediately, without a restart.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, Method, StatusCode};
use kanon_api::{ApiState, SystemConfigStore};
use serde_json::{Value, json};

/// Builds state whose node system configuration lives inside an isolated config directory.
///
/// The node starts with **no** provider, which is what makes the hot-apply assertions meaningful.
async fn provider_state(config_dir: PathBuf) -> ApiState {
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

/// A provider endpoint that needs no network access to be *constructed* (only to be called).
fn offline_provider(name: &str) -> Value {
    json!({
        "name": name,
        "protocol": "openai",
        "base_url": "http://127.0.0.1:9/v1",
        "api_key": "sk-unit-test"
    })
}

async fn add_provider(app: &axum::Router, name: &str) -> Value {
    let (status, body) = common::send_json(
        app,
        Method::POST,
        "/api/v1/providers",
        Some(offline_provider(name)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    body
}

async fn set_default(app: &axum::Router, model: Value) -> (StatusCode, Value) {
    common::send_json(
        app,
        Method::PUT,
        "/api/v1/models/default",
        Some(json!({ "model": model })),
    )
    .await
}

#[tokio::test]
async fn an_unconfigured_node_lists_presets_and_no_providers() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    let (status, body) = common::send_json(&app, Method::GET, "/api/v1/providers", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["providers"], json!([]));
    assert!(body["presets"].as_array().is_some_and(|p| !p.is_empty()));
    assert!(body["available_protocols"].as_array().is_some());
    // The retired concepts are gone from the payload rather than reported as null: nothing in the
    // catalog can claim a provider is "active" or "the default".
    assert!(body.get("active").is_none(), "unexpected body: {body}");
    assert!(body.get("default_provider").is_none());
    assert!(state.agent().is_none());
}

#[tokio::test]
async fn the_reasoning_extension_is_selectable_and_persisted_for_custom_endpoints() {
    let config_dir = tempfile::tempdir().expect("temp dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    let (_, catalog) = common::send_json(&app, Method::GET, "/api/v1/providers", None).await;
    assert!(
        catalog["available_protocols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "openai_reasoning")
    );
    let preset = catalog["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "deepseek")
        .unwrap();
    assert_eq!(preset["protocol"], "openai_reasoning");

    let mut custom = offline_provider("reasoning-proxy");
    custom["protocol"] = json!("openai_reasoning");
    let (status, body) =
        common::send_json(&app, Method::POST, "/api/v1/providers", Some(custom)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["providers"][0]["protocol"], "openai_reasoning");
    assert_eq!(
        state.node_settings().providers[0].protocol,
        "openai_reasoning"
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(config_dir.path().join("system.json")).unwrap())
            .unwrap();
    assert_eq!(saved["providers"][0]["protocol"], "openai_reasoning");
}

#[tokio::test]
async fn saving_a_provider_never_picks_a_default_model() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    let body = add_provider(&app, "local").await;

    assert_eq!(body["providers"][0]["name"], json!("local"));
    assert_eq!(body["providers"][0]["api_key_configured"], json!(true));
    assert!(
        body["providers"][0].get("api_key").is_none(),
        "the credential must never be echoed to the console"
    );
    assert!(body["providers"][0].get("is_default").is_none());
    assert!(
        state.agent().is_none(),
        "adding an endpoint must not decide what answers"
    );

    let (_, models) = common::send_json(&app, Method::GET, "/api/v1/models", None).await;
    assert_eq!(models["default_model"], Value::Null);
}

#[tokio::test]
async fn the_default_model_is_persisted_and_applied_without_restart() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "local").await;

    // Chat is disabled before a default model exists.
    let (before, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "provider-test", "message": "ping" })),
    )
    .await;
    assert_eq!(before, StatusCode::SERVICE_UNAVAILABLE);

    let (status, body) = set_default(&app, json!("local/unit-test-model")).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["default_model"], json!("local/unit-test-model"));

    // The running node observes it immediately: the pipeline and IPC gateway read the same slot.
    let agent = state.agent().expect("agent installed on the running node");
    assert_eq!(agent.config().default_model, "unit-test-model");
    assert_eq!(agent.config().provider.as_deref(), Some("local"));

    // Chat is no longer refused for lack of a model. (The call itself fails because nothing
    // listens on the probe port, which is exactly the point: the model is now being used.)
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
    let persisted = store.load_node_settings().expect("load persisted config");
    assert_eq!(
        persisted.default_model.as_deref(),
        Some("local/unit-test-model")
    );
    assert_eq!(
        persisted.providers[0].api_key.as_deref(),
        Some("sk-unit-test")
    );
    let raw = std::fs::read_to_string(store.path()).expect("read document");
    assert!(
        !raw.contains("default_provider"),
        "no default provider is ever written: {raw}"
    );

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
async fn a_default_model_must_name_a_configured_provider() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "local").await;
    set_default(&app, json!("local/first")).await;

    let (status, body) = set_default(&app, json!("elsewhere/model")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("elsewhere"),
        "unexpected error body: {body}"
    );

    let (status, body) = set_default(&app, json!("no-provider-prefix")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");

    // A rejected choice leaves the previous default serving, in memory and on disk.
    assert_eq!(
        state.agent().expect("agent").config().model_ref(),
        "local/first"
    );
    let persisted = SystemConfigStore::new(config_dir.path().join("system.json"))
        .load_node_settings()
        .expect("reload");
    assert_eq!(persisted.default_model.as_deref(), Some("local/first"));
}

#[tokio::test]
async fn a_model_of_any_provider_can_be_the_default_and_switching_is_one_call() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "alpha").await;
    add_provider(&app, "beta").await;

    set_default(&app, json!("alpha/model-a")).await;
    assert_eq!(state.agent().unwrap().config().model_ref(), "alpha/model-a");

    let (status, body) = set_default(&app, json!("beta/model-b")).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(
        state.agent().unwrap().config().model_ref(),
        "beta/model-b",
        "switching provider and model is a single decision"
    );
}

#[tokio::test]
async fn clearing_the_default_model_disables_chat_but_keeps_the_providers() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "local").await;
    set_default(&app, json!("local/some-model")).await;
    assert!(state.agent().is_some());

    let (status, body) = set_default(&app, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["default_model"], Value::Null);
    assert!(state.agent().is_none(), "no default model, no agent");

    let (_, providers) = common::send_json(&app, Method::GET, "/api/v1/providers", None).await;
    assert_eq!(providers["providers"][0]["name"], json!("local"));

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
async fn deleting_the_provider_serving_the_default_clears_it() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "alpha").await;
    add_provider(&app, "beta").await;
    set_default(&app, json!("alpha/model-a")).await;

    // Deleting an unrelated endpoint leaves the default alone.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({ "name": "beta" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.agent().unwrap().config().model_ref(), "alpha/model-a");

    // Deleting the serving endpoint clears the default instead of leaving it dangling.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({ "name": "alpha" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.agent().is_none());

    let (_, models) = common::send_json(&app, Method::GET, "/api/v1/models", None).await;
    assert_eq!(models["default_model"], Value::Null);
    let persisted = SystemConfigStore::new(config_dir.path().join("system.json"))
        .load_node_settings()
        .expect("reload");
    assert!(persisted.providers.is_empty());
    assert!(persisted.default_model.is_none());

    // Deleting it again is reported rather than silently accepted.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({ "name": "alpha" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_providers_are_rejected_before_anything_is_persisted() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());

    let cases = [
        (
            json!({ "name": "odd", "protocol": "definitely-not-a-protocol", "base_url": "https://example.invalid/v1" }),
            "Unsupported protocol",
        ),
        (
            json!({ "name": "odd", "protocol": "openai", "base_url": "api.deepseek.com/v1" }),
            "http://",
        ),
        (
            json!({ "name": "  ", "protocol": "openai", "base_url": "https://example.invalid/v1" }),
            "name",
        ),
        (
            json!({ "name": "a/b", "protocol": "openai", "base_url": "https://example.invalid/v1" }),
            "'/'",
        ),
    ];

    for (payload, expected) in cases {
        let (status, body) =
            common::send_json(&app, Method::POST, "/api/v1/providers", Some(payload)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(expected),
            "expected '{expected}' in error body: {body}"
        );
    }

    // Validation happens before persistence and before the live node is touched.
    assert!(
        !config_dir.path().join("system.json").exists(),
        "a rejected provider must not reach disk"
    );
    assert!(state.agent().is_none());
}

#[tokio::test]
async fn editing_a_provider_without_a_key_keeps_the_stored_credential() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "local").await;

    // The console never receives the secret, so an edit omits it.
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(json!({
            "name": "local",
            "protocol": "openai",
            "base_url": "http://127.0.0.1:9/v1",
            "temperature": 0.4
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(
        body["providers"][0]["base_url"],
        json!("http://127.0.0.1:9/v1")
    );
    assert_eq!(body["providers"][0]["api_key_configured"], json!(true));

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(json!({
            "name": "local",
            "protocol": "openai",
            "base_url": "http://127.0.0.1:9/v1",
            "clear_api_key": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["providers"][0]["api_key_configured"], json!(false));
}

/// Requests an OpenAI-compatible stub server captured, oldest first.
type Captured = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

/// Starts a local OpenAI-compatible endpoint that records every request it receives.
async fn spawn_openai_stub() -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(
            move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                let sink = sink.clone();
                async move {
                    sink.lock().expect("capture lock").push((headers, body));
                    axum::Json(json!({
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": "pong" },
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), captured)
}

#[tokio::test]
async fn testing_a_configured_provider_uses_its_stored_credential_on_the_server() {
    let (base_url, captured) = spawn_openai_stub().await;
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state);

    common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(json!({
            "name": "stub",
            "protocol": "openai",
            "base_url": base_url,
            "api_key": "sk-stored"
        })),
    )
    .await;

    // No coordinates and no key: the server looks the endpoint up by name.
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({ "provider": "stub", "model": "stub-model" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["reply"], json!("pong"));
    assert_eq!(body["model"], json!("stub-model"));

    let requests = captured.lock().expect("capture lock");
    let (headers, sent) = requests.first().expect("the stub received a request");
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer sk-stored"),
        "the stored credential must reach the endpoint"
    );
    assert_eq!(sent["model"], json!("stub-model"));
}

#[tokio::test]
async fn a_typed_key_overrides_the_stored_one_and_the_model_defaults_sensibly() {
    let (base_url, captured) = spawn_openai_stub().await;
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state);

    common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(json!({
            "name": "stub",
            "protocol": "openai",
            "base_url": base_url,
            "api_key": "sk-stored"
        })),
    )
    .await;
    common::send_json(
        &app,
        Method::PUT,
        "/api/v1/models",
        Some(json!({ "provider": "stub", "model": "catalog-model" })),
    )
    .await;

    // No model given: the endpoint's first catalog model is used.
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({ "provider": "stub", "api_key": "sk-typed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(body["model"], json!("catalog-model"));

    let requests = captured.lock().expect("capture lock");
    assert_eq!(
        requests[0]
            .0
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some("Bearer sk-typed")
    );
}

#[tokio::test]
async fn testing_needs_a_provider_a_model_or_explicit_coordinates() {
    let config_dir = tempfile::tempdir().expect("config dir");
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state);

    // Nothing to test at all.
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");

    // An unknown provider is a 404, not a silent fallback to some other endpoint.
    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({ "provider": "ghost", "model": "m" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A configured endpoint with no model to try says so instead of inventing one.
    add_provider(&app, "empty").await;
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({ "provider": "empty" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("model"),
        "unexpected error body: {body}"
    );
}

#[tokio::test]
async fn injected_slot_still_receives_the_builder_provider() {
    // Regression guard: the composition root shares one slot across the gateway, the pipeline and
    // the IPC service. A provider handed to the builder must land *in* that slot — silently
    // ignoring it left the node reporting a provider it could not actually use.
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    let slot = Arc::new(kanon_llm::AgentSlot::new());
    let state = ApiState::builder(supervisor)
        .with_agent_slot(slot.clone())
        .with_llm_provider(
            "bootstrap",
            Arc::new(common::MockProvider::new("bootstrap reply")),
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

#[tokio::test]
async fn reasoning_replay_setting_is_persisted_and_omission_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = provider_state(dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    let initial = add_provider(&app, "fixture").await;
    assert_eq!(initial["providers"][0]["replay_reasoning"], true);
    for preference in [Some(false), None, Some(true)] {
        let mut payload = offline_provider("fixture");
        if let Some(value) = preference {
            payload["replay_reasoning"] = json!(value);
        }
        let (status, body) =
            common::send_json(&app, Method::POST, "/api/v1/providers", Some(payload)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let expected = preference.unwrap_or(false);
        assert_eq!(body["providers"][0]["replay_reasoning"], expected);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("system.json")).unwrap())
                .unwrap();
        assert_eq!(saved["providers"][0]["replay_reasoning"], expected);
        assert_eq!(
            state.node_settings().providers[0].replay_reasoning,
            expected
        );
    }
}

/// URL edits must not leak a stored credential through probing or automatic discovery.
#[tokio::test]
async fn changed_endpoint_requires_an_explicit_credential_decision() {
    let config_dir = tempfile::tempdir().unwrap();
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "local").await;
    let (target, captured) = spawn_openai_stub().await;

    for path in ["/api/v1/providers/test", "/api/v1/providers"] {
        let (status, body) = common::send_json(
            &app,
            Method::POST,
            path,
            Some(json!({
                "provider": "local", "name": "local", "protocol": "openai",
                "base_url": target, "model": "probe"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
    }
    assert!(captured.lock().unwrap().is_empty());
    assert_eq!(
        state.node_settings().providers[0].base_url,
        "http://127.0.0.1:9/v1"
    );

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/test",
        Some(json!({
            "provider": "local", "base_url": target, "model": "probe", "api_key": "sk-new"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        captured.lock().unwrap()[0].0["authorization"],
        "Bearer sk-new"
    );

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers",
        Some(json!({
            "name": "local", "protocol": "openai", "base_url": target, "clear_api_key": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!state.node_settings().providers[0].has_api_key());
}

/// Identical upstream model names on different endpoints must stay distinguishable.
#[tokio::test]
async fn chat_override_selects_its_provider_and_rejects_unresolved_references() {
    let config_dir = tempfile::tempdir().unwrap();
    let state = provider_state(config_dir.path().to_path_buf()).await;
    let app = kanon_api::app(state);
    let (first_url, first) = spawn_openai_stub().await;
    let (second_url, second) = spawn_openai_stub().await;
    for (name, url) in [("first", first_url), ("second", second_url)] {
        let (status, body) = common::send_json(
            &app,
            Method::POST,
            "/api/v1/providers",
            Some(json!({
                "name": name, "protocol": "openai", "base_url": url
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(
        set_default(&app, json!("first/shared-model")).await.0,
        StatusCode::OK
    );
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "selected-provider", "message": "ping", "model": "second/shared-model"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(first.lock().unwrap().is_empty());
    assert_eq!(second.lock().unwrap()[0].1["model"], "shared-model");

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "unknown-provider", "message": "ping", "model": "missing/shared-model"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(first.lock().unwrap().is_empty());
    assert_eq!(second.lock().unwrap().len(), 1);
}

/// Deleting an endpoint cannot orphan saved instance or Bash review model references.
#[tokio::test]
async fn provider_deletion_preserves_referenced_instances_and_review_models() {
    let dir = tempfile::tempdir().unwrap();
    let state = provider_state(dir.path().to_path_buf()).await;
    let app = kanon_api::app(state.clone());
    add_provider(&app, "alpha").await;
    let (status, created) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({"name": "Bot", "enabled": false, "model": "alpha/model"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["instance"]["id"].as_str().unwrap();
    let before = std::fs::read(dir.path().join("system.json")).unwrap();
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({"name": "alpha"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(state.node_settings().providers.len(), 1);
    assert_eq!(
        std::fs::read(dir.path().join("system.json")).unwrap(),
        before
    );
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/instances/{id}"),
        Some(json!({"name": "Bot", "enabled": false, "model": "ghost/model"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        state.instances().get(id).await.unwrap().model.as_deref(),
        Some("alpha/model")
    );
    let (status, body) = common::send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/instances/{id}"),
        Some(json!({"name": "Bot", "enabled": false, "model": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    state
        .update_node_settings(|settings| {
            settings.bash_policy.local.review_model = Some("alpha/model".into());
            Ok(())
        })
        .unwrap();
    let before = std::fs::read(dir.path().join("system.json")).unwrap();
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({"name": "alpha"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(state.node_settings().providers.len(), 1);
    assert_eq!(
        std::fs::read(dir.path().join("system.json")).unwrap(),
        before
    );
    state
        .update_node_settings(|settings| {
            settings.bash_policy.local.review_model = None;
            Ok(())
        })
        .unwrap();
    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/providers/delete",
        Some(json!({"name": "alpha"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(state.node_settings().providers.is_empty());
}
