//! Milky adapter management routes: persistence, hot application and connectivity probing.
//!
//! These tests exercise the control plane the way a console does — over HTTP, against a real
//! `data/system.json` on disk — because the two failure modes that matter here are invisible to a
//! unit test: a configuration that is applied but not persisted (silently lost on restart), and a
//! credential that is dropped by an unrelated edit.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::routing::{get, post};
use axum::{Json, extract::Path as AxumPath};
use kanon_adapter_milky::{MilkyAdapter, MilkyConfig};
use kanon_api::{ApiState, SystemConfigStore, app};
use kanon_core::supervisor::Supervisor;
use kanon_core::{EventIngress, PlatformAdapter};
use kanon_proto::v1::IngestEventRequest;
use tokio::sync::mpsc;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::send_json;

mod common;

/// A minimal Milky endpoint serving the two probe endpoints and a never-ending event stream.
struct FakeEndpoint {
    /// Base URL the adapter should be pointed at.
    base_url: String,
    /// Server task, aborted when the handle drops.
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for FakeEndpoint {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl FakeEndpoint {
    /// Starts the fake endpoint on an ephemeral port.
    async fn start() -> Self {
        let router = Router::new()
            .route("/event", get(never_ending_stream))
            .route("/api/:endpoint", post(api_handler));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake endpoint should bind");
        let address: SocketAddr = listener.local_addr().expect("address");

        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self {
            base_url: format!("http://{address}"),
            handle,
        }
    }
}

/// Serves an SSE stream that never produces an event, so the connection stays established.
async fn never_ending_stream() -> Response {
    let stream = futures_util::stream::pending::<Result<SseEvent, Infallible>>();
    Sse::new(stream).into_response()
}

/// Answers every probe endpoint with a well-formed success envelope.
async fn api_handler(AxumPath(endpoint): AxumPath<String>) -> Json<Value> {
    let data = match endpoint.as_str() {
        "get_login_info" => json!({ "uin": 10001, "nickname": "Kanon Test" }),
        "get_impl_info" => json!({
            "impl_name": "FakeMilky",
            "impl_version": "1.0.0",
            "qq_protocol_version": "9.0.0",
            "qq_protocol_type": "linux",
            "milky_version": "1.3",
        }),
        _ => json!({}),
    };

    Json(json!({ "status": "ok", "retcode": 0, "data": data }))
}

/// Builds gateway state hosting a started Milky adapter whose configuration persists into `dir`.
///
/// The returned receiver keeps the ingest channel open for the duration of the test; a closed
/// channel would make the adapter's connection pointless and is not what the node does.
async fn milky_state(
    dir: &Path,
    config: MilkyConfig,
) -> (
    ApiState,
    Arc<MilkyAdapter>,
    mpsc::Receiver<IngestEventRequest>,
) {
    // The supervisor owns its run directory; only the configuration directory must outlive the
    // returned state, so the run directory guard is intentionally detached.
    let run = tempfile::tempdir().expect("run dir");
    let supervisor = Arc::new(Supervisor::new(Some(run.path().to_path_buf()), None));
    std::mem::forget(run);

    let store = Arc::new(SystemConfigStore::new(dir.join("system.json")));
    let adapter = Arc::new(MilkyAdapter::new(config).expect("adapter should build"));
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .expect("adapter should register");

    let state = ApiState::builder(supervisor.clone())
        .with_config_dir(dir.to_path_buf())
        .with_system_config(store)
        .with_milky_adapter(adapter.clone())
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
    let (state, _adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::GET,
        "/api/v1/adapters/milky/config",
        None,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["config"]["enabled"], false);
    assert_eq!(body["config"]["platform"], "milky");
    assert_eq!(body["config"]["base_url"], "http://127.0.0.1:3010");
    assert_eq!(body["config"]["transport"], "sse");
    assert_eq!(body["status"]["state"], "disabled");
    assert_eq!(body["status"]["connected"], false);
    assert!(
        body["config"].get("access_token").is_none(),
        "the credential must never be reported"
    );
}

/// A saved configuration is applied to the running adapter and written to disk.
#[tokio::test]
async fn update_applies_and_persists_the_configuration() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);
    let fake = FakeEndpoint::start().await;

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": true,
            "platform": "milky",
            "base_url": fake.base_url,
            "transport": "sse",
            "access_token": "s3cret",
        })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["status"]["enabled"], true);
    assert_eq!(body["config"]["base_url"], fake.base_url);
    assert_eq!(body["status"]["token_configured"], true);
    assert!(body["config"].get("access_token").is_none());

    // The live adapter is reconfigured, not merely the file.
    let live = adapter.config();
    assert!(live.enabled);
    assert_eq!(live.base_url, fake.base_url);
    assert_eq!(live.access_token.as_deref(), Some("s3cret"));

    // …and the document on disk describes exactly that, credential included.
    let document = persisted(&dir);
    assert_eq!(document["milky"]["enabled"], true);
    assert_eq!(document["milky"]["base_url"], fake.base_url);
    assert_eq!(document["milky"]["access_token"], "s3cret");

    // The connection comes up without a restart.
    for _ in 0..500 {
        if adapter.is_connected() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        adapter.is_connected(),
        "the adapter should connect after being configured"
    );
}

/// A rejected configuration leaves both the running adapter and the file untouched.
#[tokio::test]
async fn invalid_update_changes_nothing() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": true,
            "platform": "milky",
            "base_url": "not-a-url",
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
    let (state, _adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "qq",
            "base_url": "http://127.0.0.1:3010",
        })),
    )
    .await;

    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("restart the node"),
        "body: {body}"
    );
}

/// Editing an unrelated field preserves the stored credential; clearing requires the explicit flag.
#[tokio::test]
async fn credential_is_preserved_and_cleared_explicitly() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    // Save a token first.
    let (status, _) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "milky",
            "base_url": "http://127.0.0.1:3010",
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
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "milky",
            "base_url": "http://127.0.0.1:3011",
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
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "milky",
            "base_url": "http://127.0.0.1:3012",
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
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "milky",
            "base_url": "http://127.0.0.1:3012",
            "clear_access_token": true,
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert!(adapter.config().access_token.is_none());
    assert_eq!(persisted(&dir)["milky"]["access_token"], Value::Null);
}

/// Contradictory credential instructions are refused rather than resolved by guessing.
#[tokio::test]
async fn contradictory_credential_instructions_are_refused() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::PUT,
        "/api/v1/adapters/milky/config",
        Some(json!({
            "enabled": false,
            "platform": "milky",
            "base_url": "http://127.0.0.1:3010",
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

/// The probe reports the account identity without saving or enabling anything.
#[tokio::test]
async fn probe_reports_identity_without_saving() {
    let dir = temp_dir();
    let (state, adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);
    let fake = FakeEndpoint::start().await;

    let (status, body) = send_json(
        &router,
        Method::POST,
        "/api/v1/adapters/milky/config/test",
        Some(json!({ "base_url": fake.base_url })),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["login"]["uin"], 10001);
    assert_eq!(body["login"]["nickname"], "Kanon Test");
    assert_eq!(body["implementation"]["impl_name"], "FakeMilky");
    assert!(body["latency_ms"].is_u64());

    assert!(!adapter.config().enabled);
    assert!(
        !dir.path().join("system.json").exists(),
        "a probe must not persist anything"
    );
}

/// An unreachable endpoint is an upstream failure, clearly distinguished from a bad request.
#[tokio::test]
async fn probe_reports_an_unreachable_endpoint() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(
        &router,
        Method::POST,
        "/api/v1/adapters/milky/config/test",
        Some(json!({ "base_url": "http://127.0.0.1:1" })),
    )
    .await;

    assert_eq!(status, 502);
    assert_eq!(body["error"]["code"], "upstream_error");
}

/// A node without the adapter answers `404` instead of pretending the routes do not exist.
#[tokio::test]
async fn routes_report_a_missing_adapter() {
    let dir = temp_dir();
    let router: Router = app(common::empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(
        &router,
        Method::GET,
        "/api/v1/adapters/milky/config",
        None,
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
}

/// The configured adapter appears in the catalog as a built-in adapter.
#[tokio::test]
async fn catalog_lists_the_milky_adapter_as_builtin() {
    let dir = temp_dir();
    let (state, _adapter, _ingest) = milky_state(dir.path(), MilkyConfig::default()).await;
    let router: Router = app(state);

    let (status, body) = send_json(&router, Method::GET, "/api/v1/adapters", None).await;

    assert_eq!(status, 200);
    let milky = body["adapters"]
        .as_array()
        .expect("adapter array")
        .iter()
        .find(|adapter| adapter["platform"] == "milky")
        .expect("the Milky adapter should be listed");

    assert_eq!(milky["kind"], "builtin");
    assert_eq!(milky["display_name"], "milky");
    assert_eq!(milky["connected"], false);
    assert_eq!(milky["circuit_state"], "closed");
}
