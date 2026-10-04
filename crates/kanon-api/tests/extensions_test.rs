//! Integration tests for the skill and MCP control endpoints.
//!
//! Both resources live outside the plugin tree, so these tests build their own state over temporary
//! directories: the skill store must never read the repository's `data/skills`, and the MCP
//! configuration must never write the node's `data/mcp.json`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use kanon_api::ApiState;
use kanon_core::{
    McpConfigStore, McpPool, McpServerConfig, McpTransport, SkillStore, Supervisor, ToggleStore,
};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::{GatewayError, LlmProvider};
use serde_json::{Value, json};

/// Provider that records the tool names offered with each request and answers with fixed text.
///
/// Tool wiring is what these tests assert, so the provider must not swallow the definitions it
/// receives — which is exactly what a plain mock returning content would do.
#[derive(Default)]
struct ToolRecorder {
    offered: std::sync::Mutex<Vec<String>>,
    messages: std::sync::Mutex<Vec<kanon_llm::gateway::types::ChatMessage>>,
}

impl ToolRecorder {
    /// Tool names offered by the most recent request.
    fn offered(&self) -> Vec<String> {
        self.offered.lock().expect("recorder lock").clone()
    }

    /// Messages of the most recent request.
    fn messages(&self) -> Vec<kanon_llm::gateway::types::ChatMessage> {
        self.messages.lock().expect("recorder lock").clone()
    }
}

#[async_trait]
impl LlmProvider for ToolRecorder {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        *self.offered.lock().expect("recorder lock") =
            request.tools.iter().map(|tool| tool.name.clone()).collect();
        *self.messages.lock().expect("recorder lock") = request.messages.clone();
        Ok(ChatResponse {
            content: Some("recorded".to_string()),
            ..Default::default()
        })
    }
}

/// Builds gateway state whose skills, toggles and MCP configuration all live under `root`.
async fn extension_state(root: &Path) -> ApiState {
    extension_state_recording(root).await.0
}

/// Same as [`extension_state`], additionally exposing the provider's tool recorder.
async fn extension_state_recording(root: &Path) -> (ApiState, Arc<ToolRecorder>) {
    let recorder = Arc::new(ToolRecorder::default());
    let supervisor = Arc::new(Supervisor::new(None, None));
    let skills = Arc::new(SkillStore::new(root.join("skills")));
    let toggles = Arc::new(
        ToggleStore::open(root.join("toggles.json"))
            .await
            .expect("toggle store"),
    );
    let mcp_config = Arc::new(
        McpConfigStore::open(root.join("mcp.json"))
            .await
            .expect("mcp config"),
    );

    // Mirrors the node's composition root: the same store backing the `read_skill` tool and the
    // catalog hook, so a test exercises the wiring the running node actually uses.
    let instances = Arc::new(kanon_core::InstanceRegistry::default());
    let state = ApiState::builder(supervisor)
        .with_config_dir(root.join("config"))
        .with_skill_store(skills.clone())
        .with_plugin_state(toggles.clone())
        .with_instances(instances.clone())
        .with_native_tools(vec![Arc::new(kanon_core::ReadSkillTool::new(
            skills.clone(),
            toggles.clone(),
            instances.clone(),
        ))])
        .with_hooks(vec![Arc::new(kanon_core::SkillCatalogHook::new(
            skills.clone(),
            toggles.clone(),
            instances.clone(),
        ))])
        .with_mcp_config(mcp_config)
        .with_mcp_pool(Arc::new(McpPool::new(toggles.clone())))
        .with_llm_provider(
            "recorder",
            recorder.clone(),
            kanon_api::default_agent_config("mock-model"),
        )
        .build();

    (state, recorder)
}

/// Writes a minimal installable skill directory and returns its path.
fn write_skill_source(root: &Path, folder: &str, description: &str) -> PathBuf {
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: Demo\ndescription: {description}\n---\n\nDo the demo thing.\n"),
    )
    .expect("skill body");
    dir
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
if [ "$#" -gt 0 ]; then : > "$1"; fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo a string","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
  esac
done
"#,
    )
    .expect("fixture server");
    path
}

/// Issues a JSON request through the shared helper used by the other API tests.
async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = match body {
        Some(body) => axum::http::Request::builder()
            .header("host", "localhost")
            .method(method)
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => axum::http::Request::builder()
            .header("host", "localhost")
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("request"),
    };

    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

use tower::ServiceExt;

#[tokio::test]
async fn skills_round_trip_through_install_enable_and_remove() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    let (status, body) = send(&app, Method::GET, "/api/v1/skills", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skills"], json!([]));

    let source = write_skill_source(dir.path(), "demo-skill", "Explains the demo");
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": source.to_string_lossy(), "id": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("demo"));
    assert_eq!(body["description"], json!("Explains the demo"));

    let (_, body) = send(&app, Method::GET, "/api/v1/skills", None).await;
    assert_eq!(body["skills"].as_array().expect("array").len(), 1);
    assert_eq!(body["skills"][0]["enabled"], json!(true));

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/skills/demo/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!(true));

    let (_, body) = send(&app, Method::GET, "/api/v1/skills", None).await;
    assert_eq!(body["skills"][0]["enabled"], json!(false));

    // A file replacement preserves the disabled setting in both its response and the catalog.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": source.to_string_lossy(), "id": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));
    std::fs::write(source.join("SKILL.md"), [0xff]).unwrap();
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": source.to_string_lossy(), "id": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, body) = send(&app, Method::GET, "/api/v1/skills", None).await;
    assert_eq!(body["skills"][0]["description"], json!("Explains the demo"));
    assert_eq!(body["skills"][0]["enabled"], json!(false));

    let (status, _) = send(&app, Method::DELETE, "/api/v1/skills/demo", None).await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = send(&app, Method::GET, "/api/v1/skills", None).await;
    assert_eq!(body["skills"], json!([]));
}

#[tokio::test]
async fn skills_reject_unknown_targets_and_bad_identifiers() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    // Toggling a skill that is not installed records no state: a typo must not silently vanish.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/skills/ghost/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, Method::DELETE, "/api/v1/skills/ghost", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": dir.path().to_string_lossy(), "id": "bad id" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A directory without SKILL.md is not an installable skill.
    let empty = dir.path().join("not-a-skill");
    std::fs::create_dir_all(&empty).expect("dir");
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": empty.to_string_lossy(), "id": "not-a-skill" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_zip_upload_installs_the_wrapped_skill_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    // "Zip this folder" produces one wrapping directory; the upload path must find SKILL.md in it.
    let mut buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        writer
            .start_file::<_, ()>(
                "demo-skill/SKILL.md",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("zip entry");
        std::io::Write::write_all(
            &mut writer,
            b"---\nname: Zipped\ndescription: Zipped skill\n---\n\nBody.\n",
        )
        .expect("zip body");
        writer.finish().expect("zip finish");
    }

    let boundary = "kanon-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skill.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&buffer);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = axum::http::Request::builder()
        .header("host", "localhost")
        .method(Method::POST)
        .uri("/api/v1/skills")
        .header(
            axum::http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("router response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let decoded: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(decoded["id"], json!("demo-skill"));
    assert_eq!(decoded["name"], json!("Zipped"));

    // The identifier came from the wrapping directory, and the body is readable through the store.
    let installed = dir
        .path()
        .join("skills")
        .join("demo-skill")
        .join("SKILL.md");
    assert!(installed.is_file(), "the skill body must be installed");
}

#[tokio::test]
async fn a_zip_upload_rejects_an_archive_without_a_skill() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    let mut buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        writer
            .start_file::<_, ()>("readme.txt", zip::write::SimpleFileOptions::default())
            .expect("zip entry");
        std::io::Write::write_all(&mut writer, b"nothing to see").expect("zip body");
        writer.finish().expect("zip finish");
    }

    let boundary = "kanon-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skill.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&buffer);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = axum::http::Request::builder()
        .header("host", "localhost")
        .method(Method::POST)
        .uri("/api/v1/skills")
        .header(
            axum::http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("router response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn chat_without_plugin_tools_does_not_start_mcp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let (state, recorder) = extension_state_recording(dir.path()).await;
    let script = write_fixture_mcp_server(dir.path());
    let started = dir.path().join("mcp-started");
    // Load an enabled definition without using the management endpoint, which deliberately
    // connects enabled servers when saved. This models a pool that has not been reached yet.
    state
        .mcp_config()
        .upsert(McpServerConfig {
            id: "lazy".into(),
            name: "Lazy fixture".into(),
            transport: McpTransport::Stdio {
                command: "sh".into(),
                args: vec![
                    script.to_string_lossy().into_owned(),
                    started.to_string_lossy().into_owned(),
                ],
                env: Default::default(),
            },
        })
        .await
        .unwrap();
    state.mcp().sync_from_config(state.mcp_config()).await;
    let app = kanon_api::app(state.clone());

    for stream in [false, true] {
        let (status, _) = send(
            &app,
            Method::POST,
            "/api/v1/chat/completions",
            Some(json!({
                "session_id": format!("standalone-{stream}"),
                "message": "hi",
                "tools": false,
                "stream": stream,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // The flag controls external tools; native tools retain their existing behavior.
        assert_eq!(recorder.offered(), vec!["read_skill".to_string()]);
        assert!(
            !started.exists(),
            "MCP discovery must not launch an unused server"
        );
        assert!(!state.mcp().get("lazy").await.unwrap().is_connected().await);
    }

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "with-tools", "message": "hi", "tools": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(started.is_file());
    assert!(recorder.offered().contains(&"mcp__lazy__echo".to_string()));
}

#[tokio::test]
async fn chat_completions_offer_mcp_tools_to_the_model() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, recorder) = extension_state_recording(dir.path()).await;
    let app = kanon_api::app(state.clone());

    let script = write_fixture_mcp_server(dir.path());
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx",
        Some(json!({
            "name": "Fixture",
            "transport": { "type": "stdio", "command": "sh", "args": [script.to_string_lossy()] },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["health"]["state"], json!("connected"));

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "sandbox",
            "message": "hi",
            "tools": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let offered = recorder.offered();
    assert!(
        offered.contains(&"mcp__fx__echo".to_string()),
        "MCP tools must reach the model, got {offered:?}"
    );

    // Disabling the server node-wide removes its tools without deleting the definition.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Re-saving the same definition and editing it must both preserve the disabled process
    // state, rather than connecting a server that the catalog still labels disabled.
    for name in ["Fixture", "Edited Fixture"] {
        let (status, body) = send(
            &app,
            Method::PUT,
            "/api/v1/mcp/servers/fx",
            Some(json!({
                "name": name,
                "transport": { "type": "stdio", "command": "sh", "args": [script.to_string_lossy()] },
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], json!(false));
        assert_eq!(body["health"]["state"], json!("disconnected"));
        assert!(!state.mcp().get("fx").await.unwrap().is_connected().await);
    }

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "sandbox",
            "message": "hi",
            "tools": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !recorder.offered().contains(&"mcp__fx__echo".to_string()),
        "a disabled server must not offer tools"
    );

    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx/enabled",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.mcp().get("fx").await.unwrap().is_connected().await);
    let (_, catalog) = send(&app, Method::GET, "/api/v1/mcp/servers", None).await;
    assert_eq!(catalog["servers"][0]["name"], json!("Edited Fixture"));
    assert_eq!(catalog["servers"][0]["enabled"], json!(true));
    assert_eq!(catalog["servers"][0]["health"]["state"], json!("connected"));
}

#[tokio::test]
async fn mcp_servers_round_trip_through_save_enable_and_remove() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    let (status, body) = send(&app, Method::GET, "/api/v1/mcp/servers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["servers"], json!([]));

    // A stdio server whose command cannot start still saves: the definition is valid, the health
    // snapshot reports the connection failure instead of hiding it behind a 200 "all good".
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/files",
        Some(json!({
            "name": "Files",
            "transport": { "type": "stdio", "command": "kanon-no-such-command", "args": ["--root", "/tmp"] },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("files"));
    assert_eq!(body["name"], json!("Files"));
    assert_eq!(body["host_id"], json!("mcp_files"));
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["transport"]["args"], json!(["--root", "/tmp"]));

    let (_, body) = send(&app, Method::GET, "/api/v1/mcp/servers", None).await;
    assert_eq!(body["servers"].as_array().expect("array").len(), 1);

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/files/enabled",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!(true));

    let (_, body) = send(&app, Method::GET, "/api/v1/mcp/servers", None).await;
    assert_eq!(body["servers"][0]["enabled"], json!(false));

    let (status, _) = send(&app, Method::DELETE, "/api/v1/mcp/servers/files", None).await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = send(&app, Method::GET, "/api/v1/mcp/servers", None).await;
    assert_eq!(body["servers"], json!([]));
}

/// A failed definition write keeps the process running; later cleanup failure is partial success.
#[tokio::test]
async fn mcp_removal_commits_before_disconnect_and_reports_cleanup_failure() {
    let dir = tempfile::tempdir().unwrap();
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state.clone());
    let script = write_fixture_mcp_server(dir.path());
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx",
        Some(json!({
            "transport": {"type": "stdio", "command": "sh", "args": [script.to_string_lossy()]}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let server = state.mcp().get("fx").await.unwrap();
    assert!(server.is_connected().await);

    let path = dir.path().join("mcp.json");
    let document = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx",
        Some(json!({ "name": "replacement", "transport": server.config().transport })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state.mcp_config().get("fx").await.unwrap(),
        *server.config()
    );
    let (status, _) = send(&app, Method::DELETE, "/api/v1/mcp/servers/fx", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(state.mcp_config().get("fx").await.is_some());
    assert!(server.is_connected().await);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, document).unwrap();

    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/fx/enabled",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let toggle_path = dir.path().join("toggles.json");
    std::fs::remove_file(&toggle_path).unwrap();
    std::fs::create_dir(&toggle_path).unwrap();
    let (status, body) = send(&app, Method::DELETE, "/api/v1/mcp/servers/fx", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true);
    assert!(body["warning"].as_str().unwrap().contains("was removed"));
    assert!(state.mcp_config().get("fx").await.is_none());
    assert!(state.mcp().get("fx").await.is_none());
    assert!(!server.is_connected().await);
    let persisted: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted["servers"], json!([]));
}

/// Closing the HTTP waiter must not strand a durable definition outside the runtime pool.
#[tokio::test]
async fn canceled_mcp_save_still_publishes_the_definition() {
    let dir = tempfile::tempdir().unwrap();
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state.clone());
    let guard = state.mcp_config().lock_server("canceled").await;
    let mut request = Box::pin(send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/canceled",
        Some(json!({
            "transport": {"type": "stdio", "command": "kanon-no-such-command"}
        })),
    ));
    assert!(futures_util::poll!(&mut request).is_pending());
    drop(request);

    // Another identifier can finish while the canceled request still waits for its own lock.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/independent",
        Some(json!({
            "transport": {"type": "stdio", "command": "kanon-no-such-command"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.mcp_config().get("canceled").await.is_none());
    drop(guard);

    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(server) = state.mcp().get("canceled").await {
                if server.health().await.last_error.is_some() {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(state.mcp_config().get("canceled").await.is_some());
    let persisted: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("mcp.json")).unwrap()).unwrap();
    assert_eq!(persisted["servers"][0]["id"], "canceled");
}

#[tokio::test]
async fn mcp_rejects_unknown_servers_and_bad_identifiers() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = extension_state(dir.path()).await;
    let app = kanon_api::app(state);

    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/ghost/enabled",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, Method::DELETE, "/api/v1/mcp/servers/ghost", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The identifier becomes part of a tool name, so anything outside the safe set is refused.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/bad%20id",
        Some(json!({
            "transport": { "type": "http", "url": "https://example.com/mcp" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An empty transport URL is rejected by the store, not accepted and then failed later.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/mcp/servers/blank",
        Some(json!({ "transport": { "type": "http", "url": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_chat_turn_carries_the_persona_and_the_skill_catalog() {
    // Regression guard for the real request composition: the skill catalog is injected next to the
    // persona in the static system block, and the persona hook must not overwrite it. This test drives the HTTP route,
    // so it also covers the wiring between the state builder, the factory and the agent.
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, recorder) = extension_state_recording(dir.path()).await;
    let app = kanon_api::app(state);

    let source = write_skill_source(dir.path(), "probe-skill", "PROBE-CATALOG-MARKER");
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/skills",
        Some(json!({ "path": source.to_string_lossy(), "id": "probe" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({ "session_id": "context-probe", "message": "hi", "tools": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let messages = recorder.messages();
    let systems: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == kanon_llm::gateway::types::Role::System)
        .filter_map(|message| message.content.as_deref())
        .collect();

    // Persona and skill catalog form one static system block, persona first.
    assert_eq!(
        systems.len(),
        1,
        "one merged system block, got {messages:?}"
    );
    assert!(
        systems[0].starts_with(kanon_llm::BASE_PERSONA_PROMPT),
        "the persona leads the static block: {systems:?}"
    );
    assert!(
        systems[0].contains("PROBE-CATALOG-MARKER"),
        "the catalog must be in the request: {systems:?}"
    );
}
