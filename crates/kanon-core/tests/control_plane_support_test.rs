//! Coverage for the manifest extensions, restart safety and pipeline lifecycle observation
//! added to support the management control plane.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kanon_core::PluginManifest;
use kanon_core::pipeline::{PipelineEngine, PipelineObserver, PipelineResult, PipelineStage};
use kanon_core::supervisor::{LaunchSpec, ManagedHost, Supervisor, SupervisorError};
use kanon_proto::v1::{IngestEventRequest, PipelineEventRequest, PluginMeta};
use tempfile::tempdir;

/// Observer recording every emitted stage for later assertions.
#[derive(Default)]
struct RecordingObserver {
    stages: Mutex<Vec<PipelineStage>>,
}

impl PipelineObserver for RecordingObserver {
    fn on_stage(&self, stage: &PipelineStage) {
        self.stages.lock().expect("stage lock").push(stage.clone());
    }
}

/// The documented `[config_schema]` and `[[tools]]` sections are parsed.
#[test]
fn manifest_parses_config_schema_and_tools() {
    let toml = r#"
[plugin]
id = "org.kanon.plugin.weather"
name = "Weather"
version = "1.0.0"
author = "Kanon Dev"
description = "Weather lookup"
runtime = "python"
entrypoint = "main.py"

[config_schema]
type = "object"
properties = { api_key = { type = "string", title = "API Key" }, default_city = { type = "string", default = "Beijing" } }
required = ["api_key"]

[[commands]]
name = "weather"
description = "Look up weather"
usage = "/weather <city>"

[[tools]]
name = "fetch_weather"
description = "Fetch live weather"
parameters = { type = "object", properties = { city = { type = "string" } }, required = ["city"] }
"#;

    let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");

    assert_eq!(manifest.plugin.id, "org.kanon.plugin.weather");
    assert_eq!(manifest.commands.len(), 1);
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "fetch_weather");
    assert_eq!(
        manifest.tools[0]
            .parameters
            .as_ref()
            .and_then(|params| params.get("required"))
            .and_then(|required| required.get(0))
            .and_then(|city| city.as_str()),
        Some("city")
    );

    let schema = manifest.config_schema.expect("config schema parsed");
    assert_eq!(
        schema
            .get("properties")
            .and_then(|properties| properties.get("default_city"))
            .and_then(|city| city.get("default"))
            .and_then(|value| value.as_str()),
        Some("Beijing")
    );
    assert_eq!(
        schema
            .get("required")
            .and_then(|r| r.get(0))
            .and_then(|v| v.as_str()),
        Some("api_key")
    );
}

/// A manifest without the optional sections still parses with empty defaults.
#[test]
fn manifest_tolerates_missing_optional_sections() {
    let toml = r#"
[plugin]
id = "org.kanon.plugin.minimal"
name = "Minimal"
version = "0.1.0"
runtime = "rust"
entrypoint = "target/release/minimal"
"#;

    let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");
    assert!(manifest.commands.is_empty());
    assert!(manifest.tools.is_empty());
    assert!(manifest.config_schema.is_none());
}

/// Packages belong to `pyproject.toml` / `package.json`; a leftover `[dependencies]` section
/// must be rejected instead of silently doing nothing.
#[test]
fn manifest_rejects_dependencies_section() {
    let toml = r#"
[plugin]
id = "org.kanon.plugin.legacy"
name = "Legacy"
version = "0.1.0"
runtime = "python"
entrypoint = "main.py"

[dependencies]
packages = ["httpx>=0.25.0"]
"#;

    let error = toml::from_str::<PluginManifest>(toml).expect_err("stale section must fail");
    assert!(error.to_string().contains("dependencies"), "{error}");
}

/// A plugin whose environment is not installed is reported unavailable before any process starts.
#[tokio::test]
async fn missing_plugin_environments_are_runtime_unavailable() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Supervisor::new(Some(run_dir.path().to_path_buf()), None);

    let cases = [
        ("python", "main.py", None, "uv sync"),
        (
            "typescript",
            "index.ts",
            Some(r#"{"dependencies": {"left-pad": "^1.3.0"}}"#),
            "npm install",
        ),
    ];
    for (runtime, entrypoint, package_json, hint) in cases {
        let plugin_dir = run_dir.path().join(runtime);
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                "[plugin]\nid = \"org.kanon.test.{runtime}\"\nname = \"Env\"\nversion = \"0.1.0\"\nruntime = \"{runtime}\"\nentrypoint = \"{entrypoint}\"\n"
            ),
        )
        .expect("manifest");
        if let Some(package_json) = package_json {
            std::fs::write(plugin_dir.join("package.json"), package_json).expect("package.json");
        }

        let error = supervisor
            .spawn_from_manifest(plugin_dir.join("plugin.toml"), None)
            .await
            .expect_err("a plugin without its environment must not start");
        assert!(
            matches!(&error, SupervisorError::RuntimeUnavailable { reason, .. } if reason.contains(hint)),
            "{error}"
        );
    }
    assert_eq!(supervisor.get_unavailable_plugins().await.len(), 2);
}

/// Externally registered hosts cannot be restarted and say so explicitly.
#[tokio::test]
async fn restart_requires_a_recorded_launch_spec() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Supervisor::new(Some(run_dir.path().to_path_buf()), None);

    let host = Arc::new(ManagedHost::new(
        "host_external".to_string(),
        run_dir.path().join("host_external.sock"),
        tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy(),
        Vec::<PluginMeta>::new(),
        500,
    ));
    supervisor.register_managed_host(host).await;

    let error = supervisor
        .restart_host("host_external")
        .await
        .expect_err("restart without a launch recipe must fail");
    assert!(matches!(error, SupervisorError::RestartUnavailable(id) if id == "host_external"));

    let error = supervisor
        .restart_host("host_missing")
        .await
        .expect_err("unknown hosts must fail");
    assert!(matches!(error, SupervisorError::HostNotFound(id) if id == "host_missing"));
}

/// Configuration reloads target the host declaring the plugin, or fail explicitly.
#[tokio::test]
async fn reload_config_reports_missing_plugin_and_invalid_payload() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Supervisor::new(Some(run_dir.path().to_path_buf()), None);

    let plugin = PluginMeta {
        id: "org.kanon.plugin.demo".to_string(),
        name: "Demo".to_string(),
        version: "1.0.0".to_string(),
        ..PluginMeta::default()
    };
    let host = Arc::new(ManagedHost::new(
        "host_demo".to_string(),
        run_dir.path().join("host_demo.sock"),
        tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy(),
        vec![plugin],
        500,
    ));
    supervisor.register_managed_host(host).await;

    let error = supervisor
        .reload_plugin_config("org.kanon.plugin.absent", &serde_json::json!({}))
        .await
        .expect_err("unknown plugins must fail");
    assert!(
        matches!(error, SupervisorError::PluginNotFound(id) if id == "org.kanon.plugin.absent")
    );

    // Non-object payloads cannot be mapped onto `google.protobuf.Struct`.
    let error = supervisor
        .reload_plugin_config("org.kanon.plugin.demo", &serde_json::json!("nope"))
        .await
        .expect_err("scalar payloads must be rejected");
    assert!(matches!(error, SupervisorError::InvalidConfigPayload));

    // A declared plugin on an unreachable host surfaces the IPC failure rather than succeeding.
    let error = supervisor
        .reload_plugin_config("org.kanon.plugin.demo", &serde_json::json!({ "k": "v" }))
        .await
        .expect_err("unreachable hosts must fail the reload");
    assert!(matches!(error, SupervisorError::Rpc(_)));
}

/// Launch recipes are retained for supervisor-spawned hosts and absent otherwise.
#[tokio::test]
async fn managed_host_exposes_launch_spec_and_manifest() {
    let manifest: PluginManifest = toml::from_str(
        r#"
[plugin]
id = "org.kanon.plugin.demo"
name = "Demo"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin/demo"
"#,
    )
    .expect("manifest parses");

    let host = ManagedHost::new(
        "host_demo".to_string(),
        PathBuf::from("/tmp/demo.sock"),
        tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy(),
        vec![PluginMeta {
            id: "org.kanon.plugin.demo".to_string(),
            name: "Demo".to_string(),
            version: "1.0.0".to_string(),
            ..PluginMeta::default()
        }],
        300,
    )
    .with_manifest(manifest)
    .with_launch_spec(LaunchSpec::Direct {
        executable: PathBuf::from("bin/demo"),
        args: vec!["--flag".to_string()],
        priority: 300,
    });

    // Routing decisions follow the metadata reported by the live process.
    assert!(host.declares_plugin("org.kanon.plugin.demo"));
    assert!(!host.declares_plugin("org.kanon.plugin.other"));
    assert!(matches!(
        host.launch_spec(),
        Some(LaunchSpec::Direct { .. })
    ));
    assert_eq!(
        host.manifest().map(|m| m.plugin.runtime.as_str()),
        Some("rust")
    );
}

/// Processing an event without hosts emits pre-filter entry and pass stages in order.
#[tokio::test]
async fn pipeline_engine_emits_pre_filter_stages() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));
    let observer = Arc::new(RecordingObserver::default());

    let engine = PipelineEngine::new(supervisor).with_observer(observer.clone());

    let result = engine
        .process_event(PipelineEventRequest {
            event_id: "evt-1".to_string(),
            platform: "discord".to_string(),
            channel_id: "chan".to_string(),
            sender_id: "user".to_string(),
            raw_text: "hello".to_string(),
            segments: vec![],
            metadata: None,
        })
        .await;

    assert!(matches!(result, PipelineResult::Passed(_)));

    let stages = observer.stages.lock().expect("stage lock");
    let names: Vec<&str> = stages.iter().map(PipelineStage::name).collect();
    assert_eq!(names, vec!["pre_filter_started", "pre_filter_passed"]);
    assert_eq!(stages[0].event_id(), "evt-1");
}

/// The worker loop announces ingested events and queues replies for the dispatcher.
#[tokio::test]
async fn pipeline_worker_emits_ingest_and_outbound_stages() {
    let run_dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.path().to_path_buf()), None));
    let observer = Arc::new(RecordingObserver::default());

    let engine = Arc::new(PipelineEngine::new(supervisor).with_observer(observer.clone()));

    let (event_tx, event_rx) = tokio::sync::mpsc::channel(4);
    event_tx
        .send(IngestEventRequest {
            platform: "discord".to_string(),
            event: Some(PipelineEventRequest {
                event_id: "evt-9".to_string(),
                platform: "discord".to_string(),
                channel_id: "chan".to_string(),
                sender_id: "user".to_string(),
                raw_text: "hello".to_string(),
                segments: vec![],
                metadata: None,
            }),
        })
        .await
        .expect("event queued");
    drop(event_tx);

    // No dispatcher task runs here and no adapter can produce a reply, so only the inbound
    // stages are expected: the pipeline itself must never invent outbound traffic.
    engine.run_worker_loop(event_rx).await;

    let stages = observer.stages.lock().expect("stage lock");
    let names: Vec<&str> = stages.iter().map(PipelineStage::name).collect();
    assert_eq!(
        names,
        vec!["ingested", "pre_filter_started", "pre_filter_passed"]
    );
}

/// Configuration reloads strictly enforce monotonically increasing CAS version vectors.
#[tokio::test]
async fn config_hot_reload_enforces_cas_version_vectors() {
    use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
    use kanon_proto::v1::{
        GetPluginMetaRequest, GetPluginMetaResponse, PingRequest, PingResponse,
        PluginActionRequest, PluginActionResponse, ReloadPluginConfigRequest,
        ReloadPluginConfigResponse,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use tonic::{Request, Response, Status};

    struct TestHostService {
        version: AtomicU64,
    }

    #[tonic::async_trait]
    impl PluginHostService for TestHostService {
        /// This fixture declares no management actions.
        async fn invoke_action(
            &self,
            _req: Request<PluginActionRequest>,
        ) -> Result<Response<PluginActionResponse>, Status> {
            Err(Status::unimplemented("fixture host has no actions"))
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
            let cur = self.version.load(Ordering::SeqCst);
            if r.version > 0 && r.version <= cur {
                return Ok(Response::new(ReloadPluginConfigResponse {
                    success: false,
                    error_message: format!("Stale version: cur={cur}, req={}", r.version),
                    applied_version: cur,
                }));
            }
            self.version.store(r.version, Ordering::SeqCst);
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
            // The advertised tool depends on the applied configuration, like a plugin whose tool
            // only exists once an API key is configured.
            let version = self.version.load(Ordering::SeqCst);
            Ok(Response::new(GetPluginMetaResponse {
                plugins: vec![PluginMeta {
                    id: "org.kanon.plugin.cas_test".to_string(),
                    tools: vec![kanon_proto::v1::ToolMeta {
                        name: format!("tool_v{version}"),
                        ..Default::default()
                    }],
                    ..PluginMeta::default()
                }],
            }))
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let service = TestHostService {
        version: AtomicU64::new(0),
    };
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    let run_dir = tempdir().expect("temp dir");
    let supervisor = Supervisor::new(Some(run_dir.path().to_path_buf()), None);

    let plugin_id = "org.kanon.plugin.cas_test";
    let plugin = PluginMeta {
        id: plugin_id.to_string(),
        name: "CAS Test".to_string(),
        version: "1.0.0".to_string(),
        ..PluginMeta::default()
    };

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();

    let host = Arc::new(ManagedHost::new(
        "host_cas".to_string(),
        run_dir.path().join("host_cas.sock"),
        channel,
        vec![plugin],
        500,
    ));
    supervisor.register_managed_host(host.clone()).await;

    // Initial version is 0
    assert_eq!(supervisor.config_version(plugin_id).await, 0);

    // 1. CAS check: Attempting update with expected_version = 5 must fail with StaleConfigVersion
    let err = supervisor
        .reload_plugin_config_cas(plugin_id, &serde_json::json!({"k": 1}), Some(5))
        .await
        .expect_err("mismatched version must be rejected");
    assert!(matches!(
        err,
        SupervisorError::StaleConfigVersion {
            current_version: 0,
            requested_version: 5,
            ..
        }
    ));

    // 2. Successful reload with expected_version = 0 increments version to 1
    let v1 = supervisor
        .reload_plugin_config_cas(plugin_id, &serde_json::json!({"k": 1}), Some(0))
        .await
        .expect("CAS reload with expected version 0 succeeds");
    assert_eq!(v1, 1);
    assert_eq!(supervisor.config_version(plugin_id).await, 1);

    // 3. Next update with expected_version = 1 increments version to 2
    let v2 = supervisor
        .reload_plugin_config_cas(plugin_id, &serde_json::json!({"k": 2}), Some(1))
        .await
        .expect("CAS reload with expected version 1 succeeds");
    assert_eq!(v2, 2);
    assert_eq!(supervisor.config_version(plugin_id).await, 2);
    // An accepted reload refreshes the metadata, so configuration-dependent tools appear.
    assert_eq!(host.metas()[0].tools[0].name, "tool_v2");

    // 4. Stale update with expected_version = 1 fails (since current is 2)
    let err2 = supervisor
        .reload_plugin_config_cas(plugin_id, &serde_json::json!({"k": 3}), Some(1))
        .await
        .expect_err("stale CAS version 1 must be rejected when current is 2");
    assert!(matches!(
        err2,
        SupervisorError::StaleConfigVersion {
            current_version: 2,
            requested_version: 1,
            ..
        }
    ));

    // Version remains 2
    assert_eq!(supervisor.config_version(plugin_id).await, 2);
}
