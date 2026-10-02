//! Integration tests for the tool catalog endpoint.
//!
//! The catalog is the operator's answer to "what can the model call, and who provides it?", so
//! these tests assert all three sources appear with correct attribution and that switching an MCP
//! server off removes its tools from the list, not just from the model's view.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use kanon_api::ApiState;
use kanon_core::{McpConfigStore, McpPool, ReadSkillTool, SkillStore, Supervisor, ToggleStore};
use serde_json::{Value, json};

/// Builds state with native tools, the fixture plugin host and an empty MCP pool.
async fn tools_state(root: &Path) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    // The supervisor owns socket cleanup for its run directory; only `root` must outlive the call.
    std::mem::forget(temp);
    common::register_fixture_host(&supervisor).await;

    let skills = Arc::new(SkillStore::new(root.join("skills")));
    let toggles = Arc::new(
        ToggleStore::open(root.join("toggles.json"))
            .await
            .expect("toggle store"),
    );
    let instances = Arc::new(kanon_core::InstanceRegistry::default());

    ApiState::builder(supervisor)
        .with_config_dir(root.join("config"))
        .with_plugin_state(toggles.clone())
        .with_skill_store(skills.clone())
        .with_native_tools(vec![Arc::new(ReadSkillTool::new(
            skills, toggles, instances,
        ))])
        .with_mcp_config(Arc::new(
            McpConfigStore::open(root.join("mcp.json"))
                .await
                .expect("mcp config"),
        ))
        .with_mcp_pool(Arc::new(McpPool::new()))
        .build()
}

/// Writes a stdio MCP server speaking just enough JSON-RPC for the client handshake.
///
/// It is started as `sh <script>`, never executed itself: a child forked by a parallel test while
/// the script was still open for writing would make executing it fail with `ETXTBSY`.
fn write_fixture_mcp_server(dir: &Path) -> PathBuf {
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
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo a string","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
  esac
done
"#,
    )
    .expect("fixture server");
    path
}

/// Finds one tool by name.
fn find<'a>(catalog: &'a Value, name: &str) -> &'a Value {
    catalog["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == json!(name))
        .unwrap_or_else(|| panic!("tool '{name}' missing from {catalog}"))
}

#[tokio::test]
async fn builtin_and_plugin_tools_are_listed_with_their_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app = kanon_api::app(tools_state(dir.path()).await);

    let (status, catalog) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["builtin"], json!(1));
    assert_eq!(catalog["plugin"], json!(1));
    assert_eq!(catalog["mcp"], json!(0));
    assert_eq!(catalog["total"], json!(2));

    let builtin = find(&catalog, "read_skill");
    assert_eq!(builtin["source"], json!("builtin"));
    assert_eq!(builtin["provider_id"], json!("kanon-core"));
    assert!(builtin.get("host_id").is_none(), "builtins have no host");

    // The fixture host advertises `fixture_tool` without a schema; the catalog must still hand the
    // console a usable object schema rather than `null`.
    let plugin = find(&catalog, "fixture_tool");
    assert_eq!(plugin["source"], json!("plugin"));
    assert_eq!(plugin["provider_id"], json!(common::FIXTURE_PLUGIN_ID));
    assert_eq!(plugin["host_id"], json!(common::FIXTURE_HOST_ID));
    assert_eq!(plugin["parameters"]["type"], json!("object"));
    assert_eq!(plugin["parameters"]["properties"], json!({}));
}

#[tokio::test]
async fn mcp_tools_are_namespaced_and_follow_the_node_switch() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app = kanon_api::app(tools_state(dir.path()).await);
    let script = write_fixture_mcp_server(dir.path());

    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx",
        Some(json!({
            "name": "Fixture",
            "transport": { "type": "stdio", "command": "sh", "args": [script.to_string_lossy()] },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, catalog) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["mcp"], json!(1));
    assert_eq!(catalog["total"], json!(3));

    let mcp = find(&catalog, "mcp__fx__echo");
    assert_eq!(mcp["source"], json!("mcp"));
    // The operator typed `fx`; the console must show that, not the derived host name.
    assert_eq!(mcp["provider_id"], json!("fx"));
    assert_eq!(mcp["host_id"], json!("mcp_fx"));
    assert_eq!(
        mcp["parameters"]["properties"]["text"]["type"],
        json!("string")
    );

    // Disabling the server removes its tools: the catalog lists callable tools, not definitions.
    let (status, _) = common::send_json(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, catalog) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
    assert_eq!(catalog["mcp"], json!(0));
    assert_eq!(catalog["total"], json!(2));
    assert!(
        catalog["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .all(|tool| tool["name"] != json!("mcp__fx__echo")),
        "a disabled server must not advertise tools"
    );
}
