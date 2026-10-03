//! Plugin catalog, configuration and restart route coverage.

mod common;

use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use serde_json::json;

use common::{FIXTURE_HOST_ID, FIXTURE_PLUGIN_ID, error_code, fixture_state, send_json};

/// The catalog merges live host metadata with the static manifest declaration.
#[tokio::test]
async fn plugin_catalog_lists_hosts_and_plugins() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/plugins", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);

    let host = &body["hosts"][0];
    assert_eq!(host["host_id"], FIXTURE_HOST_ID);
    assert_eq!(host["status"], "running");
    assert_eq!(host["runtime"], "rust");
    assert_eq!(host["priority"], 120);
    // Externally registered hosts carry no launch recipe, so they cannot be restarted.
    assert_eq!(host["restartable"], false);

    let plugin = &body["plugins"][0];
    assert_eq!(plugin["id"], FIXTURE_PLUGIN_ID);
    assert_eq!(plugin["version"], "2.1.0");
    assert_eq!(plugin["status"], "running");
    assert_eq!(plugin["commands"][0]["name"], "fixture");
    assert_eq!(plugin["tools"][0]["name"], "fixture_tool");
}

/// The catalog is empty (not an error) when no host is registered.
#[tokio::test]
async fn plugin_catalog_is_empty_without_hosts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(common::empty_state(PathBuf::from(dir.path())).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/plugins", None).await;

    assert_eq!(status, 200);
    assert_eq!(body["total"], 0);
    assert_eq!(body["plugins"].as_array().map(Vec::len), Some(0));
}

/// Configuration reads expose the declared schema with defaults merged in.
#[tokio::test]
async fn plugin_config_returns_schema_and_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config"),
        None,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["plugin_id"], FIXTURE_PLUGIN_ID);
    assert_eq!(body["persisted"], false);
    assert_eq!(body["schema"]["required"][0], "api_key");
    assert_eq!(body["values"]["default_city"], "Beijing");
    assert_eq!(body["values"]["enable_cache"], true);
    // `api_key` has no default, so it must not be invented.
    assert!(body["values"].get("api_key").is_none());
}

/// Unknown plugins produce a structured `404`.
#[tokio::test]
async fn plugin_config_unknown_plugin_returns_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/plugins/org.kanon.plugin.missing/config",
        None,
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// Schema violations are rejected before any host round trip happens.
#[tokio::test]
async fn plugin_config_rejects_schema_violations() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);
    let uri = format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config");

    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "default_city": "Shanghai" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("api_key")),
        "error must name the missing property: {body}"
    );

    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k", "enable_cache": "yes" } })),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("enable_cache")),
        "error must name the mistyped property: {body}"
    );

    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k", "surprise": 1 } })),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("surprise")),
        "additionalProperties=false must reject unknown keys: {body}"
    );

    let (status, body) = send_json(&app, Method::PUT, &uri, Some(json!({ "values": 7 }))).await;
    assert_eq!(status, 400);
    assert!(body["error"]["message"].as_str().is_some());
}

/// A rejected configuration payload is never persisted to disk.
#[tokio::test]
async fn plugin_config_is_not_persisted_when_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config_dir = PathBuf::from(dir.path());
    let app: Router = app(fixture_state(config_dir.clone(), false).await);
    let uri = format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config");

    let (status, _) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k", "unknown": true } })),
    )
    .await;
    assert_eq!(status, 400);

    assert!(
        !config_dir
            .join(FIXTURE_PLUGIN_ID)
            .join("config.json")
            .exists(),
        "validation failures must not leave a configuration file behind"
    );
}

/// A valid payload reaching an unreachable host surfaces as an upstream failure.
#[tokio::test]
async fn plugin_config_reports_upstream_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config_dir = PathBuf::from(dir.path());
    let app: Router = app(fixture_state(config_dir.clone(), false).await);
    let uri = format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config");

    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "secret", "default_city": "Hangzhou" } })),
    )
    .await;

    // The fixture host listens on a closed port, so the reload RPC fails explicitly.
    assert_eq!(status, 502, "body: {body}");
    assert_eq!(error_code(&body), "upstream_error");

    // The tentative file is restored when the hot reload cannot be confirmed.
    assert!(
        !config_dir
            .join(FIXTURE_PLUGIN_ID)
            .join("config.json")
            .exists(),
        "a failed hot reload must not persist configuration"
    );
}

/// Restarting a host without a recorded launch recipe is a conflict, not a silent no-op.
#[tokio::test]
async fn plugin_restart_without_launch_spec_is_conflict() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/restart"),
        None,
    )
    .await;

    assert_eq!(status, 409);
    assert_eq!(error_code(&body), "conflict");
    assert_eq!(
        body["error"]["message"],
        format!(
            "Host '{FIXTURE_HOST_ID}' has no recorded launch specification and cannot be restarted"
        )
    );
}

/// Restarting an unknown plugin reports `404`.
#[tokio::test]
async fn plugin_restart_unknown_plugin_returns_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/org.kanon.plugin.missing/restart",
        None,
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// Plugin identifiers that could escape the data sandbox are rejected outright.
#[tokio::test]
async fn plugin_config_rejects_path_traversal_identifier() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/plugins/..%2F..%2Fetc/config",
        None,
    )
    .await;

    // Either the router rejects the encoded path (404) or the store rejects the identifier (400);
    // both are explicit failures and neither may touch the filesystem outside the sandbox.
    assert!(
        status == 404 || status == 400,
        "expected explicit rejection, got {status}: {body}"
    );
}

/// Config update endpoint PUT /api/v1/plugins/{id}/config enforces CAS version token.
#[tokio::test]
async fn plugin_config_cas_version_enforcement() {
    use kanon_core::ManagedHost;
    use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
    use kanon_proto::v1::{
        GetPluginMetaRequest, GetPluginMetaResponse, PingRequest, PingResponse,
        PluginActionRequest, PluginActionResponse, ReloadPluginConfigRequest,
        ReloadPluginConfigResponse,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tonic::{Request, Response, Status};

    struct CasMockHost {
        ver: AtomicU64,
    }

    #[tonic::async_trait]
    impl PluginHostService for CasMockHost {
        /// The CAS fixture declares no management actions.
        async fn invoke_action(
            &self,
            _req: Request<PluginActionRequest>,
        ) -> Result<Response<PluginActionResponse>, Status> {
            Err(Status::unimplemented("cas fixture has no actions"))
        }

        async fn ping(&self, req: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
            Ok(Response::new(PingResponse {
                timestamp: req.into_inner().timestamp,
            }))
        }
        async fn reload_plugin_config(
            &self,
            req: Request<ReloadPluginConfigRequest>,
        ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
            let r = req.into_inner();
            let cur = self.ver.load(Ordering::SeqCst);
            if r.version > 0 && r.version <= cur {
                return Ok(Response::new(ReloadPluginConfigResponse {
                    success: false,
                    error_message: format!("Stale: cur={cur}, req={}", r.version),
                    applied_version: cur,
                }));
            }
            self.ver.store(r.version, Ordering::SeqCst);
            Ok(Response::new(ReloadPluginConfigResponse {
                success: true,
                error_message: String::new(),
                applied_version: r.version,
            }))
        }
        async fn get_plugin_meta(
            &self,
            _: Request<GetPluginMetaRequest>,
        ) -> Result<Response<GetPluginMetaResponse>, Status> {
            Ok(Response::new(GetPluginMetaResponse { plugins: vec![] }))
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(CasMockHost {
                ver: AtomicU64::new(0),
            }))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let config_dir = PathBuf::from(dir.path());
    let supervisor = Arc::new(kanon_core::Supervisor::new(
        Some(config_dir.join("run")),
        None,
    ));

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();

    let host = Arc::new(
        ManagedHost::new(
            "cas_host".to_string(),
            config_dir.join("cas_host.sock"),
            channel,
            vec![common::fixture_meta()],
            100,
        )
        .with_manifest(common::fixture_manifest()),
    );
    supervisor.register_managed_host(host).await;

    let state = kanon_api::ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .build();
    let app: Router = app(state);

    let uri = format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config");

    // 1. Initial GET reports version 0
    let (status, body) = send_json(&app, Method::GET, &uri, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["version"], 0);

    // 2. PUT with mismatched expected version (e.g. 5) returns 409 Conflict
    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k1" }, "version": 5 })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(error_code(&body), "conflict");

    // 3. PUT with matching expected version 0 succeeds and returns version 1
    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k1" }, "version": 0 })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["reloaded"], true);
    assert_eq!(body["version"], 1);

    // 4. Stale PUT with version 0 now fails with 409 Conflict
    let (status, body) = send_json(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "values": { "api_key": "k2" }, "version": 0 })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(error_code(&body), "conflict");

    // 5. Subsequent GET reports version 1
    let (status, body) = send_json(&app, Method::GET, &uri, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["version"], 1);
}

/// Invoking a tool on an unregistered plugin returns 404.
#[tokio::test]
async fn plugin_tool_call_unknown_plugin_returns_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/unknown.plugin/tools/test_tool",
        Some(json!({ "arguments": {} })),
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// Invoking a tool when the host is dead surfaces an internal server error.
#[tokio::test]
async fn plugin_tool_call_reports_upstream_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/tools/fixture_tool"),
        Some(json!({ "arguments": { "expr": "echo" } })),
    )
    .await;

    assert_eq!(status, 500);
    assert_eq!(error_code(&body), "internal_error");
}

/// An unknown plugin is a 404 for management actions too, not an internal error.
#[tokio::test]
async fn plugin_action_unknown_plugin_returns_404() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/unknown.plugin/actions/anything",
        Some(json!({ "arguments": {} })),
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "not_found");
}

/// A dead host surfaces as an upstream failure, exactly like a tool call would.
#[tokio::test]
async fn plugin_action_reports_upstream_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/actions/bind_credentials"),
        Some(json!({ "arguments": { "mode": "qr" } })),
    )
    .await;

    assert_eq!(status, 502, "unexpected body: {body}");
}

/// A live host answers management actions with its structured result.
#[tokio::test]
async fn plugin_action_returns_host_result() {
    use kanon_core::ManagedHost;
    use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
    use kanon_proto::v1::{
        GetPluginMetaRequest, GetPluginMetaResponse, PingRequest, PingResponse,
        PluginActionRequest, PluginActionResponse, ReloadPluginConfigRequest,
        ReloadPluginConfigResponse,
    };
    use std::sync::Arc;
    use tonic::{Request, Response, Status};

    struct ActionMockHost;

    #[tonic::async_trait]
    impl PluginHostService for ActionMockHost {
        /// Only the declared action succeeds; anything else is an explicit failure.
        async fn invoke_action(
            &self,
            req: Request<PluginActionRequest>,
        ) -> Result<Response<PluginActionResponse>, Status> {
            let req = req.into_inner();
            if req.action != "bind_credentials" {
                return Ok(Response::new(PluginActionResponse {
                    success: false,
                    error_message: format!("Unknown action '{}'", req.action),
                    result: None,
                }));
            }

            let result = kanon_llm::tool_router::json_to_prost_struct(&json!({
                "task_id": "task-42",
                "qrcode_url": "https://q.qq.com/connect?task_id=task-42"
            }))
            .expect("action result");

            Ok(Response::new(PluginActionResponse {
                success: true,
                error_message: String::new(),
                result: Some(result),
            }))
        }

        async fn ping(&self, req: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
            Ok(Response::new(PingResponse {
                timestamp: req.into_inner().timestamp,
            }))
        }

        async fn reload_plugin_config(
            &self,
            _req: Request<ReloadPluginConfigRequest>,
        ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
            Ok(Response::new(ReloadPluginConfigResponse {
                success: true,
                error_message: String::new(),
                applied_version: 1,
            }))
        }

        async fn get_plugin_meta(
            &self,
            _req: Request<GetPluginMetaRequest>,
        ) -> Result<Response<GetPluginMetaResponse>, Status> {
            Ok(Response::new(GetPluginMetaResponse { plugins: vec![] }))
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(ActionMockHost))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let config_dir = PathBuf::from(dir.path());
    let supervisor = Arc::new(kanon_core::Supervisor::new(
        Some(config_dir.join("run")),
        None,
    ));

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();

    let host = Arc::new(
        ManagedHost::new(
            "action_host".to_string(),
            config_dir.join("action_host.sock"),
            channel,
            vec![common::fixture_meta()],
            100,
        )
        .with_manifest(common::fixture_manifest()),
    );
    supervisor.register_managed_host(host).await;

    let state = kanon_api::ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .build();
    let app: Router = app(state);

    // 1. A declared action returns its structured payload.
    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/actions/bind_credentials"),
        Some(json!({ "arguments": { "mode": "qr" } })),
    )
    .await;
    assert_eq!(status, 200, "unexpected body: {body}");
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["result"]["task_id"], json!("task-42"));

    // 2. An undeclared action is an explicit failure, never a silent success.
    let (status, body) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/actions/not_declared"),
        Some(json!({ "arguments": {} })),
    )
    .await;
    assert_eq!(status, 200, "unexpected body: {body}");
    assert_eq!(body["success"], json!(false));
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown action"),
        "unexpected body: {body}"
    );
}
