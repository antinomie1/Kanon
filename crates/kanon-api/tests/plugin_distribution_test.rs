//! Plugin distribution: installing from a package URL and from Git, plus the install-time checks
//! (manifest, `kanon_version`, id collisions, explicit replace) every source goes through.
//!
//! Nothing here touches the internet: packages come from a server bound to 127.0.0.1 and
//! repositories from `file://` URLs.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::Command;

use axum::Router;
use axum::http::{Method, StatusCode};
use axum::response::Redirect;
use axum::routing::get;
use kanon_api::app;
use serde_json::{Value, json};

use common::{FIXTURE_PLUGIN_ID, error_code, fixture_state, send_json};

/// Manifest of a Python plugin: it installs without a runtime and reports `RuntimeUnavailable`,
/// which keeps these tests independent of uv and of built binaries.
fn manifest(id: &str, version: &str, extra: &str) -> String {
    format!(
        "[plugin]\nid = \"{id}\"\nname = \"Packaged\"\nversion = \"{version}\"\n\
         runtime = \"python\"\nentrypoint = \"main.py\"\n{extra}\n"
    )
}

/// Builds a ZIP package from `(name, content)` entries.
fn package(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer
}

/// Serves packages on a loopback port and returns its base URL.
async fn package_server(package: Vec<u8>) -> String {
    let router = Router::new()
        .route("/plugin.kpk", get(move || async move { package.clone() }))
        // A redirect from loopback to plain http elsewhere must not be followed.
        .route(
            "/downgrade.kpk",
            get(|| async { Redirect::temporary("http://example.invalid/plugin.kpk") }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

/// Posts an install request.
async fn install(app: &Router, body: Value) -> (StatusCode, Value) {
    send_json(app, Method::POST, "/api/v1/plugins/install", Some(body)).await
}

#[tokio::test]
async fn install_from_url_downloads_and_installs_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugins_dir = state.plugins_dir().to_path_buf();
    let app = app(state);
    let base = package_server(package(&[(
        "plugin.toml",
        &manifest("org.kanon.test.url", "1.0.0", ""),
    )]))
    .await;

    let (status, body) = install(&app, json!({ "url": format!("{base}/plugin.kpk") })).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin_id"], "org.kanon.test.url");
    assert_eq!(body["status"], "RuntimeUnavailable");
    assert!(plugins_dir.join("org.kanon.test.url/plugin.toml").is_file());
}

#[tokio::test]
async fn install_from_url_refuses_insecure_and_failing_sources() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_state(dir.path().to_path_buf(), false).await);
    let base = package_server(package(&[])).await;

    let cases = [
        // Plain http is only accepted for loopback hosts; refused before any connection.
        (
            "http://example.com/plugin.kpk".to_string(),
            400,
            "bad_request",
        ),
        (
            "ftp://example.com/plugin.kpk".to_string(),
            400,
            "bad_request",
        ),
        (
            "https://user:secret@example.com/plugin.kpk".to_string(),
            400,
            "bad_request",
        ),
        // The server answered, but not with a package.
        (format!("{base}/missing.kpk"), 502, "upstream_error"),
        (format!("{base}/downgrade.kpk"), 502, "upstream_error"),
    ];
    for (url, status, code) in cases {
        let (actual, body) = install(&app, json!({ "url": url })).await;
        assert_eq!(actual.as_u16(), status, "{url}: {body}");
        assert_eq!(error_code(&body), code, "{url}");
    }
}

/// Runs git in `dir`, panicking with its output on failure.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Kanon Test",
            "-c",
            "user.email=test@kanon.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether a `git` binary is available; the clone test is skipped (with a note) when not.
fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

#[tokio::test]
async fn install_from_git_clones_the_requested_ref() {
    if !git_available() {
        eprintln!("git is not installed; skipping the clone test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("plugin.toml"),
        manifest("org.kanon.test.git", "1.0.0", ""),
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v1"]);
    git(&repo, &["tag", "v1"]);
    std::fs::write(
        repo.join("plugin.toml"),
        manifest("org.kanon.test.git", "2.0.0", ""),
    )
    .unwrap();
    git(&repo, &["commit", "-q", "-am", "v2"]);

    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugins_dir = state.plugins_dir().to_path_buf();
    let app = app(state);
    let url = format!("file://{}", repo.display());

    let (status, body) = install(&app, json!({ "git": url, "ref": "v1" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin_id"], "org.kanon.test.git");
    assert_eq!(body["version"], "1.0.0");
    // The checkout's `.git` folder is environment, not plugin, and is not installed.
    assert!(!plugins_dir.join("org.kanon.test.git/.git").exists());

    let (status, body) = install(&app, json!({ "git": url, "replace": true })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "2.0.0");

    let (status, body) = install(
        &app,
        json!({ "git": url, "ref": "no-such-branch", "replace": true }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
}

#[tokio::test]
async fn install_from_git_refuses_unsafe_urls_and_refs() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_state(dir.path().to_path_buf(), false).await);

    // Each of these would make git run a command or read an option; all are refused before git
    // is started.
    for (git_url, git_ref) in [
        ("ext::sh -c touch% /tmp/kanon-pwned", None),
        ("--upload-pack=touch /tmp/kanon-pwned", None),
        ("git://example.com/plugin.git", None),
        ("http://example.com/plugin.git", None),
        ("https://example.com/plugin.git", Some("--upload-pack=x")),
    ] {
        let mut request = json!({ "git": git_url });
        if let Some(git_ref) = git_ref {
            request["ref"] = json!(git_ref);
        }
        let (status, body) = install(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{git_url}: {body}");
    }
}

#[tokio::test]
async fn reinstalling_requires_replace_and_swaps_the_whole_folder() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let installed = state.plugins_dir().join("org.kanon.test.upgrade");
    let app = app(state);

    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        manifest("org.kanon.test.upgrade", "1.0.0", ""),
    )
    .unwrap();
    std::fs::write(source.join("old.txt"), "v1 only").unwrap();
    let path = source.to_string_lossy().to_string();
    let (status, body) = install(&app, json!({ "path": path })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    std::fs::remove_file(source.join("old.txt")).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        manifest("org.kanon.test.upgrade", "2.0.0", ""),
    )
    .unwrap();

    let (status, body) = install(&app, json!({ "path": path })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("replace")
    );
    assert!(
        installed.join("old.txt").is_file(),
        "a refused install changes nothing"
    );

    let (status, body) = install(&app, json!({ "path": path, "replace": true })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "2.0.0");
    assert!(
        !installed.join("old.txt").exists(),
        "files of the old version are gone"
    );
    // No staging or parking folder is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(installed.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".kanon-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn a_hand_copied_plugin_keeps_its_folder_when_reinstalled_or_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    // Copied by hand under a folder name that is not the plugin id.
    let installed = state.plugins_dir().join("weather");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("plugin.toml"),
        manifest("org.kanon.test.hand", "1.0.0", ""),
    )
    .unwrap();
    state.rescan_plugins().unwrap();
    let by_id = state.plugins_dir().join("org.kanon.test.hand");
    let app = app(state);

    // Installing from the folder it already lives in registers it where it is.
    let (status, body) = install(&app, json!({ "path": installed.to_string_lossy() })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!by_id.exists(), "no second copy is made");

    // A new version replaces that folder rather than landing next to it.
    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        manifest("org.kanon.test.hand", "2.0.0", ""),
    )
    .unwrap();
    let (status, body) = install(
        &app,
        json!({ "path": source.to_string_lossy(), "replace": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!by_id.exists());
    assert!(
        std::fs::read_to_string(installed.join("plugin.toml"))
            .unwrap()
            .contains("2.0.0")
    );
}

#[tokio::test]
async fn install_refuses_an_id_another_host_already_serves() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugins_dir = state.plugins_dir().to_path_buf();
    let app = app(state);
    let source = dir.path().join("impostor");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        manifest(FIXTURE_PLUGIN_ID, "9.0.0", ""),
    )
    .unwrap();

    let (status, body) = install(
        &app,
        json!({ "path": source.to_string_lossy(), "replace": true }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(!plugins_dir.join(FIXTURE_PLUGIN_ID).exists());
}

#[tokio::test]
async fn install_checks_the_manifest_before_copying() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugins_dir = state.plugins_dir().to_path_buf();
    let app = app(state);

    let cases = [
        ("kanon_version = \">=99\"", "requires Kanon >=99"),
        (
            "kanon_version = \"not a requirement\"",
            "has an invalid kanon_version",
        ),
    ];
    for (index, (extra, expected)) in cases.into_iter().enumerate() {
        let source = dir.path().join(format!("source{index}"));
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("plugin.toml"),
            manifest("org.kanon.test.checked", "1.0.0", extra),
        )
        .unwrap();
        let (status, body) = install(&app, json!({ "path": source.to_string_lossy() })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(expected), "{message}");
    }

    let source = dir.path().join("escape");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        manifest("org.kanon.test.checked", "1.0.0", "").replace("main.py", "../../bin/sh"),
    )
    .unwrap();
    let (status, body) = install(&app, json!({ "path": source.to_string_lossy() })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("entrypoint")
    );

    assert!(!plugins_dir.join("org.kanon.test.checked").exists());
}

#[tokio::test]
async fn install_refuses_package_entries_outside_the_plugin_folder() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugins_dir = state.plugins_dir().to_path_buf();
    let app = app(state);
    let base = package_server(package(&[
        ("plugin.toml", &manifest("org.kanon.test.slip", "1.0.0", "")),
        ("../../escaped.txt", "zip slip"),
    ]))
    .await;

    let (status, body) = install(&app, json!({ "url": format!("{base}/plugin.kpk") })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside the plugin folder")
    );
    assert!(!plugins_dir.join("org.kanon.test.slip").exists());
}

#[tokio::test]
async fn install_request_names_exactly_one_source() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_state(dir.path().to_path_buf(), false).await);

    for request in [
        json!({}),
        json!({ "path": "/a", "url": "https://example.com/a.kpk" }),
        json!({ "url": "https://example.com/a.kpk", "ref": "main" }),
        // A misspelt field is an error, not an install without it.
        json!({ "path": "/a", "replcae": true }),
    ] {
        let (status, body) = install(&app, request.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{request}: {body}");
    }
}
