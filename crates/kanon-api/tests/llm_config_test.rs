//! Tests for node system-configuration persistence (`data/system.json`).

use kanon_api::llm_config::resolve_bootstrap;
use kanon_api::{LlmProviderConfig, SystemConfigStore};

fn sample() -> LlmProviderConfig {
    LlmProviderConfig {
        protocol: "openai".to_string(),
        base_url: "https://api.deepseek.com/v1".to_string(),
        model: "deepseek-flash".to_string(),
        api_key: Some("sk-secret".to_string()),
        temperature: Some(0.3),
        max_tokens: Some(2048),
    }
}

#[test]
fn save_load_round_trip_preserves_every_field() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SystemConfigStore::new(dir.path().join("system.json"));

    // A store with no file yet reports "nothing persisted" rather than an error.
    assert!(store.load().expect("load empty store").is_none());

    store.save(&sample()).expect("save provider");
    let loaded = store
        .load()
        .expect("load provider")
        .expect("provider present");

    assert_eq!(loaded, sample());
}

#[test]
fn clear_removes_only_the_provider_entry() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    let store = SystemConfigStore::new(&path);

    // An unrelated system setting must survive clearing the provider.
    std::fs::write(&path, r#"{"llm":null,"future_setting":{"keep":true}}"#).expect("seed document");
    store.save(&sample()).expect("save provider");
    store.clear().expect("clear provider");

    let raw = std::fs::read_to_string(&path).expect("read document");
    assert!(
        raw.contains("future_setting"),
        "unrelated settings must be preserved: {raw}"
    );
    assert!(store.load().expect("load after clear").is_none());
}

#[test]
fn malformed_document_is_reported_not_ignored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    std::fs::write(&path, "{ this is not json").expect("seed malformed document");

    let store = SystemConfigStore::new(&path);
    let err = store.load().expect_err("malformed config must be reported");
    assert!(err.contains("Failed to parse"), "unexpected error: {err}");
}

#[test]
fn credential_is_not_exposed_by_the_redacted_view() {
    let config = sample();
    assert!(config.has_api_key());

    let redacted = config.without_secret();
    assert!(!redacted.has_api_key());
    assert_eq!(redacted.model, config.model);
    assert_eq!(redacted.base_url, config.base_url);
}

#[test]
fn resolve_rejects_blank_model_and_scheme_less_url() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SystemConfigStore::new(dir.path().join("system.json"));
    assert!(store.path().ends_with("system.json"));

    let mut config = sample();
    config.model = "  ".to_string();
    assert!(
        config
            .resolve()
            .err()
            .expect("blank model must be rejected")
            .contains("Model")
    );

    let mut config = sample();
    config.base_url = "api.deepseek.com/v1".to_string();
    assert!(
        config
            .resolve()
            .err()
            .expect("scheme-less URL must be rejected")
            .contains("http")
    );

    let mut config = sample();
    config.protocol = "carrier-pigeon".to_string();
    assert!(
        config
            .resolve()
            .err()
            .expect("unknown protocol must be rejected")
            .contains("Unsupported protocol")
    );

    // A valid description produces a client without touching the network.
    sample().resolve().expect("valid provider must build");
}

#[test]
fn persisted_provider_wins_over_the_environment_bootstrap() {
    let persisted = sample();
    let from_env = LlmProviderConfig {
        model: "env-model".to_string(),
        ..sample()
    };

    let (winner, source) = resolve_bootstrap(Some(persisted.clone()), Some(from_env.clone()))
        .expect("a bootstrap provider must resolve");
    assert_eq!(winner.model, persisted.model);
    assert_eq!(source, "data/system.json");

    let (winner, source) =
        resolve_bootstrap(None, Some(from_env.clone())).expect("environment fallback");
    assert_eq!(winner.model, from_env.model);
    assert_eq!(source, "environment");

    assert!(resolve_bootstrap(None, None).is_none());
}

#[test]
fn a_legacy_single_provider_document_is_migrated_to_a_named_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    std::fs::write(
        &path,
        r#"{"llm":{"protocol":"openai","base_url":"https://api.xiaomimimo.com/v1","model":"mimo-v2.6-flash","api_key":"sk-x"}}"#,
    )
    .expect("seed legacy document");

    let store = SystemConfigStore::new(&path);
    let settings = store.load_node_settings().expect("migrated settings");

    assert_eq!(settings.providers.len(), 1);
    assert_eq!(
        settings.providers[0].name, "xiaomi",
        "the preset for the base URL names the provider"
    );
    assert_eq!(settings.default_provider.as_deref(), Some("xiaomi"));
    assert_eq!(
        settings.default_model.as_deref(),
        Some("xiaomi/mimo-v2.6-flash"),
        "the model must be represented as provider/model-id"
    );
    assert_eq!(
        settings.source,
        kanon_api::llm_config::SettingsSource::Console
    );
}

#[test]
fn provider_names_are_derived_from_presets_and_hosts() {
    use kanon_api::derive_provider_name;

    assert_eq!(
        derive_provider_name("https://api.xiaomimimo.com/v1"),
        "xiaomi"
    );
    assert_eq!(
        derive_provider_name("https://api.deepseek.com/v1"),
        "deepseek"
    );
    assert_eq!(derive_provider_name("https://api.openai.com/v1"), "openai");
    // An endpoint no preset covers falls back to a sanitized host label.
    assert_eq!(derive_provider_name("https://llm.example.com/v1"), "llm");
    // An IP address has no usable label; `local` is the honest name for it.
    assert_eq!(derive_provider_name("http://127.0.0.1:9000/v1"), "local");
    // A known local preset still wins over the label fallback.
    assert_eq!(derive_provider_name("http://127.0.0.1:8000/v1"), "vllm");
}

#[test]
fn saving_node_settings_mirrors_the_legacy_section_and_round_trips() {
    use kanon_api::llm_config::NodeSettings;

    let dir = tempfile::tempdir().expect("temp dir");
    let store = SystemConfigStore::new(dir.path().join("system.json"));

    let settings = LlmProviderConfig {
        protocol: "openai".to_string(),
        base_url: "https://api.xiaomimimo.com/v1".to_string(),
        model: "mimo-v2.6-flash".to_string(),
        api_key: Some("sk-secret".to_string()),
        temperature: None,
        max_tokens: None,
    }
    .into_node_settings();

    store.save_node_settings(&settings).expect("save settings");

    let reloaded = store.load_node_settings().expect("reload settings");
    assert_eq!(reloaded.default_model, settings.default_model);
    assert_eq!(reloaded.providers.len(), 1);

    // The legacy section still describes the endpoint actually in effect, so an older binary can
    // keep booting from the same document.
    let legacy = store
        .load()
        .expect("load legacy")
        .expect("mirrored provider");
    assert_eq!(legacy.model, "mimo-v2.6-flash");

    // The reply policy is persisted in the same document as the providers.
    let updated = NodeSettings {
        reply_policy: kanon_core::ReplyPolicy::new(kanon_core::ReplyMode::Mention),
        ..reloaded
    };
    store.save_node_settings(&updated).expect("save policy");
    assert_eq!(
        store
            .load_node_settings()
            .expect("reload")
            .reply_policy
            .mode,
        kanon_core::ReplyMode::Mention
    );
}
