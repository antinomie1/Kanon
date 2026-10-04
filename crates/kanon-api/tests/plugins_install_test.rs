//! Tests for POST /api/v1/plugins/install route (path import and archive upload).

mod common;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use kanon_api::app;
use serde_json::json;

use common::{error_code, fixture_state, send_json};

#[tokio::test]
async fn install_rejects_missing_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": "/path/does/not/exist/999" })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
}

#[tokio::test]
async fn install_rejects_directory_without_manifest() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let empty_plugin = dir.path().join("empty_plugin");
    fs::create_dir_all(&empty_plugin).expect("create empty");

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": empty_plugin.to_string_lossy().to_string() })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
}

#[tokio::test]
async fn install_rejects_invalid_manifest_content() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let broken_plugin = dir.path().join("broken_plugin");
    fs::create_dir_all(&broken_plugin).expect("create broken");
    fs::write(broken_plugin.join("plugin.toml"), "invalid toml syntax [[[").expect("write broken");

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": broken_plugin.to_string_lossy().to_string() })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
}

#[tokio::test]
async fn install_rejects_directory_traversal_plugin_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let bad_id_plugin = dir.path().join("bad_id_plugin");
    fs::create_dir_all(&bad_id_plugin).expect("create bad id");
    fs::write(
        bad_id_plugin.join("plugin.toml"),
        r#"
[plugin]
id = "../escaped_id"
name = "Malicious"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin"
"#,
    )
    .expect("write bad id");

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": bad_id_plugin.to_string_lossy().to_string() })),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
}

#[tokio::test]
async fn install_from_path_succeeds_with_runtime_unavailable_graceful_degradation() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    let py_plugin = dir.path().join("demo_py");
    fs::create_dir_all(&py_plugin).expect("create py plugin");
    fs::write(
        py_plugin.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.installed_py"
name = "Installed Python Plugin"
version = "1.2.0"
runtime = "python"
entrypoint = "main.py"

[[commands]]
name = "pyhello"
description = "Say hello from python"

[[tools]]
name = "pycalc"
description = "Calculate with python"
parameters = { type = "object" }
"#,
    )
    .expect("write plugin.toml");

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": py_plugin.to_string_lossy().to_string() })),
    )
    .await;

    // The copied plugin has no `.venv` yet: Kanon never installs dependencies itself, so the
    // plugin is installed but unavailable until the operator runs `uv sync` in its directory.
    assert_eq!(status, 200);
    assert_eq!(body["plugin_id"], "org.kanon.test.installed_py");
    assert_eq!(body["name"], "Installed Python Plugin");
    assert_eq!(body["version"], "1.2.0");
    assert_eq!(body["runtime"], "python");
    assert_eq!(body["commands"][0]["name"], "pyhello");
    assert_eq!(body["tools"][0]["name"], "pycalc");
    assert_eq!(body["status"], "RuntimeUnavailable");
    assert!(body["message"].as_str().unwrap().contains("uv sync"));
}

#[tokio::test]
async fn install_from_zip_archive_multipart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    // Create a memory zip archive
    let mut zip_buffer = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_buffer);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();

        zip.start_file("plugin.toml", options).expect("start file");
        zip.write_all(
            br#"
[plugin]
id = "org.kanon.test.zip_plugin"
name = "ZIP Package Plugin"
version = "2.0.0"
runtime = "typescript"
entrypoint = "dist/index.js"

[[commands]]
name = "tsgreet"
description = "Greet from ts"
"#,
        )
        .expect("write manifest");

        zip.finish().expect("finish zip");
    }

    // Build multipart request
    let boundary = "------------------------boundary123456789";
    let mut body_bytes = Vec::new();
    body_bytes.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body_bytes.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"plugin.kpk\"\r\n",
    );
    body_bytes.extend_from_slice(b"Content-Type: application/zip\r\n\r\n");
    body_bytes.extend_from_slice(&zip_buffer);
    body_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = axum::http::Request::builder()
        .header("host", "localhost")
        .method(Method::POST)
        .uri("/api/v1/plugins/install")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body_bytes))
        .expect("build request");

    let response = tower::ServiceExt::oneshot(app, req)
        .await
        .expect("execute request");

    assert_eq!(response.status(), 200);

    let resp_bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&resp_bytes).expect("parse json");

    assert_eq!(body["plugin_id"], "org.kanon.test.zip_plugin");
    assert_eq!(body["name"], "ZIP Package Plugin");
    assert_eq!(body["version"], "2.0.0");
    assert_eq!(body["runtime"], "typescript");
    assert_eq!(body["commands"][0]["name"], "tsgreet");
    let status_str = body["status"].as_str().unwrap();
    assert!(status_str == "running" || status_str == "RuntimeUnavailable");
}

/// Sends a `.kpk` to the install endpoint as a multipart upload.
async fn upload_package(app: Router, package: Vec<u8>) -> axum::http::StatusCode {
    let boundary = "------------------------boundary123456789";
    let mut body_bytes = Vec::new();
    body_bytes.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body_bytes.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"plugin.kpk\"\r\n",
    );
    body_bytes.extend_from_slice(b"Content-Type: application/zip\r\n\r\n");
    body_bytes.extend_from_slice(&package);
    body_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = axum::http::Request::builder()
        .header("host", "localhost")
        .method(Method::POST)
        .uri("/api/v1/plugins/install")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body_bytes))
        .expect("build request");
    tower::ServiceExt::oneshot(app, req)
        .await
        .expect("execute request")
        .status()
}

#[tokio::test]
async fn install_keeps_a_packaged_binary_under_target() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    // What `kanon-dev pack` makes of a Rust plugin: the manifest and the binary at its entrypoint.
    let mut package = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut package));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("plugin.toml", options).unwrap();
        zip.write_all(
            br#"[plugin]
id = "org.kanon.test.packaged_bin"
name = "Packaged Binary"
version = "1.0.0"
runtime = "rust"
entrypoint = "target/debug/packaged_bin"
"#,
        )
        .unwrap();
        zip.start_file("target/debug/packaged_bin", options.unix_permissions(0o755))
            .unwrap();
        zip.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        zip.finish().unwrap();
    }

    // The fake binary exits at once, so the host may fail to start; the files must be in place
    // either way.
    upload_package(app, package).await;

    let installed = dir
        .path()
        .join("plugins/org.kanon.test.packaged_bin/target/debug/packaged_bin");
    assert!(
        installed.is_file(),
        "the entrypoint under target/ was dropped"
    );
}

#[test]
fn cancelled_preparation_keeps_its_directory_until_the_copy_worker_finishes() {
    use std::future::Future;
    use std::task::Poll;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let state = fixture_state(dir.path().to_path_buf(), false).await;
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("plugin.toml"),
            "[plugin]\nid = 'org.kanon.test.cancelled_copy'\nname = 'Copy'\n\
             version = '1.0.0'\nruntime = 'python'\nentrypoint = 'main.py'\n",
        )
        .unwrap();
        fs::write(source.join("main.py"), "print('copied')\n").unwrap();

        // Hold the only blocking worker so the copy is queued at a precise cancellation point.
        // Channel ownership also releases the worker during unwinding if an assertion fails.
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            let _ = waiting.recv();
        });
        started.await.unwrap();
        let mut installing = Box::pin(kanon_api::plugin_install::install(
            &state,
            kanon_api::plugin_install::InstallSource::Path(source),
            false,
        ));
        std::future::poll_fn(|cx| {
            assert!(installing.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let staging = fs::read_dir(state.plugins_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".kanon-install-")
            })
            .expect("copy has reserved its staging directory");
        drop(installing);
        assert!(
            staging.is_dir(),
            "the queued worker must retain its directory"
        );

        release.send(()).unwrap();
        blocker.await.unwrap();
        // With one blocking thread, this sentinel completes only after the cancelled copy's
        // worker has run and dropped its unclaimed result, including the directory guard.
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert!(
            !staging.exists(),
            "cancelled preparation must clean up after copying"
        );
        assert!(
            !state
                .plugins_dir()
                .join("org.kanon.test.cancelled_copy")
                .exists()
        );
    });
}

#[cfg(unix)]
#[test]
fn cancelled_upgrade_finishes_and_failed_swap_restores_the_previous_host() {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::Poll;
    use std::time::Duration;

    use kanon_core::{LaunchSpec, ManagedHost, PluginManifest};
    use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
    use kanon_proto::v1::*;
    use tokio::sync::{Notify, Semaphore};
    use tonic::{Request, Response, Status};

    struct ControlledHandshake {
        entered: Arc<Notify>,
        release: Arc<Semaphore>,
    }

    #[tonic::async_trait]
    impl PluginHostService for ControlledHandshake {
        async fn get_plugin_meta(
            &self,
            _: Request<GetPluginMetaRequest>,
        ) -> Result<Response<GetPluginMetaResponse>, Status> {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            let mut meta = common::fixture_meta();
            meta.version = "2.0.0".into();
            Ok(Response::new(GetPluginMetaResponse {
                plugins: vec![meta],
            }))
        }

        async fn ping(
            &self,
            request: Request<PingRequest>,
        ) -> Result<Response<PingResponse>, Status> {
            Ok(Response::new(PingResponse {
                timestamp: request.into_inner().timestamp,
            }))
        }

        async fn reload_plugin_config(
            &self,
            _: Request<ReloadPluginConfigRequest>,
        ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
            Err(Status::unimplemented("no configuration"))
        }

        async fn invoke_action(
            &self,
            _: Request<PluginActionRequest>,
        ) -> Result<Response<PluginActionResponse>, Status> {
            Err(Status::unimplemented("no actions"))
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let state = fixture_state(dir.path().to_path_buf(), false).await;
        let installed = state.plugins_dir().join(common::FIXTURE_PLUGIN_ID);
        fs::create_dir_all(&installed).unwrap();
        let manifest = format!(
            "[plugin]\nid = '{}'\nname = 'Upgrade'\nversion = '1.0.0'\n\
             runtime = 'rust'\nentrypoint = 'host.sh'\n",
            common::FIXTURE_PLUGIN_ID
        );
        let manifest_path = installed.join("plugin.toml");
        fs::write(&manifest_path, &manifest).unwrap();
        fs::write(installed.join("old-only"), "old version").unwrap();
        state.rescan_plugins().unwrap();
        state
            .supervisor()
            .register_managed_host(Arc::new(
                ManagedHost::new(
                    common::FIXTURE_HOST_ID.into(),
                    state.supervisor().run_dir().join("old.sock"),
                    common::dead_channel(),
                    vec![common::fixture_meta()],
                    100,
                )
                .with_manifest(PluginManifest::load_from_file(&manifest_path).unwrap())
                .with_launch_spec(LaunchSpec::Manifest {
                    manifest_path,
                    executable_override: None,
                    priority: 100,
                }),
            ))
            .await;
        let source = dir.path().join("new-version");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("plugin.toml"),
            manifest.replace("1.0.0", "2.0.0"),
        )
        .unwrap();
        // The in-process fixture serves IPC while this real child gives the supervisor a lifetime
        // to manage. No compiler or language runtime is needed for the handshake regression.
        fs::write(source.join("host.sh"), "#!/bin/sh\nexec sleep 30\n").unwrap();
        let listener = kanon_transport::IpcListener::bind(kanon_transport::host_socket_path(
            common::FIXTURE_HOST_ID,
            Some(state.supervisor().run_dir()),
        ))
        .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let service = ControlledHandshake {
            entered: entered.clone(),
            release: release.clone(),
        };
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(PluginHostServiceServer::new(service))
                .serve_with_incoming(listener.incoming())
                .await
                .unwrap();
        });
        let router = app(state.clone());
        let request = tokio::spawn(async move {
            send_json(
                &router,
                Method::POST,
                "/api/v1/plugins/install",
                Some(json!({
                    "path": source, "replace": true
                })),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        assert!(!installed.join("old-only").exists());
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());

        let mut transaction = Box::pin(
            state
                .supervisor()
                .lock_plugin_config(common::FIXTURE_PLUGIN_ID),
        );
        std::future::poll_fn(|cx| {
            assert!(
                transaction.as_mut().poll(cx).is_pending(),
                "cancelled HTTP must not release the commit lock"
            );
            Poll::Ready(())
        })
        .await;
        release.add_permits(1);
        drop(
            tokio::time::timeout(Duration::from_secs(2), transaction)
                .await
                .unwrap(),
        );
        let host = state
            .supervisor()
            .find_host_for_plugin(common::FIXTURE_PLUGIN_ID)
            .await
            .unwrap();
        assert_eq!(host.metas()[0].version, "2.0.0");
        assert_eq!(
            PluginManifest::load_from_file(installed.join("plugin.toml"))
                .unwrap()
                .plugin
                .version,
            "2.0.0"
        );
        // Retry an upgrade after its copy completed but its staging directory was removed by an
        // external actor. Both the successful rollback restart and a failed restart must report
        // the original installation honestly, without publishing the rejected version.
        let retry_source = dir.path().join("retry-version");
        fs::create_dir(&retry_source).unwrap();
        fs::write(
            retry_source.join("plugin.toml"),
            manifest.replace("1.0.0", "3.0.0"),
        )
        .unwrap();
        fs::write(retry_source.join("host.sh"), "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::write(installed.join("retained"), "version two").unwrap();
        for can_restart in [true, false] {
            if !can_restart {
                fs::remove_file(installed.join("host.sh")).unwrap();
            }
            let (started, entered_worker) = tokio::sync::oneshot::channel();
            let (unblock, waiting) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = waiting.recv();
            });
            entered_worker.await.unwrap();
            let mut installing = Box::pin(kanon_api::plugin_install::install(
                &state,
                kanon_api::plugin_install::InstallSource::Path(retry_source.clone()),
                true,
            ));
            std::future::poll_fn(|cx| {
                assert!(installing.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let staging = fs::read_dir(state.plugins_dir())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".kanon-install-")
                })
                .unwrap();
            unblock.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            // The install future has not resumed since copying, so this reliably fails the
            // second swap rename after the previous directory has been moved aside.
            fs::remove_dir_all(staging).unwrap();
            if can_restart {
                release.add_permits(1);
            }
            let error = tokio::time::timeout(Duration::from_secs(2), installing)
                .await
                .unwrap()
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("the previous version was restored"),
                "{error}"
            );
            assert!(
                error.contains(if can_restart {
                    "the previous host was restarted"
                } else {
                    "restarting the previous host also failed"
                }),
                "{error}"
            );
            assert_eq!(
                state
                    .supervisor()
                    .find_host_for_plugin(common::FIXTURE_PLUGIN_ID)
                    .await
                    .is_some(),
                can_restart
            );
            assert_eq!(
                PluginManifest::load_from_file(installed.join("plugin.toml"))
                    .unwrap()
                    .plugin
                    .version,
                "2.0.0"
            );
            assert_eq!(
                fs::read_to_string(installed.join("retained")).unwrap(),
                "version two"
            );
            assert!(fs::read_dir(state.plugins_dir()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with('.')
            }));
        }
        state.supervisor().stop_all().await.unwrap();
        server.abort();
        let _ = server.await;
    });
}

#[tokio::test]
async fn install_demo_weather_plugin_end_to_end() {
    let dir = tempfile::tempdir().expect("temp dir");
    let app: Router = app(fixture_state(PathBuf::from(dir.path()), false).await);

    // Resolve relative path to ./plugins/demo_weather from workspace
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join("demo_weather");

    if !manifest_path.exists() {
        return;
    }

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({ "path": manifest_path.to_string_lossy().to_string() })),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["plugin_id"], "org.kanon.plugin.weather");
    assert_eq!(body["name"], "Demo Weather Plugin");
    assert_eq!(body["version"], "0.1.0");
    assert_eq!(body["runtime"], "rust");
    assert_eq!(body["commands"][0]["name"], "weather");
    assert_eq!(body["tools"][0]["name"], "fetch_weather");
    // Verify that the supervisor dynamically spawned it or cleanly handled it
    let status_str = body["status"].as_str().unwrap();
    assert!(status_str == "running" || status_str == "RuntimeUnavailable");
}
