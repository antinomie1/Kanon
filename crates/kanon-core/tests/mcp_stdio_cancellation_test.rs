//! Cancelled stdio operations discard partial JSON frames before any later request.

#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use kanon_core::mcp::{McpServer, McpServerConfig, McpTransport};
use kanon_core::toggle::ToggleStore;
use kanon_llm::tool_router::ToolHost;
use serde_json::json;

#[tokio::test]
async fn cancelled_partial_stdio_responses_reconnect_before_calls_and_probes() {
    for blocked_method in ["tools/call", "tools/list"] {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("partial_mcp.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
state=$1
blocked_method=$2
printf 'started\n' >> "$state/starts"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      method=initialize
      result='{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"partial","version":"1"}}' ;;
    *'"tools/list"'*)
      method=tools/list
      result='{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}' ;;
    *'"tools/call"'*)
      method=tools/call
      result='{"content":[{"type":"text","text":"reconnected"}]}' ;;
    *) continue ;;
  esac
  if [ "$method" = "$blocked_method" ] && [ -f "$state/block" ]; then
    printf '{"jsonrpc":"2.0","id":%s,"result":' "$id"
    printf 'partial\n' > "$state/partial"
    while :; do sleep 1; done
  fi
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done
"#,
        )
        .unwrap();
        let server = Arc::new(McpServer::new(
            McpServerConfig {
                id: "partial".into(),
                name: "Partial frame fixture".into(),
                transport: McpTransport::Stdio {
                    command: "sh".into(),
                    args: vec![
                        script.to_string_lossy().into_owned(),
                        directory.path().to_string_lossy().into_owned(),
                        blocked_method.into(),
                    ],
                    env: HashMap::new(),
                },
            },
            Arc::new(ToggleStore::in_memory()),
        ));
        server.connect().await.unwrap();
        let catalog = server.plugin_metas();
        std::fs::write(directory.path().join("block"), b"").unwrap();
        let pending_server = server.clone();
        let pending = tokio::spawn(async move {
            if blocked_method == "tools/list" {
                pending_server.probe().await
            } else {
                pending_server.call("echo", json!({})).await.map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.path().join("partial").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the fixture must write a partial JSON frame");
        // Discovery remains stable while a probe owns the transport. Cancellation must then
        // discard the byte stream, rather than preserving a connection with incomplete framing.
        assert_eq!(server.plugin_metas(), catalog);
        assert_eq!(server.health().await.state, "connected");
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert!(!server.is_connected().await, "{blocked_method}");
        assert_eq!(server.health().await.state, "disconnected");

        std::fs::remove_file(directory.path().join("block")).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), server.call("echo", json!({})))
            .await
            .expect("a later call must initialize a fresh child")
            .unwrap();
        assert_eq!(result.text, "reconnected");
        assert_eq!(server.plugin_metas(), catalog);
        assert_eq!(server.health().await.state, "connected");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("starts"))
                .unwrap()
                .lines()
                .count(),
            2,
            "the cancelled {blocked_method} must not reuse its child",
        );
        server.disconnect().await;
    }
}
