//! Tests for model references, the per-model settings catalog and provider routing.
//!
//! These pin the two rules the rest of the node depends on: a model is addressed as
//! `<provider>/<model-id>`, and an unregistered provider prefix is an aggregator model id rather
//! than a routing instruction.

use kanon_llm::{
    ModelCapabilities, ModelCatalog, ModelRef, ModelSettingsSource, ModelSpec, ProviderEntry,
    ProviderRegistry,
};

fn entry(name: &str, base_url: &str) -> ProviderEntry {
    ProviderEntry::new(name, "openai", base_url)
}

#[test]
fn model_references_split_once_at_the_first_slash() {
    let qualified = ModelRef::parse("xiaomi/mimo-v2.6-flash");
    assert_eq!(qualified.provider(), Some("xiaomi"));
    assert_eq!(qualified.model(), "mimo-v2.6-flash");
    assert_eq!(qualified.canonical(), "xiaomi/mimo-v2.6-flash");

    // Aggregator ids legitimately contain a slash; only the first one is the provider.
    let aggregator = ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet");
    assert_eq!(aggregator.provider(), Some("openrouter"));
    assert_eq!(aggregator.model(), "anthropic/claude-3.5-sonnet");

    let bare = ModelRef::parse("  deepseek-chat  ");
    assert_eq!(bare.provider(), None);
    assert_eq!(bare.model(), "deepseek-chat");
    assert_eq!(bare.canonical(), "deepseek-chat");

    // An empty prefix is not a provider named by the empty string.
    assert_eq!(ModelRef::parse("/model").provider(), None);
}

#[test]
fn a_registered_provider_prefix_is_stripped_from_the_upstream_model() {
    let registry = ProviderRegistry::new();
    registry
        .replace(
            vec![entry("xiaomi", "http://127.0.0.1:9/v1")],
            Some("xiaomi".to_string()),
        )
        .expect("directory accepted");

    let resolved = registry
        .resolve(&ModelRef::parse("xiaomi/mimo-v2.6-flash"))
        .expect("resolved");
    assert_eq!(resolved.provider_name, "xiaomi");
    assert_eq!(resolved.model, "mimo-v2.6-flash");
}

#[test]
fn an_unregistered_prefix_is_passed_through_to_the_default_provider() {
    let registry = ProviderRegistry::new();
    registry
        .replace(
            vec![entry("openrouter", "http://127.0.0.1:9/v1")],
            Some("openrouter".to_string()),
        )
        .expect("directory accepted");

    let resolved = registry
        .resolve(&ModelRef::parse("anthropic/claude-3.5-sonnet"))
        .expect("resolved");
    assert_eq!(
        resolved.provider_name, "openrouter",
        "the configured default serves the request"
    );
    assert_eq!(
        resolved.model, "anthropic/claude-3.5-sonnet",
        "an aggregator model id must reach the endpoint verbatim"
    );
}

#[test]
fn resolving_without_a_default_provider_is_an_explicit_error() {
    let registry = ProviderRegistry::new();
    registry
        .replace(vec![entry("xiaomi", "http://127.0.0.1:9/v1")], None)
        .expect("directory accepted");

    let error = registry
        .resolve(&ModelRef::parse("mimo-v2.6-flash"))
        .expect_err("no default provider");
    assert!(
        error.contains("default provider"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_default_pointing_at_an_unknown_provider_is_rejected() {
    let registry = ProviderRegistry::new();
    let error = registry
        .replace(
            vec![entry("xiaomi", "http://127.0.0.1:9/v1")],
            Some("nowhere".to_string()),
        )
        .expect_err("dangling default must be rejected");
    assert!(error.contains("nowhere"), "unexpected error: {error}");
}

#[test]
fn a_scheme_less_endpoint_is_rejected_before_it_is_used() {
    assert!(entry("bad", "api.example.com/v1").validate().is_err());
    assert!(
        entry("good", "https://api.example.com/v1")
            .validate()
            .is_ok()
    );
}

#[test]
fn the_catalog_returns_conservative_defaults_for_unknown_models() {
    let catalog = ModelCatalog::new();
    let settings = catalog.settings_for(&ModelRef::parse("xiaomi/unknown"));

    assert_eq!(settings.source, ModelSettingsSource::Unknown);
    assert!(
        settings.capabilities.tool_calling,
        "an unknown model is assumed to accept tools"
    );
    assert!(
        !settings.capabilities.vision,
        "an unknown model must not be assumed to accept images"
    );
    assert!(settings.context_length.is_none());
    assert!(!catalog.supports_vision(&ModelRef::parse("xiaomi/unknown")));
}

#[test]
fn the_catalog_round_trips_and_scopes_removal_by_provider() {
    let catalog = ModelCatalog::new();

    let mut spec = ModelSpec::new("xiaomi", "mimo-v2.6-flash");
    spec.context_length = Some(262_144);
    spec.capabilities = ModelCapabilities {
        vision: true,
        ..ModelCapabilities::default()
    };
    catalog.upsert(spec).expect("upsert");
    catalog
        .upsert(ModelSpec::new("deepseek", "deepseek-chat"))
        .expect("upsert");

    let reference = ModelRef::parse("xiaomi/mimo-v2.6-flash");
    let stored = catalog.get(&reference).expect("stored spec");
    assert_eq!(stored.context_length, Some(262_144));
    assert!(catalog.supports_vision(&reference));

    assert_eq!(catalog.remove_provider("xiaomi"), 1);
    assert!(catalog.get(&reference).is_none());
    assert_eq!(catalog.len(), 1, "other providers are untouched");
}

#[test]
fn an_entry_without_a_provider_or_model_id_is_rejected() {
    let catalog = ModelCatalog::new();
    assert!(catalog.upsert(ModelSpec::new("", "model")).is_err());
    assert!(catalog.upsert(ModelSpec::new("provider", "  ")).is_err());
}
