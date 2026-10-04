//! Tests for the persisted plugin enable/disable state.

use kanon_core::{MCP_SECTION, PLUGIN_SECTION, SKILL_SECTION, ToggleStore};

#[tokio::test]
async fn plugins_are_enabled_until_an_operator_says_otherwise() {
    let store = ToggleStore::in_memory();

    // Absence means enabled: a freshly cloned plugin runs without an opt-in.
    assert!(
        store
            .is_enabled(PLUGIN_SECTION, "org.kanon.plugin.fresh")
            .await
    );
    assert!(store.disabled_ids(PLUGIN_SECTION).await.is_empty());

    assert!(
        store
            .set_enabled(PLUGIN_SECTION, "org.kanon.plugin.fresh", false)
            .await
            .expect("disable")
    );
    assert!(
        !store
            .is_enabled(PLUGIN_SECTION, "org.kanon.plugin.fresh")
            .await
    );
    assert_eq!(
        store.disabled_ids(PLUGIN_SECTION).await,
        vec!["org.kanon.plugin.fresh".to_string()]
    );

    // Idempotence is reported so callers can skip stopping a host twice.
    assert!(
        !store
            .set_enabled(PLUGIN_SECTION, "org.kanon.plugin.fresh", false)
            .await
            .expect("second disable")
    );
}

#[tokio::test]
async fn state_survives_a_restart_and_keeps_unrelated_keys() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("toggles.json");

    // A document that already carries another component's setting.
    std::fs::write(
        &path,
        r#"{"version":1,"plugins":{},"future_setting":{"keep":true}}"#,
    )
    .expect("seed document");

    let store = ToggleStore::open(&path).await.expect("open store");
    store
        .set_enabled(PLUGIN_SECTION, "org.kanon.plugin.weather", false)
        .await
        .expect("disable");

    let reopened = ToggleStore::open(&path).await.expect("reopen store");
    assert!(
        !reopened
            .is_enabled(PLUGIN_SECTION, "org.kanon.plugin.weather")
            .await
    );
    assert!(
        reopened
            .is_enabled(SKILL_SECTION, "org.kanon.plugin.other")
            .await
    );

    let raw = std::fs::read_to_string(&path).expect("read document");
    assert!(
        raw.contains("future_setting"),
        "writing the plugin state must not discard sibling settings: {raw}"
    );
}

#[tokio::test]
async fn re_enabling_removes_the_entry_instead_of_storing_a_redundant_true() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("toggles.json");
    let store = ToggleStore::open(&path).await.expect("open store");

    store
        .set_enabled(MCP_SECTION, "files", false)
        .await
        .expect("disable");
    store
        .set_enabled(MCP_SECTION, "files", true)
        .await
        .expect("enable");

    // Absence is the canonical representation of "enabled", so the file must not keep a key that
    // would otherwise accumulate for every server ever toggled.
    let raw = std::fs::read_to_string(&path).expect("read document");
    assert!(!raw.contains("files"), "{raw}");
    assert!(store.disabled_ids(MCP_SECTION).await.is_empty());
}

#[tokio::test]
async fn a_malformed_document_is_reported_instead_of_ignored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("toggles.json");
    std::fs::write(&path, "{ not json").expect("seed malformed document");

    // Silently enabling a plugin the operator disabled would be the worst possible recovery.
    let err = ToggleStore::open(&path)
        .await
        .expect_err("malformed state must be rejected");
    assert!(err.contains("failed to parse"), "unexpected error: {err}");
}

#[tokio::test]
async fn an_in_memory_store_never_touches_the_filesystem() {
    let store = ToggleStore::in_memory();
    assert!(store.path().is_none());

    store
        .set_enabled(PLUGIN_SECTION, "org.kanon.plugin.x", false)
        .await
        .expect("disable without a path");
    assert!(!store.is_enabled(PLUGIN_SECTION, "org.kanon.plugin.x").await);
}

#[tokio::test]
async fn unsupported_versions_are_neither_loaded_nor_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("toggles.json");
    let store = ToggleStore::open(&path).await.unwrap();
    let future = r#"{"version":2,"plugins":{"plugin":false}}"#;
    std::fs::write(&path, future).unwrap();
    assert!(
        ToggleStore::open(&path)
            .await
            .unwrap_err()
            .contains("schema version 2")
    );
    assert!(
        store
            .set_enabled(PLUGIN_SECTION, "other", false)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), future);
    assert!(store.is_enabled(PLUGIN_SECTION, "other").await);
}
