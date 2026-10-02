//! Plugin translations (`i18n/<locale>.json`) and the plugin details endpoint that carries them.

mod common;

use axum::http::{Method, StatusCode};
use kanon_api::app;
use kanon_api::plugin_files::{load_translations, translation_key_is_known};
use serde_json::json;

use common::{fixture_state, send_json};

#[test]
fn translation_keys_follow_the_documented_shapes() {
    for key in [
        "name",
        "description",
        "config.api_key.title",
        "config.server.port.description",
        "commands.weather.description",
    ] {
        assert!(translation_key_is_known(key), "{key}");
    }
    for key in [
        "title",
        "config.title",
        "config..title",
        "config.api_key.label",
        "commands.weather.usage",
        "commands.a.b.description",
    ] {
        assert!(!translation_key_is_known(key), "{key}");
    }
}

#[test]
fn translations_keep_usable_texts_and_report_every_problem() {
    let dir = tempfile::tempdir().unwrap();
    let i18n = dir.path().join("i18n");
    std::fs::create_dir_all(&i18n).unwrap();
    std::fs::write(
        i18n.join("zh-CN.json"),
        json!({
            "name": "天气",
            "config.city.title": "城市",
            "commands.weather.description": "查询天气",
            "nickname": "unknown key",
            "description": 42
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        i18n.join("en.json"),
        json!({ "name": "Weather" }).to_string(),
    )
    .unwrap();
    std::fs::write(i18n.join("fr.json"), "{ not json").unwrap();
    std::fs::write(i18n.join("x y.json"), "{}").unwrap();
    std::fs::write(i18n.join("README.md"), "not a translation").unwrap();
    std::fs::write(i18n.join(".draft.json"), "{ ignored").unwrap();

    let translations = load_translations(dir.path());

    let locales: Vec<&str> = translations.locales.keys().map(String::as_str).collect();
    assert_eq!(locales, ["en", "zh-CN"]);
    let zh = &translations.locales["zh-CN"];
    assert_eq!(zh["name"], "天气");
    assert_eq!(zh["config.city.title"], "城市");
    assert_eq!(zh["commands.weather.description"], "查询天气");
    assert_eq!(zh.len(), 3);

    let errors = translations.errors.join("\n");
    assert_eq!(translations.errors.len(), 4, "{errors}");
    assert!(errors.contains("i18n/fr.json: is not valid JSON"));
    assert!(errors.contains("'x y' is not a locale tag"));
    assert!(errors.contains("unknown key 'nickname'"));
    assert!(errors.contains("value of 'description' must be a string"));

    assert_eq!(
        load_translations(&dir.path().join("missing")),
        Default::default()
    );
}

#[tokio::test]
async fn plugin_details_carry_links_pages_and_translations() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path().to_path_buf(), false).await;
    let plugin = state.plugins_dir().join("org.kanon.test.i18n");
    std::fs::create_dir_all(plugin.join("i18n")).unwrap();
    std::fs::create_dir_all(plugin.join("pages")).unwrap();
    std::fs::write(
        plugin.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.i18n"
name = "Weather"
version = "1.0.0"
runtime = "python"
entrypoint = "main.py"
kanon_version = ">=0.1"
platforms = ["qq", "onebot"]
homepage = "https://example.org/weather"
repository = "https://example.org/weather.git"
"#,
    )
    .unwrap();
    std::fs::write(plugin.join("pages/index.html"), "<p>hi</p>").unwrap();
    std::fs::write(
        plugin.join("i18n/zh-CN.json"),
        json!({ "name": "天气", "bogus": "x" }).to_string(),
    )
    .unwrap();
    state.rescan_plugins().unwrap();
    let app = app(state);

    let (status, body) = send_json(
        &app,
        Method::GET,
        "/api/v1/plugins/org.kanon.test.i18n",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Weather");
    assert_eq!(body["homepage"], "https://example.org/weather");
    assert_eq!(body["repository"], "https://example.org/weather.git");
    assert_eq!(body["platforms"], json!(["qq", "onebot"]));
    assert_eq!(body["kanon_version"], ">=0.1");
    assert_eq!(body["has_pages"], true);
    assert_eq!(body["serves_http"], false);
    assert_eq!(body["i18n"], json!({ "zh-CN": { "name": "天气" } }));
    assert_eq!(body["i18n_errors"].as_array().unwrap().len(), 1);

    // The catalog carries the same facts, so the console can translate its list.
    let (_, catalog) = send_json(&app, Method::GET, "/api/v1/plugins", None).await;
    let listed = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "org.kanon.test.i18n")
        .unwrap();
    assert_eq!(listed["i18n"]["zh-CN"]["name"], "天气");

    let (status, _) = send_json(
        &app,
        Method::GET,
        "/api/v1/plugins/org.kanon.test.nope",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
