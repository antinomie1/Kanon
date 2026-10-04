//! Tests for the built-in MCP client: handshake, tool discovery, invocation and policy.
//!
//! The fixture server is a POSIX shell script speaking JSON-RPC on stdio. A real MCP server would
//! pull a runtime into the test environment; this one exercises the exact contract the client
//! depends on (initialize, notifications/initialized, tools/list, tools/call) with no such cost.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanon_core::instance::{InstanceDraft, InstanceRegistry, ItemPolicy};
use kanon_core::mcp::{
    McpConfigStore, McpPool, McpServer, McpServerConfig, McpTransport, host_id, prune_attachments,
    qualified_tool_name, unqualified_tool_name,
};
use kanon_core::toggle::{MCP_SECTION, ToggleStore};
use kanon_llm::tool_router::ToolHost;
use kanon_proto::v1::ToolCallRequest;

/// Writes the fixture MCP server and returns its path.
///
/// The script extracts the request id from every line so the client's id matching is exercised for
/// real rather than being bypassed by a fixed id.
fn write_fixture_server(dir: &Path) -> PathBuf {
    let path = dir.join("fake_mcp_server.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo a string","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}},{"name":"chart","description":"Draws a chart","inputSchema":{"type":"object"}},{"name":"broken_image","description":"Returns corrupt image data","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      case "$line" in
        *'"name":"chart"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"chart text"},{"type":"image","mimeType":"image/png","data":"%s"}]}}\n' "$id" "$FAKE_IMAGE_BASE64" ;;
        *'"name":"broken_image"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"image","mimeType":"image/png","data":"not-base64!!"}]}}\n' "$id" ;;
        *'"name":"files"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"audio","mimeType":"audio/mpeg","data":"aGVsbG8="},{"type":"resource","resource":{"uri":"file:///reports/q3.pdf?v=2","mimeType":"application/pdf","blob":"JVBERi0="}},{"type":"resource","resource":{"uri":"file:///reports/..","mimeType":"application/pdf","blob":"JVBERi0="}},{"type":"resource","resource":{"uri":"memo://notes","mimeType":"text/plain","text":"note text"}}]}}\n' "$id" ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
      esac ;;
  esac
done
"#,
    )
    .expect("fixture server");
    path
}

/// Server definition pointing at the fixture script.
///
/// The script runs as `sh <script>` rather than being executed itself: tests run in parallel, and a
/// child forked by another test while this one still had the script open for writing would make
/// executing it fail with `ETXTBSY` ("Text file busy"). `sh` only reads the file.
fn fixture_config(script: &Path, id: &str) -> McpServerConfig {
    McpServerConfig {
        id: id.to_string(),
        name: "Fake".to_string(),
        transport: McpTransport::Stdio {
            command: "sh".to_string(),
            args: vec![script.to_string_lossy().to_string()],
            // A 1x1 PNG, base64 encoded: the smallest real image the attachment path can carry.
            env: HashMap::from([(
                "FAKE_IMAGE_BASE64".to_string(),
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
                    .to_string(),
            )]),
        },
    }
}

#[tokio::test]
async fn connect_handshakes_and_exposes_qualified_tools() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let server = McpServer::new(fixture_config(&script, "fake"));

    server.connect().await.expect("handshake");

    let health = server.health().await;
    assert_eq!(health.state, "connected");
    assert_eq!(health.tools, 3);
    assert_eq!(health.failures, 0);
    assert_eq!(health.last_error, None);

    let metas = server.plugin_metas();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].id, host_id("fake"));
    assert_eq!(metas[0].tools.len(), 3);
    // The model-facing name is namespaced by server so two servers may expose the same tool.
    assert_eq!(metas[0].tools[0].name, qualified_tool_name("fake", "echo"));
    assert_eq!(
        unqualified_tool_name("fake", &metas[0].tools[0].name).as_deref(),
        Some("echo")
    );

    // The full ToolHost entry point resolves the namespaced name back to the server's own tool.
    let response = server
        .call_tool(ToolCallRequest {
            call_id: "call-1".to_string(),
            tool_name: qualified_tool_name("fake", "echo"),
            session_id: String::new(),
            payload: None,
            ..Default::default()
        })
        .await
        .expect("tool call");
    assert!(response.success, "{}", response.error_message);

    // A tool name belonging to another server is refused instead of being sent upstream.
    let foreign = server
        .call_tool(ToolCallRequest {
            call_id: "call-2".to_string(),
            tool_name: qualified_tool_name("other", "echo"),
            session_id: String::new(),
            payload: None,
            ..Default::default()
        })
        .await
        .expect("tool call");
    assert!(!foreign.success);
    assert!(foreign.error_message.contains("does not belong"));
}

#[tokio::test]
async fn a_missing_server_records_the_concrete_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = McpServer::new(McpServerConfig {
        id: "absent".to_string(),
        name: "Absent".to_string(),
        transport: McpTransport::Stdio {
            command: dir
                .path()
                .join("does-not-exist")
                .to_string_lossy()
                .to_string(),
            args: Vec::new(),
            env: HashMap::new(),
        },
    });

    let err = server.connect().await.err().expect("connect must fail");
    assert!(err.to_string().contains("does-not-exist"));

    // The console reads the health snapshot, so the cause must survive there rather than being
    // reported as a permanent "connecting".
    let health = server.health().await;
    assert_eq!(health.state, "reconnecting");
    assert_eq!(health.failures, 1);
    assert!(
        health
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("does-not-exist")),
        "expected the transport error, got {:?}",
        health.last_error
    );

    server.disconnect().await;
    assert_eq!(server.health().await.state, "disconnected");
}

/// Creates an instance with a policy for one MCP server.
async fn instance_with_policy(
    registry: &InstanceRegistry,
    server_id: &str,
    policy: ItemPolicy,
    adapter: &str,
) -> String {
    registry
        .create(
            InstanceDraft {
                name: format!("MCP Bot {adapter}"),
                enabled: true,
                // One enabled instance per adapter: the catalog refuses an adapter claimed twice.
                adapters: vec![adapter.to_string()],
                persona_id: None,
                system_prompt: None,
                model: None,
                reply_policy: None,
                context_policy: None,
                plugins: Default::default(),
                skills: Default::default(),
                mcp: HashMap::from([(server_id.to_string(), policy)]),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance")
        .id
}

#[tokio::test]
async fn stale_attachments_are_swept_but_fresh_ones_survive() {
    let dir = tempfile::tempdir().expect("temp dir");
    let attachments = dir.path().join("attachments");
    std::fs::create_dir_all(&attachments).expect("attachments dir");

    let stale = attachments.join("stale.png");
    let fresh = attachments.join("fresh.png");
    std::fs::write(&stale, b"old").expect("stale file");
    std::fs::write(&fresh, b"new").expect("fresh file");
    // A named attachment lives in a directory of its own.
    let stale_named = attachments.join("1-1");
    std::fs::create_dir_all(&stale_named).expect("named dir");
    std::fs::write(stale_named.join("report.pdf"), b"old").expect("named file");

    // Backdate entries beyond the retention window without pulling in a dependency.
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 24 * 60 * 60);
    for entry in [&stale, &stale_named] {
        std::fs::File::open(entry)
            .expect("open stale entry")
            .set_times(std::fs::FileTimes::new().set_modified(long_ago))
            .expect("backdate");
    }

    let removed = prune_attachments(
        &attachments,
        std::time::Duration::from_secs(3 * 24 * 60 * 60),
    )
    .expect("prune");

    assert_eq!(removed, 2);
    assert!(!stale.exists(), "the stale attachment must be swept");
    assert!(
        !stale_named.exists(),
        "a stale named attachment goes with its directory"
    );
    assert!(fresh.exists(), "a fresh attachment must survive");
}

#[tokio::test]
async fn the_watchdog_connects_an_unused_server_instead_of_failing_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let server = McpServer::new(fixture_config(&script, "fake"))
        .with_attachment_dir(dir.path().join("attachments"));

    // A node that just started has not needed any server yet: the first watchdog pass must
    // establish the connection, not record "not connected" as a failure.
    server.probe().await.expect("first probe connects");
    assert!(server.is_connected().await);
    let health = server.health().await;
    assert_eq!(health.state, "connected");
    assert_eq!(health.failures, 0);

    // The next pass probes the live connection.
    server.probe().await.expect("second probe refreshes");
    assert_eq!(server.health().await.state, "connected");
}

#[tokio::test]
async fn image_content_becomes_a_file_the_platform_can_send() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let attachments = dir.path().join("attachments");
    let server =
        McpServer::new(fixture_config(&script, "fake")).with_attachment_dir(attachments.clone());

    let response = server
        .call_tool(ToolCallRequest {
            call_id: "call-chart".to_string(),
            tool_name: qualified_tool_name("fake", "chart"),
            session_id: String::new(),
            payload: None,
            ..Default::default()
        })
        .await
        .expect("tool call");
    assert!(response.success, "{}", response.error_message);

    // The text still reaches the model ...
    let payload = match response.payload {
        Some(kanon_proto::v1::tool_call_response::Payload::StructuredResult(result)) => {
            kanon_llm::tool_router::prost_struct_to_json(result).unwrap()
        }
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(payload["content"], serde_json::json!("chart text"));

    // ... and the picture is materialized where the outbound adapter can pick it up.
    assert_eq!(response.attachments.len(), 1);
    let attachment = &response.attachments[0];
    assert_eq!(attachment.mime_type, "image/png");
    let path = std::path::PathBuf::from(attachment.file_path.as_deref().expect("file path"));
    assert!(path.is_absolute(), "{}", path.display());
    assert!(path.starts_with(&attachments), "{}", path.display());
    let bytes = std::fs::read(&path).expect("attachment bytes");
    // PNG magic: proves the base64 payload survived decoding intact.
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );
}

#[tokio::test]
async fn an_undecodable_image_is_reported_instead_of_vanishing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let server = McpServer::new(fixture_config(&script, "fake"))
        .with_attachment_dir(dir.path().join("attachments"));

    let outcome = server
        .call("broken_image", serde_json::json!({}))
        .await
        .expect("tool call");

    assert!(outcome.attachments.is_empty());
    assert!(
        outcome.text.contains("not valid base64"),
        "the reason must travel with the result: {}",
        outcome.text
    );
}

#[tokio::test]
async fn audio_and_file_resources_become_attachments_under_their_own_names() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let attachments = dir.path().join("attachments");
    let server =
        McpServer::new(fixture_config(&script, "fake")).with_attachment_dir(attachments.clone());

    let outcome = server
        .call("files", serde_json::json!({}))
        .await
        .expect("tool call");

    // A text resource is for the model, not a file for the user.
    assert_eq!(outcome.text, "note text");
    let kinds: Vec<&str> = outcome
        .attachments
        .iter()
        .map(|attachment| attachment.mime_type.as_str())
        .collect();
    assert_eq!(kinds, ["audio/mpeg", "application/pdf", "application/pdf"]);

    let path = |index: usize| {
        let path = std::path::PathBuf::from(
            outcome.attachments[index]
                .file_path
                .as_deref()
                .expect("file path"),
        );
        assert!(path.starts_with(&attachments), "{}", path.display());
        path
    };
    assert_eq!(std::fs::read(path(0)).expect("audio bytes"), b"hello");
    // The URI names the file the recipient sees.
    assert_eq!(path(1).file_name().unwrap(), "q3.pdf");
    assert_eq!(std::fs::read(path(1)).expect("pdf bytes"), b"%PDF-");
    // A name that would leave the attachment directory falls back to a generated one.
    assert_eq!(
        path(2).parent().unwrap(),
        attachments.canonicalize().unwrap()
    );
    assert_eq!(path(2).extension().unwrap(), "pdf");
}

#[tokio::test]
async fn pool_honours_the_global_switch_and_the_instance_policy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());

    let config = Arc::new(
        McpConfigStore::open(dir.path().join("mcp.json"))
            .await
            .expect("config store"),
    );
    config
        .upsert(fixture_config(&script, "fake"))
        .await
        .expect("upsert");

    let pool = Arc::new(McpPool::new());
    pool.sync_from_config(&config).await;
    let toggles = Arc::new(ToggleStore::in_memory());
    let registry = Arc::new(InstanceRegistry::default());

    // Node-wide switch off: no host is offered, even though the server would connect fine.
    toggles
        .set_enabled(MCP_SECTION, "fake", false)
        .await
        .expect("disable");
    assert!(
        pool.hosts_for_instance(&toggles, None).await.is_empty(),
        "a globally disabled server must not be offered"
    );

    toggles
        .set_enabled(MCP_SECTION, "fake", true)
        .await
        .expect("enable");
    let instance_id =
        instance_with_policy(&registry, "fake", ItemPolicy::Disable, "qqofficial").await;
    let instance = registry.get(&instance_id).await.expect("instance");
    assert!(
        pool.hosts_for_instance(&toggles, Some(&instance))
            .await
            .is_empty(),
        "an instance that disabled the server must not receive it"
    );

    // An explicit opt-in behaves like inherit: the global switch still decides.
    let enabled_id = instance_with_policy(&registry, "fake", ItemPolicy::Enable, "telegram").await;
    let enabled_instance = registry.get(&enabled_id).await.expect("instance");
    assert_eq!(
        pool.hosts_for_instance(&toggles, Some(&enabled_instance))
            .await
            .len(),
        1
    );

    // With no instance (console sandbox) the node-wide switch alone decides.
    assert_eq!(
        pool.hosts_for_instance(&toggles, None).await.len(),
        1,
        "with no instance the node-wide switch alone decides"
    );
}

/// The pool's connection is reused across calls rather than re-established per request.
#[tokio::test]
async fn a_connected_server_reuses_its_transport() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_fixture_server(dir.path());
    let server = McpServer::new(fixture_config(&script, "fake"));

    server.connect().await.expect("first connect");
    // A second connect must be a no-op: spawning a new child process per call would leak one
    // process per tool call.
    server.connect().await.expect("second connect");
    assert_eq!(server.health().await.state, "connected");
    assert_eq!(server.health().await.tools, 3);
}
