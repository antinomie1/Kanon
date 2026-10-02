//! Plugin market: index parsing and `GET /api/v1/plugins/market`.

mod common;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::http::{Method, StatusCode};
use axum::routing::get;
use kanon_api::plugin_market::parse_index;
use kanon_api::{ApiState, SystemConfigStore, app};
use kanon_core::supervisor::Supervisor;
use serde_json::{Value, json};

use common::{FIXTURE_PLUGIN_ID, register_fixture_host, send_json};

#[test]
fn index_keeps_valid_entries_and_reports_the_rest() {
    let index = json!({
        "name": "  Test market  ",
        "plugins": [
            {
                "id": "org.example.weather",
                "name": "Weather",
                "version": "1.2.0",
                "download_url": "https://example.org/weather.kpk",
                "kanon_version": ">=0.1",
                "platforms": ["qq"],
                "homepage": "https://example.org",
                "future_field": "ignored"
            },
            { "id": "org.example.git", "name": "Git", "version": "1.0.0",
              "repository": "https://example.org/git.git" },
            { "id": "../escape", "name": "Bad id", "version": "1.0.0",
              "download_url": "https://example.org/x.kpk" },
            { "id": "org.example.nowhere", "name": "No source", "version": "1.0.0" },
            { "id": "org.example.plain", "name": "Plain http", "version": "1.0.0",
              "download_url": "http://example.org/x.kpk" },
            { "id": "org.example.req", "name": "Bad requirement", "version": "1.0.0",
              "download_url": "https://example.org/x.kpk", "kanon_version": "soon" },
            { "id": "org.example.weather", "name": "Duplicate", "version": "9.0.0",
              "download_url": "https://example.org/dup.kpk" },
            { "name": "No id" }
        ]
    });

    let parsed = parse_index(index.to_string().as_bytes()).expect("an index");

    assert_eq!(parsed.name.as_deref(), Some("Test market"));
    let ids: Vec<&str> = parsed
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["org.example.weather", "org.example.git"]);
    assert_eq!(parsed.entries[0].version, "1.2.0");
    assert_eq!(parsed.entries[0].platforms, ["qq"]);
    // Every skipped entry is explained, none silently dropped.
    assert_eq!(parsed.warnings.len(), 6, "{:?}", parsed.warnings);
    assert!(parsed.warnings.iter().any(|w| w.contains("more than once")));
    assert!(parsed.warnings.iter().any(|w| w.contains("neither")));
}

#[test]
fn a_document_that_is_not_an_index_is_an_error() {
    assert!(parse_index(b"<html>not json</html>").is_err());
    assert!(parse_index(br#"{"name": "no plugins array"}"#).is_err());
    assert!(parse_index(br#"{"plugins": {}}"#).is_err());
}

/// Builds gateway state whose `system.json` is `system` (or absent).
async fn market_state(dir: &Path, system: Option<Value>) -> ApiState {
    let system_path = dir.join("system.json");
    if let Some(system) = system {
        std::fs::write(&system_path, system.to_string()).unwrap();
    }
    let run_dir = dir.join("run");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir), None));
    register_fixture_host(&supervisor).await;
    ApiState::builder(supervisor)
        .with_config_dir(dir.to_path_buf())
        .with_plugins_dir(dir.join("plugins"))
        .with_system_config(Arc::new(SystemConfigStore::new(system_path)))
        .build()
}

/// Serves `index` and a non-index document on a loopback port; returns the base URL.
async fn index_server(index: Value) -> String {
    let body = index.to_string();
    let router = Router::new()
        .route("/index.json", get(move || async move { body.clone() }))
        .route("/broken.json", get(|| async { "not an index" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn market_without_indexes_answers_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(market_state(dir.path(), None).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/plugins/market", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], false);
    assert!(body["hint"].as_str().unwrap().contains("plugin_market"));
    assert_eq!(body["plugins"], json!([]));
}

#[tokio::test]
async fn market_merges_sources_and_annotates_entries_for_this_node() {
    let dir = tempfile::tempdir().unwrap();
    let base = index_server(json!({
        "name": "Local",
        "plugins": [
            { "id": FIXTURE_PLUGIN_ID, "name": "Fixture", "version": "3.0.0",
              "download_url": "https://example.org/fixture.kpk" },
            { "id": "org.example.future", "name": "Future", "version": "1.0.0",
              "download_url": "https://example.org/future.kpk", "kanon_version": ">=99" }
        ]
    }))
    .await;
    let system = json!({
        "plugin_market": {
            "indexes": [
                format!("{base}/index.json"),
                format!("{base}/broken.json"),
                // Same index again: its entries are reported as shadowed, not listed twice.
                format!("{base}/index.json"),
            ]
        }
    });
    let app = app(market_state(dir.path(), Some(system)).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/plugins/market", None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], true);

    let sources = body["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0]["name"], "Local");
    assert_eq!(sources[0]["plugins"], 2);
    assert!(sources[0].get("error").is_none());
    // One broken source does not empty the market.
    assert!(
        sources[1]["error"]
            .as_str()
            .unwrap()
            .contains("not a plugin market index")
    );
    assert_eq!(sources[2]["plugins"], 0);
    assert_eq!(sources[2]["warnings"].as_array().unwrap().len(), 2);

    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 2);
    let fixture = plugins
        .iter()
        .find(|p| p["id"] == FIXTURE_PLUGIN_ID)
        .unwrap();
    assert_eq!(fixture["installed_version"], "2.1.0");
    assert_eq!(fixture["compatible"], true);
    assert_eq!(fixture["source"], format!("{base}/index.json"));
    let future = plugins
        .iter()
        .find(|p| p["id"] == "org.example.future")
        .unwrap();
    assert_eq!(future["compatible"], false);
    assert!(
        future["incompatible_reason"]
            .as_str()
            .unwrap()
            .contains("requires Kanon >=99")
    );
    assert!(future.get("installed_version").is_none());
}

#[tokio::test]
async fn unreachable_and_insecure_sources_are_reported_per_source() {
    let dir = tempfile::tempdir().unwrap();
    let system = json!({
        "plugin_market": {
            "indexes": ["http://127.0.0.1:1/index.json", "http://example.com/index.json"]
        }
    });
    let app = app(market_state(dir.path(), Some(system)).await);

    let (status, body) = send_json(&app, Method::GET, "/api/v1/plugins/market", None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let sources = body["sources"].as_array().unwrap();
    assert!(
        sources[0]["error"]
            .as_str()
            .unwrap()
            .contains("download failed")
    );
    assert!(sources[1]["error"].as_str().unwrap().contains("only https"));
}

#[tokio::test]
async fn misspelt_market_settings_fail_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let system = json!({ "plugin_market": { "index": ["https://example.org/index.json"] } });
    let app = app(market_state(dir.path(), Some(system)).await);

    let (status, _) = send_json(&app, Method::GET, "/api/v1/plugins/market", None).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
