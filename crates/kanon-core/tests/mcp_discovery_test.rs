//! MCP discovery follows opaque cursors and publishes only complete, validated catalogs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use kanon_core::mcp::{
    MCP_MAX_MESSAGE_BYTES, MCP_REQUEST_TIMEOUT, McpServer, McpServerConfig, McpTransport,
};
use kanon_core::toggle::ToggleStore;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

struct Fixture {
    pages: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
    list_delay_ms: AtomicU64,
    list_entered: Notify,
}

impl Fixture {
    async fn respond(&self, request: Value) -> Response {
        let result = match request["method"].as_str().unwrap() {
            "initialize" => json!({
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"}
            }),
            "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
            "tools/list" => {
                self.requests.lock().await.push(request["params"].clone());
                let delay = self.list_delay_ms.load(Ordering::SeqCst);
                if delay > 0 {
                    self.list_entered.notify_one();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                self.pages
                    .lock()
                    .await
                    .pop_front()
                    .expect("unexpected tools/list request")
            }
            method => panic!("unexpected method: {method}"),
        };
        Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result})).into_response()
    }
}

async fn fixture(pages: Vec<Value>) -> (McpServer, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(Fixture {
        pages: Mutex::new(pages.into()),
        requests: Mutex::new(Vec::new()),
        list_delay_ms: AtomicU64::new(0),
        list_entered: Notify::new(),
    });
    let handler = fixture.clone();
    let app = axum::Router::new().route(
        "/",
        post(move |Json(request): Json<Value>| {
            let handler = handler.clone();
            async move { handler.respond(request).await }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = McpServer::new(
        McpServerConfig {
            id: "fixture".to_string(),
            name: "Fixture".to_string(),
            transport: McpTransport::Http {
                url,
                headers: HashMap::new(),
            },
        },
        Arc::new(ToggleStore::in_memory()),
    );
    (server, fixture, task)
}

fn tool(name: &str) -> Value {
    json!({"name": name, "inputSchema": {"type": "object"}})
}

#[tokio::test]
async fn pagination_preserves_opaque_cursors_and_publishes_every_page() {
    let opaque = "  +a/= 🧪 ";
    let (server, fixture, task) = fixture(vec![
        json!({"tools": [tool("first")], "nextCursor": opaque}),
        json!({"tools": [], "nextCursor": ""}),
        json!({"tools": [tool("last")]}),
    ])
    .await;
    server.connect().await.unwrap();
    assert_eq!(
        *fixture.requests.lock().await,
        [json!({}), json!({"cursor": opaque}), json!({"cursor": ""}),]
    );
    let catalog = server.plugin_meta();
    assert_eq!(
        catalog[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__fixture__first", "mcp__fixture__last",]
    );
    assert!(
        catalog[0]
            .tools
            .iter()
            .all(|tool| tool.parameters.is_some())
    );
    assert!(server.is_connected().await);
    server.disconnect().await;
    task.abort();
}

#[tokio::test]
async fn malformed_later_pages_never_publish_a_partial_catalog() {
    let invalid_pages = [
        json!({}),
        json!({"tools": null}),
        json!({"tools": {"name": "invalid"}}),
        json!({"tools": [null]}),
        json!({"tools": [{"name": " "}]}),
        json!({"tools": [{"name": "missing_schema"}]}),
        json!({"tools": [{"name": "array_schema", "inputSchema": []}]}),
        json!({"tools": [{"name": "wrong_type", "inputSchema": {"type": "string"}}]}),
        json!({"tools": [{"name": "bad_properties", "inputSchema": {"type": "object", "properties": []}}]}),
        json!({"tools": [{"name": "bad_required", "inputSchema": {"type": "object", "required": [3]}}]}),
        json!({"tools": [{"name": "bad_description", "description": false, "inputSchema": {"type": "object"}}]}),
        json!({"tools": [], "nextCursor": 2}),
        json!({"tools": [], "nextCursor": null}),
    ];
    for invalid in invalid_pages {
        let (server, _, task) = fixture(vec![
            json!({"tools": [tool("first")], "nextCursor": "next"}),
            invalid.clone(),
        ])
        .await;
        let error = server.connect().await.unwrap_err();
        assert!(
            error.to_string().contains("invalid tools/list"),
            "{invalid}: {error}"
        );
        assert!(!server.is_connected().await);
        assert!(server.plugin_meta().is_empty());
        task.abort();
    }
}

#[tokio::test]
async fn duplicate_tools_and_cursor_cycles_fail_without_publication() {
    for (pages, expected) in [
        (
            vec![
                json!({"tools": [tool("duplicate")], "nextCursor": "next"}),
                json!({"tools": [tool("duplicate")]}),
            ],
            "duplicate tool",
        ),
        (
            vec![
                json!({"tools": [], "nextCursor": "a"}),
                json!({"tools": [], "nextCursor": "b"}),
                json!({"tools": [], "nextCursor": "a"}),
            ],
            "cursor repeated",
        ),
    ] {
        let (server, _, task) = fixture(pages).await;
        let error = server.connect().await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!server.is_connected().await);
        assert!(server.plugin_meta().is_empty());
        task.abort();
    }
}

#[tokio::test]
async fn individually_valid_pages_share_one_aggregate_size_limit() {
    let mut first = tool("first");
    first["description"] = Value::String("a".repeat(MCP_MAX_MESSAGE_BYTES / 2));
    let mut second = tool("second");
    second["description"] = Value::String("b".repeat(MCP_MAX_MESSAGE_BYTES / 2));
    let (server, fixture, task) = fixture(vec![
        json!({"tools": [first], "nextCursor": "next"}),
        json!({"tools": [second]}),
    ])
    .await;

    let error = server.connect().await.unwrap_err();
    assert!(
        error.to_string().contains("aggregate message size limit"),
        "{error}"
    );
    assert_eq!(fixture.requests.lock().await.len(), 2);
    assert!(server.plugin_meta().is_empty());
    assert!(!server.is_connected().await);
    task.abort();
}

#[tokio::test]
async fn a_new_cursor_does_not_restart_the_discovery_deadline() {
    let (server, fixture, task) = fixture(vec![json!({"tools": [tool("original")]})]).await;
    let server = Arc::new(server);
    server.connect().await.unwrap();
    fixture.pages.lock().await.extend([
        json!({"tools": [tool("first")], "nextCursor": "next"}),
        json!({"tools": [tool("second")]}),
    ]);
    fixture.list_delay_ms.store(20_000, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    let refreshing = server.clone();
    let refresh = tokio::spawn(async move { refreshing.refresh_tools().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.list_entered.notified())
        .await
        .expect("the first discovery page must enter its delay");

    // Pause only while deliberately advancing a server delay. Real time between those steps lets
    // HTTP I/O progress without Tokio auto-advancing to a request timeout before the socket wakes.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(20)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(2), fixture.list_entered.notified())
        .await
        .expect("the second page must enter after the first page completes");
    assert_eq!(fixture.requests.lock().await.len(), 3);
    assert!(!refresh.is_finished());

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(refresh.await.unwrap().is_err());
    assert!(started.elapsed() >= MCP_REQUEST_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(40));
    assert!(server.plugin_meta().is_empty());
    assert!(!server.is_connected().await);
    task.abort();
}
