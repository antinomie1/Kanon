//! Tests for model references, the per-model settings catalog and provider routing.
//!
//! These pin the two rules the rest of the node depends on: a model is addressed as
//! `<provider>/<model-id>`, and the prefix must name a configured endpoint — there is no default
//! provider to guess from, so anything else is an explicit error.

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
        .replace(vec![entry("xiaomi", "http://127.0.0.1:9/v1")])
        .expect("directory accepted");

    let resolved = registry
        .resolve(&ModelRef::parse("xiaomi/mimo-v2.6-flash"))
        .expect("resolved");
    assert_eq!(resolved.provider_name, "xiaomi");
    assert_eq!(resolved.model, "mimo-v2.6-flash");
}

#[test]
fn an_aggregator_model_id_keeps_everything_after_the_provider_prefix() {
    let registry = ProviderRegistry::new();
    registry
        .replace(vec![entry("openrouter", "http://127.0.0.1:9/v1")])
        .expect("directory accepted");

    let resolved = registry
        .resolve(&ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet"))
        .expect("resolved");
    assert_eq!(resolved.provider_name, "openrouter");
    assert_eq!(
        resolved.model, "anthropic/claude-3.5-sonnet",
        "an aggregator model id must reach the endpoint verbatim"
    );
}

#[test]
fn an_unregistered_prefix_is_an_explicit_error_not_a_guess() {
    let registry = ProviderRegistry::new();
    registry
        .replace(vec![entry("openrouter", "http://127.0.0.1:9/v1")])
        .expect("directory accepted");

    // Without a default provider there is nothing to forward `anthropic/...` to: the reference
    // must say which endpoint serves it.
    let error = registry
        .resolve(&ModelRef::parse("anthropic/claude-3.5-sonnet"))
        .expect_err("unconfigured provider");
    assert!(
        error.contains("'anthropic'") && error.contains("openrouter"),
        "the error must name the unknown provider and list the configured ones: {error}"
    );
}

#[test]
fn a_reference_without_a_provider_is_an_explicit_error() {
    let registry = ProviderRegistry::new();
    registry
        .replace(vec![entry("xiaomi", "http://127.0.0.1:9/v1")])
        .expect("directory accepted");

    let error = registry
        .resolve(&ModelRef::parse("mimo-v2.6-flash"))
        .expect_err("a bare model id has no provider");
    assert!(
        error.contains("<provider>/<model-id>"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_duplicate_provider_name_is_rejected_and_keeps_the_previous_directory() {
    let registry = ProviderRegistry::new();
    registry
        .replace(vec![entry("xiaomi", "http://127.0.0.1:9/v1")])
        .expect("directory accepted");

    let error = registry
        .replace(vec![
            entry("dup", "http://127.0.0.1:9/v1"),
            entry("dup", "http://127.0.0.1:10/v1"),
        ])
        .expect_err("duplicate names must be rejected");
    assert!(error.contains("dup"), "unexpected error: {error}");
    assert_eq!(
        registry.names(),
        vec!["xiaomi".to_string()],
        "a rejected directory must leave the previous one serving"
    );
}

#[test]
fn an_unsupported_protocol_is_rejected_when_the_directory_is_installed() {
    let registry = ProviderRegistry::new();
    let error = registry
        .replace(vec![ProviderEntry::new(
            "odd",
            "carrier-pigeon",
            "http://127.0.0.1:9/v1",
        )])
        .expect_err("unsupported protocol");
    assert!(
        error.contains("Unsupported protocol"),
        "unexpected error: {error}"
    );
}

#[test]
fn concurrent_resolvers_share_the_published_client_after_each_replacement() {
    use std::sync::{Arc, Barrier};

    let registry = Arc::new(ProviderRegistry::new());
    let mut previous = None;
    for generation in 0..8 {
        registry
            .replace(vec![entry(
                "endpoint",
                &format!("http://127.0.0.1:9/{generation}"),
            )])
            .unwrap();
        let barrier = Arc::new(Barrier::new(16));
        let resolved = std::thread::scope(|scope| {
            let mut lookups = Vec::new();
            for _ in 0..16 {
                let registry = registry.clone();
                let barrier = barrier.clone();
                lookups.push(scope.spawn(move || {
                    barrier.wait();
                    registry
                        .resolve(&ModelRef::parse("endpoint/model"))
                        .unwrap()
                        .provider
                }));
            }
            lookups
                .into_iter()
                .map(|lookup| lookup.join().unwrap())
                .collect::<Vec<_>>()
        });
        for client in &resolved {
            assert!(
                Arc::ptr_eq(client, &resolved[0]),
                "one client per published endpoint"
            );
        }
        if let Some(previous) = previous {
            assert!(
                !Arc::ptr_eq(&previous, &resolved[0]),
                "replacement drops the previous client"
            );
        }
        previous = Some(resolved[0].clone());
    }
    registry.replace(Vec::new()).unwrap();
    assert!(
        registry
            .resolve(&ModelRef::parse("endpoint/model"))
            .is_err()
    );
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
