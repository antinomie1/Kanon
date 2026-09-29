//! Tests for node system-configuration persistence (`data/system.json`).

use kanon_api::llm_config::NodeSettings;
use kanon_api::{LlmProviderConfig, StartupConfig, SystemConfigStore};
use kanon_llm::ProviderEntry;

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
fn a_missing_document_yields_empty_settings_and_a_malformed_one_is_reported() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    let store = SystemConfigStore::new(&path);

    // No file yet is "nothing configured", not an error.
    let settings = store.load_node_settings().expect("empty store");
    assert!(!settings.has_providers());
    assert!(settings.default_model.is_none());

    // A document that cannot be parsed must stop startup instead of being ignored: silently
    // running without the operator's providers is the failure this store prevents.
    std::fs::write(&path, "{ this is not json").expect("seed malformed document");
    let err = store
        .load_node_settings()
        .expect_err("malformed config must be reported");
    assert!(err.contains("Failed to parse"), "unexpected error: {err}");
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
    assert_eq!(
        settings.default_model.as_deref(),
        Some("xiaomi/mimo-v2.6-flash"),
        "the model must be represented as provider/model-id"
    );
}

#[test]
fn a_legacy_default_provider_is_ignored_and_dropped_on_the_next_save() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    std::fs::write(
        &path,
        r#"{
            "llm": {"protocol":"openai","base_url":"https://api.xiaomimimo.com/v1","model":"mimo-v2.6-flash"},
            "providers": [{"name":"xiaomi","protocol":"openai","base_url":"https://api.xiaomimimo.com/v1"}],
            "default_provider": "xiaomi",
            "default_model": "xiaomi/mimo-v2.6-flash",
            "future_setting": {"keep": true}
        }"#,
    )
    .expect("seed a pre-redesign document");

    let store = SystemConfigStore::new(&path);
    let settings = store.load_node_settings().expect("load");
    assert_eq!(
        settings.default_model.as_deref(),
        Some("xiaomi/mimo-v2.6-flash")
    );

    store.save_node_settings(&settings).expect("save");
    let raw = std::fs::read_to_string(&path).expect("read document");
    assert!(
        !raw.contains("default_provider"),
        "the retired default_provider must not be written back: {raw}"
    );
    assert!(
        !raw.contains("\"llm\""),
        "the legacy single-endpoint section must not be written back: {raw}"
    );
    assert!(
        raw.contains("future_setting"),
        "unrecognized sections written by newer components must survive: {raw}"
    );
}

#[test]
fn a_persisted_default_model_naming_no_configured_provider_is_dropped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    std::fs::write(
        &path,
        r#"{
            "providers": [{"name":"xiaomi","protocol":"openai","base_url":"https://api.xiaomimimo.com/v1"}],
            "default_model": "gone/some-model"
        }"#,
    )
    .expect("seed document");

    let settings = SystemConfigStore::new(&path)
        .load_node_settings()
        .expect("load");
    assert_eq!(settings.providers.len(), 1);
    assert!(
        settings.default_model.is_none(),
        "a default that cannot resolve must not be reported as the node's model"
    );
}

#[test]
fn settings_validation_rejects_a_default_model_that_cannot_resolve() {
    let mut settings = NodeSettings {
        providers: vec![ProviderEntry::new(
            "xiaomi",
            "openai",
            "http://127.0.0.1:9/v1",
        )],
        default_model: Some("xiaomi/mimo".to_string()),
        ..NodeSettings::default()
    };
    settings.validate().expect("a served default is accepted");

    settings.default_model = Some("elsewhere/mimo".to_string());
    let err = settings.validate().expect_err("unknown provider");
    assert!(err.contains("elsewhere"), "unexpected error: {err}");

    settings.default_model = Some("mimo".to_string());
    let err = settings.validate().expect_err("no provider prefix");
    assert!(
        err.contains("<provider>/<model-id>"),
        "unexpected error: {err}"
    );

    settings.default_model = None;
    settings.validate().expect("no default model is valid");
}

#[test]
fn settings_validation_rejects_duplicate_names_and_unsupported_protocols() {
    let duplicated = NodeSettings {
        providers: vec![
            ProviderEntry::new("dup", "openai", "http://127.0.0.1:9/v1"),
            ProviderEntry::new("dup", "openai", "http://127.0.0.1:10/v1"),
        ],
        ..NodeSettings::default()
    };
    assert!(
        duplicated
            .validate()
            .expect_err("duplicate")
            .contains("dup")
    );

    let odd = NodeSettings {
        providers: vec![ProviderEntry::new(
            "odd",
            "carrier-pigeon",
            "http://127.0.0.1:9/v1",
        )],
        ..NodeSettings::default()
    };
    assert!(
        odd.validate()
            .expect_err("protocol")
            .contains("Unsupported protocol")
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
fn a_single_endpoint_description_becomes_one_provider_and_the_default_model() {
    let settings = sample().into_node_settings();

    assert_eq!(settings.providers.len(), 1);
    assert_eq!(settings.providers[0].name, "deepseek");
    assert_eq!(
        settings.default_model.as_deref(),
        Some("deepseek/deepseek-flash")
    );
    settings
        .validate()
        .expect("the migrated settings must be valid");

    // An aggregator id keeps everything after the endpoint prefix.
    let aggregator = LlmProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        model: "anthropic/claude-3.5-sonnet".to_string(),
        ..sample()
    }
    .into_node_settings();
    assert_eq!(
        aggregator.default_model.as_deref(),
        Some("openrouter/anthropic/claude-3.5-sonnet")
    );
}

#[test]
fn saving_node_settings_round_trips_models_and_policies() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("system.json");
    let store = SystemConfigStore::new(&path);

    let settings = sample().into_node_settings();
    store.save_node_settings(&settings).expect("save settings");

    let reloaded = store.load_node_settings().expect("reload settings");
    assert_eq!(reloaded.default_model, settings.default_model);
    assert_eq!(reloaded.providers, settings.providers);

    // Both operator policies are persisted in the same document as the providers, so a restart
    // applies them without any further console action.
    let updated = NodeSettings {
        reply_policy: kanon_core::ReplyPolicy::new(kanon_core::ReplyMode::Mention),
        context_policy: kanon_core::ContextPolicy {
            include_sender_id: true,
            ..kanon_core::ContextPolicy::default()
        },
        ..reloaded
    };
    store.save_node_settings(&updated).expect("save policies");

    let reloaded = store.load_node_settings().expect("reload");
    assert_eq!(reloaded.reply_policy.mode, kanon_core::ReplyMode::Mention);
    assert!(reloaded.context_policy.include_sender_id);
    assert!(!reloaded.context_policy.include_timestamp);
}

#[test]
fn the_startup_section_survives_console_saves_and_rejects_unknown_keys() {
    let dir = tempfile::tempdir().expect("temp dir");
    let absent = SystemConfigStore::new(dir.path().join("absent.json"));
    assert_eq!(absent.load_startup().unwrap(), StartupConfig::default());

    let path = dir.path().join("system.json");
    std::fs::write(
        &path,
        r#"{"startup": {"api_addr": "0.0.0.0:9000", "log": "debug"}}"#,
    )
    .unwrap();
    let store = SystemConfigStore::new(&path);
    let startup = store.load_startup().unwrap();
    assert_eq!(startup.api_addr, "0.0.0.0:9000".parse().unwrap());
    assert_eq!(startup.log, "debug");
    assert!(startup.run_dir.is_none());

    // The console rewrites the whole document; the hand-edited section must come through intact.
    store.save_node_settings(&NodeSettings::default()).unwrap();
    assert_eq!(store.load_startup().unwrap(), startup);

    // A misspelt setting fails loudly instead of silently falling back to the default.
    std::fs::write(&path, r#"{"startup": {"api_address": "0.0.0.0:9000"}}"#).unwrap();
    assert!(store.load_startup().unwrap_err().contains("api_address"));
}
