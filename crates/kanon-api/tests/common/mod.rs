#![allow(dead_code)]
//! Shared fixtures and HTTP helpers for the `kanon-api` integration tests.
//!
//! Each integration test binary compiles this module independently and exercises a different
//! subset of the helpers, so unused-item warnings are expected and suppressed here only.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use kanon_api::{ApiState, default_agent_config};
use kanon_core::supervisor::Supervisor;
use kanon_core::{AdapterError, EventIngress, PlatformAdapter, PluginManifest};
use kanon_llm::{ChatRequest, ChatResponse, GatewayError, LlmProvider, TokenUsage};
use kanon_proto::v1::{CommandMeta, PluginMeta, ToolMeta};
use kanon_proto::v1::{DeliverMessageRequest, DeliverMessageResponse, IngestEventRequest};
use serde_json::Value;
use tower::ServiceExt;

/// Identifier of the fixture plugin used across the API tests.
pub const FIXTURE_PLUGIN_ID: &str = "org.kanon.plugin.fixture";

/// Host identifier of the fixture plugin host.
pub const FIXTURE_HOST_ID: &str = "org_kanon_plugin_fixture";

/// Deterministic provider stub returning a fixed completion.
pub struct MockProvider {
    content: String,
}

impl MockProvider {
    /// Creates a provider that always answers with `content`.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            content: Some(self.content.clone()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: Some(TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 5,
                total_tokens: 12,
                ..TokenUsage::default()
            }),
        })
    }
}

/// Fixture manifest mirroring the documented `plugin.toml` schema.
pub fn fixture_manifest() -> PluginManifest {
    let toml = r#"
[plugin]
id = "org.kanon.plugin.fixture"
name = "Fixture Plugin"
version = "2.1.0"
author = "Kanon Test"
description = "Fixture plugin used by API integration tests"
runtime = "rust"
entrypoint = "target/debug/fixture"
priority = 120

[adapter]
platform = "fixture_platform"
display_name = "Fixture Platform"

[config_schema]
type = "object"
additionalProperties = false
properties = { api_key = { type = "string" }, default_city = { type = "string", default = "Beijing" }, enable_cache = { type = "boolean", default = true } }
required = ["api_key"]

[[commands]]
name = "fixture"
description = "Fixture command"
usage = "/fixture <expr>"

[[tools]]
name = "fixture_tool"
description = "Fixture tool"
parameters = { type = "object", properties = { expr = { type = "string" } } }
"#;

    toml::from_str(toml).expect("fixture manifest must parse")
}

/// Fixture plugin metadata as reported by a live host handshake.
pub fn fixture_meta() -> PluginMeta {
    PluginMeta {
        id: FIXTURE_PLUGIN_ID.to_string(),
        name: "Fixture Plugin".to_string(),
        version: "2.1.0".to_string(),
        author: "Kanon Test".to_string(),
        description: "Fixture plugin used by API integration tests".to_string(),
        commands: vec![CommandMeta {
            name: "fixture".to_string(),
            description: "Fixture command".to_string(),
            usage: "/fixture <expr>".to_string(),
            priority: 100,
            ..Default::default()
        }],
        tools: vec![ToolMeta {
            name: "fixture_tool".to_string(),
            description: "Fixture tool".to_string(),
            parameters: None,
        }],
        ..Default::default()
    }
}

/// Creates a lazily connected channel pointing at a closed port.
///
/// Any RPC issued through it fails immediately, which is exactly what the tests need to verify
/// that upstream failures are surfaced instead of being swallowed.
pub fn dead_channel() -> tonic::transport::Channel {
    tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy()
}

/// Registers a fixture plugin host on the supervisor.
///
/// The host carries a manifest (so static metadata and the config schema are available) but no
/// launch recipe, mirroring an externally attached process that the control plane cannot restart.
pub async fn register_fixture_host(supervisor: &Supervisor) -> Arc<kanon_core::ManagedHost> {
    let host = Arc::new(
        kanon_core::ManagedHost::new(
            FIXTURE_HOST_ID.to_string(),
            PathBuf::from("/tmp/kanon-test/host_fixture.sock"),
            dead_channel(),
            vec![fixture_meta()],
            120,
        )
        .with_manifest(fixture_manifest()),
    );

    supervisor.register_managed_host(host.clone()).await;
    host
}

/// Builds gateway state over a fresh supervisor with the fixture plugin host registered.
///
/// `with_agent` attaches a deterministic mock provider; without it the gateway must answer
/// `503` for chat completions rather than inventing a fallback.
pub async fn fixture_state(config_dir: PathBuf, with_agent: bool) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    register_fixture_host(&supervisor).await;

    // The supervisor owns socket cleanup for its run directory, so the guard is intentionally
    // detached from the returned state; only the config directory must outlive this function.
    std::mem::forget(temp);

    let mut builder = ApiState::builder(supervisor)
        .with_config_dir(config_dir.clone())
        .with_plugins_dir(config_dir.join("plugins"));
    if with_agent {
        builder = builder.with_llm_provider(
            "test-agent",
            Arc::new(MockProvider::new("fixture reply")),
            default_agent_config("mock-model"),
        );
    }

    builder.build()
}

/// Builds gateway state without any plugin host registered.
pub async fn empty_state(config_dir: PathBuf) -> ApiState {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);
    ApiState::builder(supervisor)
        .with_config_dir(config_dir)
        .build()
}

/// Issues a JSON request against the router and returns the status with the decoded JSON body.
pub async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = match body {
        Some(body) => Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request build"),
        None => Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("request build"),
    };

    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body bytes");
    let decoded = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    (status, decoded)
}

/// Issues a request and returns the status with the raw response body and content type.
pub async fn send_raw(app: &Router, method: Method, uri: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request build");

    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body bytes");

    (
        status,
        String::from_utf8_lossy(&bytes).to_string(),
        content_type,
    )
}

/// Issues a JSON request and returns the status with the raw body and content type.
///
/// Used for endpoints that answer with a non-JSON body, such as `text/event-stream` chat
/// completions or Prometheus text exposition.
pub async fn send_raw_json(app: &Router, uri: &str, body: Value) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request build");

    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body bytes");

    (
        status,
        String::from_utf8_lossy(&bytes).to_string(),
        content_type,
    )
}

/// Extracts the machine-readable error code from an error envelope.
pub fn error_code(body: &Value) -> &str {
    body.get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Platform identifier of the built-in adapter registered by [`adapter_state`].
pub const BUILTIN_PLATFORM: &str = "builtin_platform";

/// Platform identifier declared by the fixture plugin manifest.
pub const PLUGIN_PLATFORM: &str = "fixture_platform";

/// Built-in adapter that records every delivered message in memory.
#[derive(Default)]
pub struct RecordingAdapter {
    deliveries: std::sync::Mutex<Vec<DeliverMessageRequest>>,
}

impl RecordingAdapter {
    /// Creates an empty recording adapter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of the messages delivered so far.
    pub fn deliveries(&self) -> Vec<DeliverMessageRequest> {
        self.deliveries.lock().expect("delivery lock").clone()
    }
}

#[async_trait]
impl PlatformAdapter for RecordingAdapter {
    fn platform(&self) -> &str {
        BUILTIN_PLATFORM
    }

    fn display_name(&self) -> &str {
        "Built-in fixture adapter"
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        let message_id = format!("builtin-{}", request.channel_id);
        self.deliveries.lock().expect("delivery lock").push(request);

        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }
}

/// Builds gateway state with both adapter routes available and an ingest queue the test can drain.
///
/// Returns the state, the receiving end of the ingest channel (so tests can assert what the
/// gateway actually enqueued), and the built-in adapter (so tests can assert what it delivered).
pub async fn adapter_state(
    config_dir: PathBuf,
    capacity: usize,
) -> (
    ApiState,
    tokio::sync::mpsc::Receiver<IngestEventRequest>,
    Arc<RecordingAdapter>,
) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    register_fixture_host(&supervisor).await;

    let adapter = Arc::new(RecordingAdapter::new());
    supervisor
        .adapters()
        .register(adapter.clone())
        .await
        .expect("built-in adapter registers");

    let (ingest_tx, ingest_rx) = tokio::sync::mpsc::channel(capacity);
    let state = ApiState::builder(supervisor)
        .with_config_dir(config_dir)
        .with_ingress(EventIngress::new(ingest_tx))
        .build();

    (state, ingest_rx, adapter)
}
