//! Configuration shape, normalization and validation.
//!
//! These tests pin the rules an operator depends on: what the defaults are, that a stored value is
//! normalized before it is persisted, and — most importantly — that a configuration which could not
//! run is rejected while an operator is still looking at the form rather than at delivery time.

use std::str::FromStr;

use kanon_adapter_milky::config::{ConfigError, MilkyConfig, TransportKind};

/// The default configuration is disabled and points at the conventional local endpoint.
#[test]
fn default_config_is_disabled_and_local() {
    let config = MilkyConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.platform, "milky");
    assert_eq!(config.base_url, "http://127.0.0.1:3010");
    assert_eq!(config.transport, TransportKind::Sse);
    assert!(config.access_token.is_none());
}

/// Normalization trims text and strips the trailing slash that would otherwise double up in URLs.
#[test]
fn normalization_trims_and_drops_trailing_slash() {
    let config = MilkyConfig {
        enabled: true,
        platform: "  milky  ".to_string(),
        display_name: Some("   ".to_string()),
        base_url: "  http://127.0.0.1:3010/  ".to_string(),
        access_token: Some("  token  ".to_string()),
        transport: TransportKind::Websocket,
    }
    .normalized();

    assert_eq!(config.platform, "milky");
    assert_eq!(config.base_url, "http://127.0.0.1:3010");
    assert_eq!(config.access_token.as_deref(), Some("token"));
    // A blank display name means "not set", so the platform identifier becomes the name.
    assert!(config.display_name.is_none());
    assert_eq!(config.effective_display_name(), "milky");
}

/// A blank token is the same as no token: neither should produce an `Authorization` header.
#[test]
fn blank_token_is_dropped() {
    let config = MilkyConfig {
        access_token: Some("   ".to_string()),
        ..MilkyConfig::default()
    }
    .normalized();

    assert!(!config.token_configured());
    assert!(config.authorization_header().is_none());
}

/// A configured token is rendered as the exact header value the protocol requires.
#[test]
fn token_renders_as_bearer_header() {
    let config = MilkyConfig {
        access_token: Some("s3cret".to_string()),
        ..MilkyConfig::default()
    };

    assert_eq!(
        config.authorization_header().as_deref(),
        Some("Bearer s3cret")
    );
    assert!(config.token_configured());
}

/// Valid configurations pass validation, including over TLS.
#[test]
fn validation_accepts_well_formed_configurations() {
    for base_url in [
        "http://127.0.0.1:3010",
        "https://milky.example.com",
        "http://milky.internal:8080/base",
        "http://[::1]:3010",
    ] {
        let config = MilkyConfig {
            base_url: base_url.to_string(),
            ..MilkyConfig::default()
        };
        assert!(
            config.validate().is_ok(),
            "expected '{base_url}' to be accepted"
        );
    }
}

/// Invalid configurations are rejected with a specific reason, never silently accepted.
#[test]
fn validation_rejects_invalid_values() {
    let cases: Vec<(MilkyConfig, &str)> = vec![
        (
            MilkyConfig {
                platform: String::new(),
                ..MilkyConfig::default()
            },
            "empty platform",
        ),
        (
            MilkyConfig {
                platform: "Milky".to_string(),
                ..MilkyConfig::default()
            },
            "uppercase platform",
        ),
        (
            MilkyConfig {
                platform: "milky bot".to_string(),
                ..MilkyConfig::default()
            },
            "platform with a space",
        ),
        (
            MilkyConfig {
                base_url: String::new(),
                ..MilkyConfig::default()
            },
            "empty base URL",
        ),
        (
            MilkyConfig {
                base_url: "127.0.0.1:3010".to_string(),
                ..MilkyConfig::default()
            },
            "scheme-less base URL",
        ),
        (
            MilkyConfig {
                base_url: "http://".to_string(),
                ..MilkyConfig::default()
            },
            "base URL without a host",
        ),
        (
            MilkyConfig {
                base_url: "http://host with space".to_string(),
                ..MilkyConfig::default()
            },
            "base URL with whitespace",
        ),
        (
            MilkyConfig {
                base_url: "http://host/path?query=1".to_string(),
                ..MilkyConfig::default()
            },
            "base URL with a query string",
        ),
    ];

    for (config, label) in cases {
        assert!(
            config.validate().is_err(),
            "expected the {label} case to be rejected"
        );
    }
}

/// `prepare` normalizes before validating, so padded input is accepted but still stored cleanly.
#[test]
fn prepare_normalizes_then_validates() {
    let prepared = MilkyConfig {
        enabled: true,
        base_url: " http://127.0.0.1:3010/ ".to_string(),
        platform: " milky ".to_string(),
        ..MilkyConfig::default()
    }
    .prepare()
    .expect("padded but otherwise valid configuration should be accepted");

    assert_eq!(prepared.base_url, "http://127.0.0.1:3010");
    assert_eq!(prepared.platform, "milky");
}

/// URLs are derived from the single configured base URL.
#[test]
fn urls_are_derived_from_the_base_url() {
    let plain = MilkyConfig {
        base_url: "http://127.0.0.1:3010".to_string(),
        ..MilkyConfig::default()
    };
    assert_eq!(
        plain.api_url("send_group_message"),
        "http://127.0.0.1:3010/api/send_group_message"
    );
    assert_eq!(plain.event_url(), "ws://127.0.0.1:3010/event");

    let secure = MilkyConfig {
        base_url: "https://milky.example.com".to_string(),
        ..MilkyConfig::default()
    };
    assert_eq!(secure.event_url(), "wss://milky.example.com/event");
}

/// The credential is never part of a configuration reported to a console.
#[test]
fn without_token_strips_the_credential() {
    let config = MilkyConfig {
        access_token: Some("s3cret".to_string()),
        ..MilkyConfig::default()
    };
    let reported = config.without_token();

    assert!(reported.access_token.is_none());
    assert!(!reported.token_configured());
    // Everything else survives the sanitization.
    assert_eq!(reported.platform, config.platform);
    assert_eq!(reported.base_url, config.base_url);
}

/// Transport names accept the aliases operators type by hand and reject the rest.
#[test]
fn transport_names_round_trip() {
    for (raw, expected) in [
        ("sse", TransportKind::Sse),
        ("SSE", TransportKind::Sse),
        ("event-stream", TransportKind::Sse),
        ("websocket", TransportKind::Websocket),
        ("ws", TransportKind::Websocket),
        (" wss ", TransportKind::Websocket),
    ] {
        assert_eq!(
            TransportKind::from_str(raw).expect("alias should parse"),
            expected,
            "unexpected result for '{raw}'"
        );
    }

    assert_eq!(TransportKind::Sse.to_string(), "sse");
    assert_eq!(TransportKind::Websocket.to_string(), "websocket");

    let error = TransportKind::from_str("carrier-pigeon").expect_err("unknown transport");
    assert!(matches!(error, ConfigError::Transport(_)));
}

/// Configuration round-trips through JSON, including forward-compatible defaults.
#[test]
fn config_round_trips_through_json() {
    let config = MilkyConfig {
        enabled: true,
        platform: "qq".to_string(),
        display_name: Some("QQ via Milky".to_string()),
        base_url: "http://127.0.0.1:3010".to_string(),
        access_token: Some("token".to_string()),
        transport: TransportKind::Websocket,
    };

    let encoded = serde_json::to_string(&config).expect("configuration should serialize");
    let decoded: MilkyConfig =
        serde_json::from_str(&encoded).expect("configuration should deserialize");

    assert_eq!(decoded, config);
}

/// A document written by an older adapter version still parses, filling in defaults.
#[test]
fn partial_document_parses_with_defaults() {
    let decoded: MilkyConfig =
        serde_json::from_str(r#"{"enabled":true,"base_url":"http://127.0.0.1:3010"}"#)
            .expect("partial document should parse");

    assert!(decoded.enabled);
    assert_eq!(decoded.platform, "milky");
    assert_eq!(decoded.transport, TransportKind::Sse);
    assert!(decoded.access_token.is_none());
}
