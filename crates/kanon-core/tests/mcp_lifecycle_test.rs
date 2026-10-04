//! Connection initialization is atomic, cancellable, and cannot revive retired MCP definitions.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::routing::post;
use futures_util::StreamExt;
use kanon_core::mcp::{McpConfigStore, McpPool, McpServer, McpServerConfig, McpTransport};
use kanon_core::toggle::{MCP_SECTION, ToggleStore};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

/// Holds one selected RPC and records protocol order without subprocess timing assumptions.
struct Fixture {
    blocked_method: &'static str,
    block_once: AtomicBool,
    entered: Notify,
    release: Notify,
    methods: Mutex<Vec<String>>,
    fail_list_once: AtomicBool,
    fail_notice_once: AtomicBool,
}

impl Fixture {
    fn new(blocked_method: &'static str) -> Self {
        Self {
            blocked_method,
            block_once: AtomicBool::new(!blocked_method.is_empty()),
            entered: Notify::new(),
            release: Notify::new(),
            methods: Mutex::new(Vec::new()),
            fail_list_once: AtomicBool::new(false),
            fail_notice_once: AtomicBool::new(false),
        }
    }

    async fn respond(
        &self,
        headers: HeaderMap,
        request: Value,
    ) -> (StatusCode, HeaderMap, Json<Value>) {
        let method = request["method"].as_str().unwrap();
        self.methods.lock().await.push(method.to_string());
        let mut response_headers = HeaderMap::new();
        if method == "initialize" {
            response_headers.insert(
                "mcp-session-id",
                HeaderValue::from_static("fixture-session"),
            );
        } else if headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some("fixture-session")
        {
            return (
                StatusCode::BAD_REQUEST,
                response_headers,
                Json(json!({"error":"missing MCP session"})),
            );
        }
        if method == self.blocked_method && self.block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        if method == "notifications/initialized" {
            let status = if self.fail_notice_once.swap(false, Ordering::SeqCst) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::ACCEPTED
            };
            return (status, response_headers, Json(Value::Null));
        }
        let id = request["id"].clone();
        if method == "tools/list" && self.fail_list_once.swap(false, Ordering::SeqCst) {
            return (
                StatusCode::OK,
                response_headers,
                Json(
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":"list failed"}}),
                ),
            );
        }
        let result = match method {
            "initialize" => json!({"protocolVersion":"2024-11-05", "capabilities":{}}),
            "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
            _ => json!({"content":[{"type":"text","text":"ok"}]}),
        };
        (
            StatusCode::OK,
            response_headers,
            Json(json!({"jsonrpc":"2.0", "id":id, "result":result})),
        )
    }
}

async fn fixture(
    blocked_method: &'static str,
) -> (Arc<Fixture>, McpServerConfig, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(Fixture::new(blocked_method));
    let handler = fixture.clone();
    let app = axum::Router::new().route(
        "/",
        post(move |headers: HeaderMap, Json(request): Json<Value>| {
            let handler = handler.clone();
            async move { handler.respond(headers, request).await }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = McpServerConfig {
        id: "fixture".to_string(),
        name: "Fixture".to_string(),
        transport: McpTransport::Http {
            url,
            headers: HashMap::new(),
        },
    };
    (fixture, config, task)
}

#[tokio::test]
async fn concurrent_connect_waits_for_initialization_and_disconnect_clears_publication() {
    let (fixture, config, task) = fixture("notifications/initialized").await;
    let server = Arc::new(standalone(config));
    let connecting = server.clone();
    let first = tokio::spawn(async move { connecting.connect().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.entered.notified())
        .await
        .unwrap();
    assert!(server.plugin_meta().is_empty());

    let connecting = server.clone();
    let second = tokio::spawn(async move { connecting.connect().await });
    tokio::task::yield_now().await;
    assert!(
        !second.is_finished(),
        "an initialized notification is still in flight"
    );
    fixture.release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        *fixture.methods.lock().await,
        ["initialize", "notifications/initialized", "tools/list"]
    );
    assert_eq!(server.tool_count(), 1);
    assert_eq!(server.health().await.state, "connected");
    assert_eq!(server.call("echo", json!({})).await.unwrap().text, "ok");
    server.disconnect().await;
    assert!(!server.is_connected().await);
    assert!(server.plugin_meta().is_empty());
    assert_eq!(server.health().await.state, "disconnected");
    task.abort();
}

#[tokio::test]
async fn disconnect_waits_for_initialization_without_being_undone_by_late_publication() {
    let (fixture, config, task) = fixture("notifications/initialized").await;
    let server = Arc::new(standalone(config));
    let connecting = server.clone();
    let first = tokio::spawn(async move { connecting.connect().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.entered.notified())
        .await
        .unwrap();
    let disconnecting = server.clone();
    let disconnected = tokio::spawn(async move { disconnecting.disconnect().await });
    tokio::task::yield_now().await;
    assert!(!disconnected.is_finished());
    fixture.release.notify_one();
    first.await.unwrap().unwrap();
    disconnected.await.unwrap();
    assert!(!server.is_connected().await);
    assert!(server.plugin_meta().is_empty());
    assert_eq!(server.health().await.state, "disconnected");
    task.abort();
}

#[tokio::test]
async fn a_cancelled_probe_preserves_the_last_committed_catalog() {
    let (fixture, config, task) = fixture("tools/list").await;
    fixture.block_once.store(false, Ordering::SeqCst);
    let server = Arc::new(standalone(config));
    server.connect().await.unwrap();
    let committed = server.plugin_meta();
    fixture.block_once.store(true, Ordering::SeqCst);
    let probing = server.clone();
    let probe = tokio::spawn(async move { probing.probe().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.entered.notified())
        .await
        .unwrap();
    assert_eq!(server.plugin_meta(), committed);
    assert_eq!(server.health().await.state, "connected");
    probe.abort();
    assert!(probe.await.unwrap_err().is_cancelled());
    assert!(server.is_connected().await);
    assert_eq!(server.plugin_meta(), committed);
    assert_eq!(server.health().await.state, "connected");
    fixture.release.notify_one();
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn the_request_deadline_includes_an_http_body_that_never_finishes() {
    let body_started = Arc::new(Notify::new());
    let started = body_started.clone();
    let app = axum::Router::new().route(
        "/",
        post(move || {
            let started = started.clone();
            async move {
                let prefix = futures_util::stream::once(async move {
                    started.notify_one();
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"{"))
                });
                axum::body::Body::from_stream(prefix.chain(futures_util::stream::pending()))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = Arc::new(standalone(McpServerConfig {
        id: "slow".to_string(),
        name: "Slow body".to_string(),
        transport: McpTransport::Http {
            url,
            headers: HashMap::new(),
        },
    }));
    let connecting = server.clone();
    let connection = tokio::spawn(async move { connecting.connect().await });
    body_started.notified().await;
    // Let the client consume the delivered headers and partial body before advancing its clock.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(kanon_core::mcp::MCP_REQUEST_TIMEOUT).await;
    let error = tokio::time::timeout(Duration::from_secs(1), connection)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("in time"), "{error}");
    assert!(!server.is_connected().await);
    assert!(server.plugin_meta().is_empty());
    task.abort();
}

#[tokio::test]
async fn cancelled_handshake_leaves_no_transport_and_retry_initializes_again() {
    let (fixture, config, task) = fixture("initialize").await;
    let server = Arc::new(standalone(config));
    let connecting = server.clone();
    let first = tokio::spawn(async move { connecting.connect().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.entered.notified())
        .await
        .unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(!server.is_connected().await);
    assert!(server.plugin_meta().is_empty());
    assert_eq!(server.health().await.state, "disconnected");
    server.connect().await.unwrap();
    assert_eq!(
        fixture
            .methods
            .lock()
            .await
            .iter()
            .filter(|method| *method == "initialize")
            .count(),
        2
    );
    assert_eq!(server.tool_count(), 1);
    fixture.release.notify_one();
    task.abort();
}

#[tokio::test]
async fn failed_discovery_or_initialized_notification_requires_a_fresh_handshake() {
    let (fixture, config, task) = fixture("").await;
    fixture.fail_notice_once.store(true, Ordering::SeqCst);
    fixture.fail_list_once.store(true, Ordering::SeqCst);
    let server = standalone(config);
    for expected in ["notification", "list failed"] {
        let error = server.connect().await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!server.is_connected().await);
        assert!(server.plugin_meta().is_empty());
    }
    server.connect().await.unwrap();
    assert_eq!(
        fixture
            .methods
            .lock()
            .await
            .iter()
            .filter(|method| *method == "initialize")
            .count(),
        3
    );
    assert_eq!(server.tool_count(), 1);
    task.abort();
}

#[tokio::test]
async fn replacement_and_removal_retire_handles_still_owned_by_a_turn() {
    let (_, definition, task) = fixture("").await;
    let config = McpConfigStore::in_memory();
    config.upsert(definition.clone()).await.unwrap();
    let pool = McpPool::new(Arc::new(ToggleStore::in_memory()));
    pool.sync_from_config(&config).await;
    let previous = pool.get("fixture").await.unwrap();
    previous.connect().await.unwrap();

    let mut replacement = definition;
    replacement.name = "Replacement".to_string();
    config.upsert(replacement).await.unwrap();
    pool.sync_from_config(&config).await;
    let current = pool.get("fixture").await.unwrap();
    assert!(!Arc::ptr_eq(&previous, &current));
    assert!(!previous.is_connected().await);
    assert!(previous.plugin_meta().is_empty());
    assert!(
        previous
            .call("echo", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("removed or replaced")
    );
    current.connect().await.unwrap();

    config.remove("fixture").await.unwrap();
    pool.sync_from_config(&config).await;
    assert!(pool.get("fixture").await.is_none());
    assert!(!current.is_connected().await);
    assert!(current.connect().await.is_err());
    assert!(current.plugin_meta().is_empty());
    task.abort();
}

#[tokio::test]
async fn a_retained_tool_handle_obeys_the_shared_disable_and_enable_switch() {
    let (fixture, definition, task) = fixture("").await;
    let config = McpConfigStore::in_memory();
    config.upsert(definition).await.unwrap();
    let toggles = Arc::new(ToggleStore::in_memory());
    let pool = McpPool::new(toggles.clone());
    pool.sync_from_config(&config).await;
    let retained = pool.get("fixture").await.unwrap();
    retained.connect().await.unwrap();
    let calls_before_disable = fixture.methods.lock().await.len();

    toggles
        .set_enabled(MCP_SECTION, "fixture", false)
        .await
        .unwrap();
    retained.disconnect().await;
    let error = retained.call("echo", json!({})).await.unwrap_err();
    assert!(error.to_string().contains("disabled"), "{error}");
    assert!(!retained.is_connected().await);
    assert_eq!(fixture.methods.lock().await.len(), calls_before_disable);

    toggles
        .set_enabled(MCP_SECTION, "fixture", true)
        .await
        .unwrap();
    assert_eq!(retained.call("echo", json!({})).await.unwrap().text, "ok");
    assert_eq!(
        fixture
            .methods
            .lock()
            .await
            .iter()
            .filter(|method| *method == "initialize")
            .count(),
        2,
        "reenabling uses the same switch and establishes a fresh connection"
    );
    task.abort();
}

#[tokio::test]
async fn disabling_during_initialization_prevents_late_connection_publication() {
    let (fixture, definition, task) = fixture("initialize").await;
    let toggles = Arc::new(ToggleStore::in_memory());
    let server = Arc::new(McpServer::new(definition, toggles.clone()));
    let connecting = server.clone();
    let connection = tokio::spawn(async move { connecting.connect().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.entered.notified())
        .await
        .unwrap();
    toggles
        .set_enabled(MCP_SECTION, "fixture", false)
        .await
        .unwrap();
    fixture.release.notify_one();
    let error = connection.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("disabled"), "{error}");
    assert!(!server.is_connected().await);
    assert!(server.plugin_meta().is_empty());
    assert!(server.connect().await.is_err());
    task.abort();
}

/// Standalone fixture servers use one isolated, initially enabled toggle store.
fn standalone(config: McpServerConfig) -> McpServer {
    McpServer::new(config, Arc::new(ToggleStore::in_memory()))
}
