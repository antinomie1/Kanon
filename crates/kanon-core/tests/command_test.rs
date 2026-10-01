//! Tests for command parsing, priority resolution, and dispatching.

use std::path::PathBuf;
use std::sync::Arc;

use kanon_core::pipeline::{CommandRouter, TriggerMatcher};
use kanon_core::supervisor::ManagedHost;
use kanon_proto::v1::{CommandMeta, PipelineEventRequest, PluginMeta, TriggerMeta};

fn create_mock_host(host_id: &str, priority: i32, commands: Vec<CommandMeta>) -> Arc<ManagedHost> {
    let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
    let plugin_id = format!("org.kanon.plugin.{host_id}");
    let meta = vec![PluginMeta {
        id: plugin_id,
        name: host_id.to_string(),
        version: "1.0.0".to_string(),
        author: "Tester".to_string(),
        description: "Test host".to_string(),
        commands,
        tools: vec![],
        ..Default::default()
    }];
    Arc::new(ManagedHost::new(
        host_id.to_string(),
        PathBuf::from(format!("/tmp/{host_id}.sock")),
        channel,
        meta,
        priority,
    ))
}

/// Shorthand for the name and arguments of a parsed command.
fn parsed(text: &str) -> Option<(String, Vec<String>, String)> {
    CommandRouter::parse_command(text).map(|parsed| (parsed.name, parsed.args, parsed.raw_args))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn test_parse_command_valid() {
    assert_eq!(
        parsed("/rustcalc 2 + 2"),
        Some(("rustcalc".into(), strings(&["2", "+", "2"]), "2 + 2".into()))
    );
    assert_eq!(
        parsed("   /weather   beijing   shanghai   "),
        Some((
            "weather".into(),
            strings(&["beijing", "shanghai"]),
            "beijing   shanghai".into()
        ))
    );
    assert_eq!(
        parsed("/ping"),
        Some(("ping".into(), vec![], String::new()))
    );
}

/// Quoted text stays one argument, in ASCII and full-width quotes alike; an unclosed quote runs to
/// the end of the text instead of failing the command.
#[test]
fn test_parse_command_quotes() {
    assert_eq!(
        CommandRouter::parse_command(r#"/say "hello world" 'a b' “你 好” """#)
            .unwrap()
            .args,
        strings(&["hello world", "a b", "你 好", ""])
    );
    assert_eq!(
        CommandRouter::parse_command(r#"/say "unclosed rest"#)
            .unwrap()
            .args,
        strings(&["unclosed rest"])
    );
    assert_eq!(
        CommandRouter::parse_command(r#"/say it's"#).unwrap().args,
        strings(&["its"])
    );
}

#[test]
fn test_parse_command_invalid() {
    assert_eq!(CommandRouter::parse_command("hello world"), None);
    assert_eq!(CommandRouter::parse_command("/"), None);
    assert_eq!(CommandRouter::parse_command("   "), None);
    assert_eq!(CommandRouter::parse_command(""), None);
}

#[tokio::test]
async fn test_resolve_command_priority() {
    let host1 = create_mock_host(
        "host1",
        500,
        vec![CommandMeta {
            name: "calc".to_string(),
            description: "Host 1 calc".to_string(),
            usage: "/calc <expr>".to_string(),
            priority: 200,
            ..Default::default()
        }],
    );

    let host2 = create_mock_host(
        "host2",
        500,
        vec![CommandMeta {
            name: "calc".to_string(),
            description: "Host 2 calc".to_string(),
            usage: "/calc <expr>".to_string(),
            priority: 50,
            ..Default::default()
        }],
    );

    let hosts = vec![host1.clone(), host2.clone()];
    let resolved = CommandRouter::resolve("calc", &hosts, &PipelineEventRequest::default())
        .expect("Should resolve");
    assert_eq!(resolved.host.host_id, "host2");
    assert_eq!(resolved.meta.priority, 50);

    let resolved_slash = CommandRouter::resolve("/calc", &hosts, &PipelineEventRequest::default())
        .expect("Should resolve with leading slash");
    assert_eq!(resolved_slash.host.host_id, "host2");

    let not_found = CommandRouter::resolve("unknown", &hosts, &PipelineEventRequest::default());
    assert!(not_found.is_none());
}

#[tokio::test]
async fn test_resolve_host_priority_fallback() {
    let host1 = create_mock_host(
        "host_low_priority",
        600,
        vec![CommandMeta {
            name: "echo".to_string(),
            description: "Low priority host echo".to_string(),
            usage: "/echo <msg>".to_string(),
            priority: 100,
            ..Default::default()
        }],
    );

    let host2 = create_mock_host(
        "host_high_priority",
        100,
        vec![CommandMeta {
            name: "echo".to_string(),
            description: "High priority host echo".to_string(),
            usage: "/echo <msg>".to_string(),
            priority: 100,
            ..Default::default()
        }],
    );

    let hosts = vec![host1, host2];
    let resolved = CommandRouter::resolve("echo", &hosts, &PipelineEventRequest::default())
        .expect("Should resolve");
    assert_eq!(resolved.host.host_id, "host_high_priority");
}

/// An alias routes to its command, and the handler is told the canonical name.
#[tokio::test]
async fn test_resolve_alias_reports_canonical_name() {
    let host = create_mock_host(
        "host_alias",
        500,
        vec![CommandMeta {
            name: "weather".to_string(),
            aliases: vec!["w".to_string(), "/天气".to_string()],
            ..Default::default()
        }],
    );
    let hosts = vec![host];

    for typed in ["w", "天气", "weather"] {
        let resolved = CommandRouter::resolve(typed, &hosts, &PipelineEventRequest::default())
            .expect("alias resolves");
        assert_eq!(resolved.name(), "weather");
    }
}

/// Builds a host declaring the given triggers.
fn trigger_host(host_id: &str, triggers: Vec<TriggerMeta>) -> Arc<ManagedHost> {
    let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
    Arc::new(ManagedHost::new(
        host_id.to_string(),
        PathBuf::from(format!("/tmp/{host_id}.sock")),
        channel,
        vec![PluginMeta {
            id: format!("org.kanon.plugin.{host_id}"),
            triggers,
            ..Default::default()
        }],
        500,
    ))
}

/// The lowest-priority matching trigger wins and receives its capture groups; an invalid pattern
/// never matches; a trigger the sender may not fire is skipped in favour of the next one.
#[tokio::test]
async fn test_trigger_resolution() {
    let hosts = vec![trigger_host(
        "host_triggers",
        vec![
            TriggerMeta {
                name: "broken".to_string(),
                pattern: "([".to_string(),
                priority: 1,
                ..Default::default()
            },
            TriggerMeta {
                name: "admin_roll".to_string(),
                pattern: r"^roll (\d+)$".to_string(),
                priority: 5,
                access: kanon_proto::v1::CommandAccess::Admins as i32,
                ..Default::default()
            },
            TriggerMeta {
                name: "roll".to_string(),
                pattern: r"^roll (\d+)(d(\d+))?$".to_string(),
                priority: 10,
                ..Default::default()
            },
        ],
    )];
    let matcher = TriggerMatcher::default();

    let everyone = |meta: &TriggerMeta| meta.access() == kanon_proto::v1::CommandAccess::Everyone;
    let matched = matcher
        .resolve(
            "roll 20",
            &PipelineEventRequest::default(),
            &hosts,
            everyone,
        )
        .expect("roll matches");
    assert_eq!(matched.meta.name, "roll");
    assert_eq!(matched.captures, strings(&["20", "", ""]));

    let admin = matcher
        .resolve("roll 20", &PipelineEventRequest::default(), &hosts, |_| {
            true
        })
        .expect("admins fire the higher-priority trigger");
    assert_eq!(admin.meta.name, "admin_roll");

    assert!(
        matcher
            .resolve("hello", &PipelineEventRequest::default(), &hosts, |_| true)
            .is_none()
    );
}

/// A message from a group on `platform`, as an adapter reports it.
fn group_event(platform: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        platform: platform.to_string(),
        metadata: Some(kanon_proto::prost_types::Struct {
            fields: [(
                kanon_core::META_CONVERSATION_KIND.to_string(),
                kanon_proto::prost_types::Value {
                    kind: Some(kanon_proto::prost_types::value::Kind::StringValue(
                        "group".to_string(),
                    )),
                },
            )]
            .into(),
        }),
        ..Default::default()
    }
}

/// A command or trigger limited to some platforms or conversation kinds does not exist
/// elsewhere, so a less preferred declaration of the same name answers instead.
#[tokio::test]
async fn test_scoped_commands_and_triggers() {
    use kanon_proto::v1::ConversationKind;

    let hosts = vec![
        create_mock_host(
            "host_scoped",
            1,
            vec![CommandMeta {
                name: "kick".to_string(),
                platforms: vec!["onebot".to_string()],
                conversation_kinds: vec![ConversationKind::Group as i32],
                ..Default::default()
            }],
        ),
        create_mock_host(
            "host_fallback",
            9,
            vec![CommandMeta {
                name: "kick".to_string(),
                ..Default::default()
            }],
        ),
    ];
    let resolved = |event: &PipelineEventRequest| {
        CommandRouter::resolve("kick", &hosts, event).map(|target| target.host.host_id.clone())
    };
    assert_eq!(
        resolved(&group_event("onebot")).as_deref(),
        Some("host_scoped")
    );
    assert_eq!(
        resolved(&group_event("telegram")).as_deref(),
        Some("host_fallback")
    );
    let private = PipelineEventRequest {
        platform: "onebot".to_string(),
        ..Default::default()
    };
    assert_eq!(resolved(&private).as_deref(), Some("host_fallback"));

    let triggers = vec![trigger_host(
        "host_triggers",
        vec![TriggerMeta {
            name: "hello".to_string(),
            pattern: "^hi$".to_string(),
            conversation_kinds: vec![ConversationKind::Private as i32],
            ..Default::default()
        }],
    )];
    let matcher = TriggerMatcher::default();
    assert!(
        matcher
            .resolve("hi", &private, &triggers, |_| true)
            .is_some()
    );
    assert!(
        matcher
            .resolve("hi", &group_event("onebot"), &triggers, |_| true)
            .is_none()
    );
}
