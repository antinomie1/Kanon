//! OneBot adapter management: persistence, validation and write-only credentials.
//!
//! These tests exercise the control plane the way a console does — over HTTP, against a real
//! `data/system.json` on disk — because the two failure modes that matter here are invisible to a
//! unit test: a configuration that is applied but not persisted (silently lost on restart), and a
//! credential that is dropped by an unrelated edit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use kanon_adapter_onebot::{OneBotAdapter, OneBotConfig};
use kanon_api::{ApiState, SystemConfigStore, app};
use kanon_core::EventIngress;
use kanon_core::supervisor::Supervisor;
use kanon_proto::v1::IngestEventRequest;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;

use common::send_json;

mod common;

/// Builds gateway state hosting a started OneBot adapter whose configuration persists into `dir`.
///
/// The returned receiver keeps the ingest channel open for the duration of the test; a closed
/// channel would make the adapter's connection pointless and is not what the node does.
async fn onebot_state(
    dir: &Path,
    config: OneBotConfig,
) -> (
    ApiState,
    Arc<OneBotAdapter>,
    mpsc::Receiver<IngestEventRequest>,
) {
    let supervisor = Arc::new(Supervisor::new(Some(dir.join("run")), None));

    let store = Arc::new(SystemConfigStore::new(dir.join("system.json")));
    let adapter = Arc::new(OneBotAdapter::new(config).expect("adapter should build"));
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .expect("adapter should register");

    let state = ApiState::builder(supervisor.clone())
        .with_config_dir(dir.to_path_buf())
        .with_system_config(store)
        .with_onebot_adapter(adapter.clone())
        .build();

    // Mirror the composition root: the ingest queue arrives through `start_all`, which is what
    // lets a subsequently enabled adapter open a connection it can actually feed.
    let (ingest_tx, ingest_rx) = mpsc::channel(8);
    supervisor
        .adapters()
        .start_all(EventIngress::new(ingest_tx))
        .await;

    (state, adapter, ingest_rx)
}

/// Reads the persisted node configuration document.
fn persisted(dir: &TempDir) -> Value {
    let raw = std::fs::read_to_string(dir.path().join("system.json"))
        .expect("the configuration document should exist");
    serde_json::from_str(&raw).expect("the configuration document should be valid JSON")
}

/// Creates a temporary directory holding a configuration document.
fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// A fresh node reports the default configuration and a disabled adapter.
#[tokio::test]
async fn read_reports_defaults_when_nothing_is_configured() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    let (status, body) =
        send_json(&router, Method::GET, "/api/v1/adapters/onebot/config", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["config"]["enabled"], false);
    assert_eq!(body["config"]["platform"], "onebot");
    assert_eq!(body["config"]["ws_url"], "ws://127.0.0.1:6700");
    assert_eq!(body["config"]["transport"], "forward_websocket");
    assert_eq!(body["status"]["connection_state"], "disabled");
    assert_eq!(body["status"]["connected"], false);
    assert!(
        body["config"].get("access_token").is_none(),
        "the credential must never be reported"
    );
}

/// Saving reverse mode updates both the live configuration and its persisted section.
#[tokio::test]
async fn update_applies_and_persists_the_configuration() {
    let dir = temp_dir();
    std::fs::write(
        dir.path().join("system.json"),
        r#"{"unrelated":{"keep":true}}"#,
    )
    .unwrap();
    let (state, adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);
    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "ws_url": "ws://127.0.0.1:6701/onebot",
            "transport": "reverse_websocket",
            "access_token": "s3cret"
        })),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["config"]["transport"], "reverse_websocket");
    assert!(body["config"].get("access_token").is_none());
    assert_eq!(body["status"]["token_configured"], true);
    assert_eq!(adapter.config().ws_url, "ws://127.0.0.1:6701/onebot");
    let document = persisted(&dir);
    assert_eq!(document["onebot"]["access_token"], "s3cret");
    assert_eq!(document["unrelated"]["keep"], true);
    let restored = SystemConfigStore::new(dir.path().join("system.json"))
        .load_onebot()
        .unwrap()
        .unwrap();
    assert_eq!(restored.ws_url, adapter.config().ws_url);
}

/// A rejected configuration leaves both the running adapter and the file untouched.
#[tokio::test]
async fn invalid_update_changes_nothing() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": true,
            "platform": "onebot",
            "ws_url": "not-a-url",
        })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "bad_request");
    assert!(!adapter.config().enabled);
    assert!(
        !dir.path().join("system.json").exists(),
        "a rejected configuration must not be persisted"
    );
}

/// The adapter's identity is fixed while the node runs, and the API says so.
#[tokio::test]
async fn identity_changes_are_rejected() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "qq",
            "ws_url": "ws://127.0.0.1:6700",
        })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "bad_request");
}

/// Editing an unrelated field preserves the stored credential; clearing requires the explicit flag.
#[tokio::test]
async fn credential_is_preserved_and_cleared_explicitly() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    // Save a token first.
    let (status, _) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "onebot",
            "ws_url": "ws://127.0.0.1:6700",
            "access_token": "keep-me",
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(adapter.config().access_token.as_deref(), Some("keep-me"));

    // An edit that omits the token keeps it.
    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "onebot",
            "ws_url": "ws://127.0.0.1:6701",
        })),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(adapter.config().access_token.as_deref(), Some("keep-me"));

    // An empty token from an untouched form field also keeps it, because that is what browsers
    // submit for a password field the operator never typed into.
    let (status, _) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "onebot",
            "ws_url": "ws://127.0.0.1:6702",
            "access_token": "",
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(adapter.config().access_token.as_deref(), Some("keep-me"));

    // Clearing is explicit.
    let (status, _) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "onebot",
            "ws_url": "ws://127.0.0.1:6702",
            "clear_access_token": true,
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert!(adapter.config().access_token.is_none());
    assert_eq!(persisted(&dir)["onebot"]["access_token"], Value::Null);
}

/// Contradictory credential instructions are refused rather than resolved by guessing.
#[tokio::test]
async fn contradictory_credential_instructions_are_refused() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({
            "enabled": false,
            "platform": "onebot",
            "ws_url": "ws://127.0.0.1:6700",
            "access_token": "new",
            "clear_access_token": true,
        })),
    )
    .await;

    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("not both")
    );
}

/// A node without the adapter answers `404` instead of pretending the routes do not exist.
#[tokio::test]
async fn routes_report_a_missing_adapter() {
    let dir = temp_dir();
    let router: Router = app(common::empty_state(PathBuf::from(dir.path())).await);

    let (status, body) =
        send_json(&router, Method::GET, "/api/v1/adapters/onebot/config", None).await;

    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
}

/// The configured adapter appears in the catalog as a built-in adapter.
#[tokio::test]
async fn catalog_lists_the_onebot_adapter_as_builtin() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(&router, Method::GET, "/api/v1/adapters", None).await;

    assert_eq!(status, 200);
    let onebot = body["adapters"]
        .as_array()
        .expect("adapter array")
        .iter()
        .find(|adapter| adapter["platform"] == "onebot")
        .expect("the OneBot adapter should be listed");

    assert_eq!(onebot["kind"], "builtin");
    assert_eq!(onebot["display_name"], "onebot");
    assert_eq!(onebot["connected"], false);
    assert_eq!(onebot["circuit_state"], "closed");
}

/// The control plane can start and stop the reverse listener without a node restart.
#[tokio::test]
async fn reverse_listener_can_be_enabled_and_disabled() {
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let dir = temp_dir();
    let (state, adapter, _ingest) = onebot_state(dir.path(), OneBotConfig::default()).await;
    let router: Router = app(state);
    let config = json!({
        "enabled": true,
        "ws_url": format!("ws://{address}/onebot"),
        "transport": "reverse_websocket"
    });
    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(config.clone()),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["status"]["connection_state"], "listening");
    assert_eq!(body["status"]["connected"], false);
    assert!(tokio::net::TcpStream::connect(address).await.is_ok());
    // A failed durable save must leave the existing listener and its configuration intact.
    let previous = adapter.config();
    let path = dir.path().join("system.json");
    let document = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, 500, "body: {body}");
    assert_eq!(adapter.config(), previous);
    assert!(tokio::net::TcpStream::connect(address).await.is_ok());
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, document).unwrap();

    // A switch changes only enablement, even when the console holds stale endpoint fields.
    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/onebot/config",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["status"]["connection_state"], "disabled");
    assert_eq!(
        adapter.config(),
        OneBotConfig {
            enabled: false,
            ..previous
        }
    );
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}
