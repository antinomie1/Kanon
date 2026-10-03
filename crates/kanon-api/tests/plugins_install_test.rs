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
