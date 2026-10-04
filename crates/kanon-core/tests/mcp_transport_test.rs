//! Real HTTP peers exercise framing, negotiated sessions and strict JSON-RPC responses.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::StreamExt;
use kanon_core::mcp::{McpServer, McpServerConfig, McpTransport};
use kanon_core::toggle::ToggleStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

async fn serve(app: axum::Router) -> (McpServer, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = McpServer::new(
        McpServerConfig {
            id: "wire".into(),
            name: "Wire fixture".into(),
            transport: McpTransport::Http {
                url,
                // Stale custom headers must not override or duplicate negotiated protocol state.
                headers: HashMap::from([
                    ("MCP-Session-Id".into(), "stale".into()),
                    ("mcp-protocol-version".into(), "stale".into()),
                    ("Accept".into(), "text/plain".into()),
                    ("Content-Type".into(), "text/plain".into()),
                ]),
            },
        },
        Arc::new(ToggleStore::in_memory()),
    );
    (server, task)
}

fn rpc(id: &Value, result: Value) -> Response {
    Json(json!({"jsonrpc":"2.0", "id":id, "result":result})).into_response()
}

fn initialize(id: &Value) -> Response {
    (
        [("mcp-session-id", "live-session")],
        Json(json!({"jsonrpc":"2.0", "id":id, "result":{
            "protocolVersion":"2025-03-26", "capabilities":{}
        }})),
    )
        .into_response()
}

#[tokio::test]
async fn sse_answers_server_ping_then_returns_matching_multiline_result_without_eof() {
    let answered = Arc::new(Notify::new());
    let app = axum::Router::new().route("/", post(move |headers: HeaderMap, Json(request): Json<Value>| {
        let answered = answered.clone();
        async move {
            assert_eq!(headers["accept"], "application/json, text/event-stream");
            assert_eq!(headers["content-type"], "application/json");
            let id = &request["id"];
            let method = request["method"].as_str();
            if method == Some("initialize") {
                assert!(headers.get("mcp-session-id").is_none());
                assert!(headers.get("mcp-protocol-version").is_none());
                assert_eq!(request["params"]["protocolVersion"], "2025-06-18");
                return initialize(id);
            }
            assert_eq!(headers.get_all("mcp-session-id").iter().count(), 1);
            assert_eq!(headers["mcp-session-id"], "live-session");
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
            match method {
                None => {
                    assert_eq!(request["result"], json!({}));
                    answered.notify_one();
                    StatusCode::ACCEPTED.into_response()
                }
                Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
                Some("tools/list") => {
                    // The peer waits for our response to its own same-ID request before replying.
                    let ping = format!("data: {}\r\r", json!({"jsonrpc":"2.0", "id":id, "method":"ping"}));
                    let result = format!(
                        ": heartbeat\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":{id},\r\ndata: \"result\":{{\"tools\":[{{\"name\":\"echo\",\"inputSchema\":{{\"type\":\"object\"}}}}]}}}}\r\n\r\n"
                    );
                    let stream = futures_util::stream::once(async move { Ok::<_, Infallible>(Bytes::from(ping)) })
                        .chain(futures_util::stream::once(async move {
                            answered.notified().await;
                            Ok::<_, Infallible>(Bytes::from(result))
                        }))
                        .chain(futures_util::stream::pending());
                    ([("content-type", "text/event-stream")], Body::from_stream(stream)).into_response()
                }
                _ => rpc(id, json!({"content":[{"type":"text","text":"ready"}]})),
            }
        }
    }));
    let (server, task) = serve(app).await;
    tokio::time::timeout(Duration::from_secs(2), server.connect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.tool_count(), 1);
    assert_eq!(server.call("echo", json!({})).await.unwrap().text, "ready");
    task.abort();
}

#[tokio::test]
async fn malformed_or_unmatched_responses_never_publish_a_connection() {
    for kind in [
        "version",
        "id",
        "both",
        "neither",
        "null",
        "error",
        "protocol",
        "content-type",
        "unfinished-sse",
    ] {
        let app = axum::Router::new().route("/", post(move |Json(request): Json<Value>| async move {
            let mut response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-06-18"}});
            match kind {
                "version" => response["jsonrpc"] = json!("1.0"),
                "id" => response["id"] = json!(999),
                "both" => response["error"] = json!({"code":-1,"message":"failed"}),
                "neither" => { response.as_object_mut().unwrap().remove("result"); }
                "null" => response["result"] = Value::Null,
                "error" => { response.as_object_mut().unwrap().remove("result"); response["error"] = json!({"message":"missing code"}); }
                "protocol" => response["result"]["protocolVersion"] = json!("2099-01-01"),
                "content-type" => return ([("content-type","text/plain")], response.to_string()).into_response(),
                "unfinished-sse" => return ([("content-type","text/event-stream")], format!("data: {response}\n")).into_response(),
                _ => unreachable!(),
            }
            Json(response).into_response()
        }));
        let (server, task) = serve(app).await;
        assert!(server.connect().await.is_err(), "accepted {kind}");
        assert!(!server.is_connected().await, "published {kind}");
        assert_eq!(server.tool_count(), 0);
        task.abort();
    }
}

#[tokio::test]
async fn expired_session_is_discarded_without_replaying_a_tool_call() {
    let initializations = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let inits = initializations.clone();
    let tool_calls = calls.clone();
    let app = axum::Router::new().route(
        "/",
        post(move |headers: HeaderMap, Json(request): Json<Value>| {
            let inits = inits.clone();
            let tool_calls = tool_calls.clone();
            async move {
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        assert!(headers.get("mcp-session-id").is_none());
                        inits.fetch_add(1, Ordering::SeqCst);
                        initialize(&request["id"])
                    }
                    "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
                    "tools/list" => rpc(&request["id"], json!({"tools":[]})),
                    "tools/call" if tool_calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                        StatusCode::NOT_FOUND.into_response()
                    }
                    _ => rpc(
                        &request["id"],
                        json!({"content":[{"type":"text","text":"done"}]}),
                    ),
                }
            }
        }),
    );
    let (server, task) = serve(app).await;
    let error = server.call("mutating", json!({})).await.unwrap_err();
    assert!(error.to_string().contains("session expired"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(initializations.load(Ordering::SeqCst), 1);
    assert!(!server.is_connected().await);
    assert_eq!(
        server.call("mutating", json!({})).await.unwrap().text,
        "done"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(initializations.load(Ordering::SeqCst), 2);
    task.abort();
}
