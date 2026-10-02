//! Plugin web surface: static pages under `/pages/` and HTTP forwarding under `/http/`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use kanon_api::{ApiState, app};
use kanon_core::ManagedHost;
use kanon_proto::v1::message_pipeline_service_server::{
    MessagePipelineService, MessagePipelineServiceServer,
};
use kanon_proto::v1::{HttpHeader, HttpRequest, HttpResponse, PluginMeta};
use serde_json::{Value, json};
use tower::ServiceExt;

use common::{dead_channel, error_code, fixture_state, send_json};

const PAGES_PLUGIN: &str = "org.kanon.test.pages";
const WEB_PLUGIN: &str = "org.kanon.test.web";

/// Writes a plugin with console pages into the state's plugin directory and rescans it.
fn install_pages_plugin(state: &ApiState) -> PathBuf {
    let dir = state.plugins_dir().join(PAGES_PLUGIN);
    std::fs::create_dir_all(dir.join("pages/sub")).unwrap();
    std::fs::create_dir_all(dir.join("pages/empty")).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "[plugin]\nid = \"{PAGES_PLUGIN}\"\nname = \"Pages\"\nversion = \"1.0.0\"\n\
             runtime = \"python\"\nentrypoint = \"main.py\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("secret.txt"), "outside the pages folder").unwrap();
    std::fs::write(dir.join("pages/index.html"), "<h1>home</h1>").unwrap();
    std::fs::write(dir.join("pages/app.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("pages/.env"), "TOKEN=1").unwrap();
    std::fs::write(dir.join("pages/sub/index.html"), "<h1>sub</h1>").unwrap();
    state.rescan_plugins().expect("rescan");
    dir
}

/// Sends a bodiless request and returns status, headers and body text.
async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn pages_serve_files_with_type_and_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    install_pages_plugin(&state);
    let app = app(state);

    let (status, headers, body) =
        get(&app, &format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<h1>home</h1>");
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    // Plugin content must never run with the console's origin.
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("sandbox")
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    let (status, headers, body) = get(
        &app,
        &format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/app.js"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log(1)");
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );

    let (status, _, body) = get(&app, &format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/sub/")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<h1>sub</h1>");
}

#[tokio::test]
async fn pages_redirect_directories_to_their_slash_form() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    install_pages_plugin(&state);
    let app = app(state);

    // Relative links in index.html only resolve correctly below the slash-terminated URL.
    for (from, to) in [
        (
            format!("/api/v1/plugins/{PAGES_PLUGIN}/pages"),
            format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/"),
        ),
        (
            format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/sub?x=1"),
            format!("/api/v1/plugins/{PAGES_PLUGIN}/pages/sub/?x=1"),
        ),
    ] {
        let (status, headers, _) = get(&app, &from).await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT, "{from}");
        assert_eq!(headers[header::LOCATION], to.as_str());
    }
}

#[tokio::test]
async fn pages_refuse_traversal_hidden_files_and_listings() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugin_dir = install_pages_plugin(&state);
    #[cfg(unix)]
    std::os::unix::fs::symlink(plugin_dir.join("secret.txt"), plugin_dir.join("pages/link"))
        .unwrap();
    let app = app(state);
    let base = format!("/api/v1/plugins/{PAGES_PLUGIN}/pages");

    // Every spelling of `..` is decoded before the check, so all are the same refusal.
    for path in [
        "../secret.txt",
        "%2e%2e/secret.txt",
        "..%2fsecret.txt",
        "sub/..%2F..%2Fsecret.txt",
    ] {
        let (status, _, body) = get(&app, &format!("{base}/{path}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(body.contains("relative segments"), "{path}: {body}");
    }

    for path in [".env", "empty/", "missing.html"] {
        let (status, _, _) = get(&app, &format!("{base}/{path}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }

    // A symlink pointing out of the folder is treated as missing, not followed.
    #[cfg(unix)]
    {
        let (status, _, body) = get(&app, &format!("{base}/link")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("outside the pages folder"));
    }

    let (status, _, _) = get(&app, "/api/v1/plugins/org.kanon.test.unknown/pages/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Plugin host that echoes forwarded HTTP requests back as JSON.
struct WebHost;

#[tonic::async_trait]
impl MessagePipelineService for WebHost {
    async fn on_http_request(
        &self,
        request: tonic::Request<HttpRequest>,
    ) -> Result<tonic::Response<HttpResponse>, tonic::Status> {
        let request = request.into_inner();
        match request.path.as_str() {
            "/boom" => return Err(tonic::Status::internal("handler crashed")),
            "/bad-status" => {
                return Ok(tonic::Response::new(HttpResponse {
                    status: 1000,
                    ..Default::default()
                }));
            }
            "/default-status" => {
                return Ok(tonic::Response::new(HttpResponse {
                    body: b"ok".to_vec(),
                    ..Default::default()
                }));
            }
            _ => {}
        }
        let echo = json!({
            "plugin_id": request.plugin_id,
            "method": request.method,
            "path": request.path,
            "query": request.query,
            "headers": request.headers.iter().map(|h| h.name.clone()).collect::<Vec<_>>(),
            "custom": request.headers.iter().find(|h| h.name == "x-custom").map(|h| h.value.clone()),
            "body": String::from_utf8_lossy(&request.body),
        });
        Ok(tonic::Response::new(HttpResponse {
            status: 201,
            headers: vec![
                HttpHeader {
                    name: "content-type".to_string(),
                    value: "application/json".to_string(),
                },
                HttpHeader {
                    name: "x-plugin".to_string(),
                    value: "web".to_string(),
                },
                HttpHeader {
                    name: "connection".to_string(),
                    value: "close".to_string(),
                },
                HttpHeader {
                    name: "transfer-encoding".to_string(),
                    value: "chunked".to_string(),
                },
            ],
            body: echo.to_string().into_bytes(),
        }))
    }

    async fn on_llm_request(
        &self,
        _request: tonic::Request<kanon_proto::v1::LlmRequestHookRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::LlmRequestHookResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_llm_request"))
    }

    async fn on_pre_filter(
        &self,
        _request: tonic::Request<kanon_proto::v1::PipelineEventRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PreFilterResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_pre_filter"))
    }

    async fn on_execute_command(
        &self,
        _request: tonic::Request<kanon_proto::v1::CommandExecuteRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::CommandExecuteResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_execute_command"))
    }

    async fn on_call_tool(
        &self,
        _request: tonic::Request<kanon_proto::v1::ToolCallRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::ToolCallResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_call_tool"))
    }

    async fn on_event(
        &self,
        _request: tonic::Request<kanon_proto::v1::EventNotification>,
    ) -> Result<tonic::Response<kanon_proto::v1::EventAck>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_event"))
    }

    async fn on_decorate_reply(
        &self,
        _request: tonic::Request<kanon_proto::v1::DecorateReplyRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DecorateReplyResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_decorate_reply"))
    }

    async fn on_prepare_turn(
        &self,
        _request: tonic::Request<kanon_proto::v1::PrepareTurnRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PrepareTurnResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_prepare_turn"))
    }

    async fn on_deliver_message(
        &self,
        _request: tonic::Request<kanon_proto::v1::DeliverMessageRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DeliverMessageResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_deliver_message"))
    }
}

/// Metadata of a plugin that serves HTTP routes.
fn web_meta(id: &str) -> PluginMeta {
    PluginMeta {
        id: id.to_string(),
        name: "Web".to_string(),
        serves_http: true,
        ..Default::default()
    }
}

/// Starts the echoing host on a socket below `dir` and registers it with the supervisor.
async fn register_web_host(state: &ApiState, dir: &Path) {
    let socket = dir.join("host_web.sock");
    let listener = kanon_transport::IpcListener::bind(&socket).expect("host socket binds");
    let incoming = listener.incoming();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(MessagePipelineServiceServer::new(WebHost))
            .serve_with_incoming(incoming)
            .await;
    });

    let mut channel = None;
    for _ in 0..50 {
        match kanon_transport::connect_ipc(&socket).await {
            Ok(candidate) => {
                channel = Some(candidate);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    state
        .supervisor()
        .register_managed_host(Arc::new(ManagedHost::new(
            "host_web".to_string(),
            socket,
            channel.expect("web host reachable"),
            vec![web_meta(WEB_PLUGIN)],
            100,
        )))
        .await;
}

/// Sends a request with headers and a body, returning status, headers and decoded JSON.
async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Value, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap()
        .to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, json, bytes)
}

#[tokio::test]
async fn http_requests_are_forwarded_without_hop_by_hop_headers() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    register_web_host(&state, dir.path()).await;
    let app = app(state);

    let (status, headers, echo, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{WEB_PLUGIN}/http/items/a%20b?x=1&y=2"),
        &[
            ("x-custom", "kept"),
            ("connection", "x-drop"),
            ("x-drop", "connection-scoped"),
            ("te", "trailers"),
        ],
        b"payload".to_vec(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(echo["plugin_id"], WEB_PLUGIN);
    assert_eq!(echo["method"], "POST");
    // The path below `/http` is passed on raw, as any HTTP server would see it.
    assert_eq!(echo["path"], "/items/a%20b");
    assert_eq!(echo["query"], "x=1&y=2");
    assert_eq!(echo["custom"], "kept");
    assert_eq!(echo["body"], "payload");
    let names: Vec<&str> = echo["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for dropped in ["connection", "x-drop", "te"] {
        assert!(!names.contains(&dropped), "{dropped} must not be forwarded");
    }

    assert_eq!(headers["x-plugin"], "web");
    assert!(headers.get(header::CONNECTION).is_none());
    assert!(headers.get(header::TRANSFER_ENCODING).is_none());
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("sandbox")
    );

    // The root of the plugin's routes is "/", with or without the trailing slash.
    for uri in [
        format!("/api/v1/plugins/{WEB_PLUGIN}/http"),
        format!("/api/v1/plugins/{WEB_PLUGIN}/http/"),
    ] {
        let (status, _, echo, _) = send(&app, Method::GET, &uri, &[], Vec::new()).await;
        assert_eq!(status, StatusCode::CREATED, "{uri}");
        assert_eq!(echo["path"], "/");
        assert_eq!(echo["method"], "GET");
    }

    // An unset status is 200 by contract.
    let (status, _, _, body) = send(
        &app,
        Method::GET,
        &format!("/api/v1/plugins/{WEB_PLUGIN}/http/default-status"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"ok");
}

#[tokio::test]
async fn http_forwarding_maps_failures_to_gateway_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    register_web_host(&state, dir.path()).await;
    // A host that declares HTTP routes but cannot be reached.
    state
        .supervisor()
        .register_managed_host(Arc::new(ManagedHost::new(
            "host_down".to_string(),
            dir.path().join("host_down.sock"),
            dead_channel(),
            vec![web_meta("org.kanon.test.down")],
            100,
        )))
        .await;
    install_pages_plugin(&state);
    let app = app(state);

    let cases = [
        // The plugin's handler failed.
        (
            format!("/api/v1/plugins/{WEB_PLUGIN}/http/boom"),
            502,
            "upstream_error",
        ),
        // The plugin answered something that is not an HTTP status.
        (
            format!("/api/v1/plugins/{WEB_PLUGIN}/http/bad-status"),
            502,
            "upstream_error",
        ),
        // Running, but it does not serve HTTP.
        (
            format!("/api/v1/plugins/{}/http/x", common::FIXTURE_PLUGIN_ID),
            404,
            "not_found",
        ),
        // Not installed at all.
        (
            "/api/v1/plugins/org.kanon.test.nope/http/x".to_string(),
            404,
            "not_found",
        ),
        // Installed but its host is not running.
        (
            format!("/api/v1/plugins/{PAGES_PLUGIN}/http/x"),
            503,
            "unavailable",
        ),
        // Host registered but unreachable.
        (
            "/api/v1/plugins/org.kanon.test.down/http/x".to_string(),
            503,
            "unavailable",
        ),
    ];
    for (uri, status, code) in cases {
        let (actual, body) = send_json(&app, Method::GET, &uri, None).await;
        assert_eq!(actual.as_u16(), status, "{uri}: {body}");
        assert_eq!(error_code(&body), code, "{uri}");
    }
}

#[tokio::test]
async fn http_forwarding_refuses_oversized_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    register_web_host(&state, dir.path()).await;
    let app = app(state);

    let (status, _, body, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{WEB_PLUGIN}/http/upload"),
        &[],
        vec![0u8; kanon_api::routes::plugin_web::MAX_HTTP_BODY_BYTES + 1],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), "payload_too_large");
}
