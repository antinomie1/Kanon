//! Model-facing MCP results preserve tool failures, structured data and resource links.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use kanon_core::mcp::{McpServer, McpServerConfig, McpTransport, qualified_tool_name};
use kanon_core::toggle::ToggleStore;
use kanon_llm::tool_router::ToolHost;
use kanon_proto::v1::{ToolCallRequest, ToolCallResponse, tool_call_response};
use serde_json::{Value, json};

/// A real HTTP MCP endpoint returning the selected result through the ToolHost contract.
struct Fixture {
    server: McpServer,
    task: tokio::task::JoinHandle<()>,
    directory: tempfile::TempDir,
}

impl Fixture {
    /// Creates an isolated endpoint and attachment directory for one result payload.
    async fn new(result: Value) -> Self {
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<Value>| {
                let result = result.clone();
                async move {
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "result-fixture", "version": "1"},
                        }),
                        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
                        "tools/list" => json!({"tools": [{"name": "result", "inputSchema": {"type": "object"}}]}),
                        "tools/call" => result,
                        method => panic!("unexpected method: {method}"),
                    };
                    Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result})).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let server = McpServer::new(
            McpServerConfig {
                id: "fixture".into(),
                name: "Fixture".into(),
                transport: McpTransport::Http {
                    url,
                    headers: HashMap::new(),
                },
            },
            Arc::new(ToggleStore::in_memory()),
        )
        .with_attachment_dir(directory.path().join("attachments"));
        Self {
            server,
            task,
            directory,
        }
    }

    /// Calls the advertised tool through the same boundary used by the agent loop.
    async fn call(&self) -> ToolCallResponse {
        self.server
            .call_tool(ToolCallRequest {
                call_id: "call-result".into(),
                tool_name: qualified_tool_name("fixture", "result"),
                ..Default::default()
            })
            .await
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads already-rendered model text without another JSON string wrapper.
fn text(response: ToolCallResponse) -> String {
    assert!(response.success, "{}", response.error_message);
    match response.payload {
        Some(tool_call_response::Payload::RawBytes(bytes)) => String::from_utf8(bytes).unwrap(),
        payload => panic!("unexpected tool text payload: {payload:?}"),
    }
}

#[tokio::test]
async fn failures_keep_diagnostics_without_delivering_or_storing_error_attachments() {
    let fixture = Fixture::new(json!({
        "isError": true,
        "content": [
            {"type": "text", "text": "Choose a range ending after its start."},
            {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="},
            {"type": "resource", "resource": {"uri": "file:///partial.txt", "blob": "aGVsbG8="}},
        ],
        "structuredContent": {"field": "end", "minimum": 12},
    }))
    .await;
    let response = fixture.call().await;
    assert!(!response.success);
    assert!(
        response
            .error_message
            .contains("Choose a range ending after its start.")
    );
    assert!(response.error_message.contains("\"minimum\":12"));
    assert!(response.payload.is_none());
    assert!(response.attachments.is_empty());
    assert!(!fixture.directory.path().join("attachments").exists());
    assert_eq!(
        fixture.server.health().await.failures,
        0,
        "a tool failure is not a transport failure"
    );

    let empty = Fixture::new(json!({"isError": true, "content": []})).await;
    let response = empty.call().await;
    assert!(!response.success);
    assert!(
        response
            .error_message
            .contains("failure without diagnostic content")
    );
}

#[tokio::test]
async fn structured_data_is_complete_and_appears_once_beside_text_and_resource_links() {
    let structured = json!({"body": "x".repeat(4000), "tail": "must remain visible"});
    let fixture = Fixture::new(json!({
        "content": [
            {"type": "text", "text": "Report ready; literal braces {stay unchanged}."},
            {"type": "text", "text": serde_json::to_string_pretty(&structured).unwrap()},
            {"type": "resource_link", "uri": "report://full", "name": "Full report", "description": "Complete source"},
        ],
        "structuredContent": structured,
        "_meta": {"privateEnvelopeField": "not model content"},
    })).await;
    let result = text(fixture.call().await);
    assert!(result.contains("Report ready; literal braces {stay unchanged}."));
    assert!(result.contains("report://full"));
    assert!(result.contains("Complete source"));
    assert!(result.contains(&structured.to_string()));
    assert_eq!(result.matches("must remain visible").count(), 1);
    assert!(!result.contains("privateEnvelopeField"));
    assert!(!result.contains("truncated"));
}

#[tokio::test]
async fn structured_only_and_empty_successes_do_not_expose_the_protocol_envelope() {
    let structured = json!({"value": "result"});
    let fixture = Fixture::new(json!({"content": [], "structuredContent": structured})).await;
    assert_eq!(text(fixture.call().await), structured.to_string());

    let empty = Fixture::new(json!({"content": [], "_meta": {"requestId": "hidden"}})).await;
    assert_eq!(text(empty.call().await), "[tool completed without content]");
}

#[tokio::test]
async fn malformed_tool_results_cannot_turn_into_successful_empty_output() {
    for result in [
        json!({"isError": "true", "content": []}),
        json!({"content": "wrong shape"}),
        json!({"content": [], "structuredContent": []}),
        json!({}),
    ] {
        let fixture = Fixture::new(result).await;
        let response = fixture.call().await;
        assert!(!response.success);
        assert!(response.error_message.contains("invalid tool result"));
    }
}
