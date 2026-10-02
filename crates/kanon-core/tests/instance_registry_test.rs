//! Tests for the bot-instance catalog: persistence, adapter ownership and session rotation.

use kanon_core::instance::{BotInstance, InstanceDraft, InstanceError, InstanceRegistry};

fn draft(name: &str, enabled: bool, adapters: &[&str]) -> InstanceDraft {
    InstanceDraft {
        name: name.to_string(),
        enabled,
        adapters: adapters.iter().map(|a| a.to_string()).collect(),
        persona_id: None,
        system_prompt: None,
        model: None,
        reply_policy: None,
        context_policy: None,
        plugins: Default::default(),
        skills: Default::default(),
        mcp: Default::default(),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_persists_and_reloads_from_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");

    let registry = InstanceRegistry::open(&path).await.expect("open catalog");
    assert!(registry.is_empty().await);

    let created = registry
        .create(draft("黑猪AI", true, &["qqofficial"]))
        .await
        .expect("create instance");
    assert!(created.enabled);
    assert_eq!(created.adapters, vec!["qqofficial".to_string()]);
    // A mostly non-ASCII name keeps whatever ASCII it contains, so the identifier stays
    // readable and stable (`黑猪AI` -> `ai`); a fully non-ASCII name falls back to `bot`.
    assert_eq!(created.id, "ai");

    // A second instance gets a distinct identifier.
    let second = registry
        .create(draft("Weather Bot", false, &[]))
        .await
        .expect("create second instance");
    assert_eq!(second.id, "weather-bot");

    // The catalog survives a restart.
    let reloaded = InstanceRegistry::open(&path).await.expect("reopen catalog");
    let names: Vec<String> = reloaded.list().await.into_iter().map(|i| i.name).collect();
    assert_eq!(names, vec!["Weather Bot".to_string(), "黑猪AI".to_string()]);
}

#[tokio::test]
async fn enabled_instances_cannot_share_an_adapter() {
    let registry = InstanceRegistry::default(); // in-memory: no disk writes in tests

    registry
        .create(draft("first", true, &["qqofficial"]))
        .await
        .expect("first instance");

    let conflict = registry
        .create(draft("second", true, &["qqofficial"]))
        .await
        .expect_err("second enabled instance must not claim the same adapter");
    match conflict {
        InstanceError::Conflict { platform, owner } => {
            assert_eq!(platform, "qqofficial");
            assert_eq!(owner, "first");
        }
        other => panic!("unexpected error: {other}"),
    }

    // A disabled instance does not collide, because it serves nothing...
    let disabled = registry
        .create(draft("third", false, &["qqofficial"]))
        .await
        .expect("disabled instance may reuse the adapter");
    assert!(!disabled.enabled);

    // ...until it is enabled, which must then be rejected.
    let err = registry
        .update(&disabled.id, draft("third", true, &["qqofficial"]))
        .await
        .expect_err("enabling a conflicting instance must fail");
    assert!(matches!(err, InstanceError::Conflict { .. }));
}

#[tokio::test]
async fn resolve_by_platform_only_returns_enabled_owners() {
    let registry = InstanceRegistry::default();

    let disabled = registry
        .create(draft("sleeping", false, &["qqofficial"]))
        .await
        .expect("create disabled instance");

    // No enabled instance claims the platform, so nothing may answer.
    assert!(
        registry
            .resolve_by_platform("qqofficial")
            .await
            .expect("resolve")
            .is_none()
    );

    registry
        .update(&disabled.id, draft("sleeping", true, &["qqofficial"]))
        .await
        .expect("enable instance");

    let resolved = registry
        .resolve_by_platform("qqofficial")
        .await
        .expect("resolve")
        .expect("enabled instance claims the platform");
    assert_eq!(resolved.id, disabled.id);

    // An unclaimed platform still resolves to nothing.
    assert!(
        registry
            .resolve_by_platform("telegram")
            .await
            .expect("resolve")
            .is_none()
    );
}

#[tokio::test]
async fn ambiguous_ownership_is_reported_instead_of_guessed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");

    // A hand-edited catalog can contain two enabled owners of one platform.
    std::fs::write(
        &path,
        r#"{
  "version": 1,
  "instances": [
    {"id": "a", "name": "A", "enabled": true, "adapters": ["qqofficial"]},
    {"id": "b", "name": "B", "enabled": true, "adapters": ["qqofficial"]}
  ]
}"#,
    )
    .expect("seed catalog");

    // Opening such a catalog fails loudly rather than routing arbitrarily.
    let err = InstanceRegistry::open(&path)
        .await
        .expect_err("ambiguous catalog must be rejected");
    assert!(
        matches!(err, InstanceError::Conflict { .. }),
        "unexpected: {err}"
    );
}

#[tokio::test]
async fn new_command_rotates_only_its_conversation_and_keeps_history() {
    let registry = InstanceRegistry::default();
    let instance = registry
        .create(draft("bot", true, &["qqofficial"]))
        .await
        .expect("create instance");

    let conversation = "c2c:user-1";
    let other = "group:user-2";

    let initial = instance.conversation_session_id(conversation);
    assert!(initial.ends_with("#0"), "unexpected session id: {initial}");

    let rotated = registry
        .select_session(&instance.id, conversation, 1)
        .await
        .expect("rotate session");
    assert!(rotated.ends_with("#1"), "unexpected session id: {rotated}");
    assert_ne!(rotated, initial, "rotation must move to a new session key");

    // Only the issuing conversation moves; other conversations keep their session.
    let current = registry
        .get(&instance.id)
        .await
        .expect("instance still present");
    assert_eq!(current.conversation_session_id(conversation), rotated);
    assert_eq!(
        current.conversation_session_id(other),
        other_session(&current, other)
    );

    // The catalog remembers the rotation across a restart.
    let restored = registry.get(&instance.id).await.expect("instance");
    assert_eq!(restored.session_generation(conversation), 1);
    assert_eq!(restored.session_generation(other), 0);
}

/// Convenience: session id of a conversation that never used `/new`.
fn other_session(instance: &BotInstance, conversation: &str) -> String {
    format!("instance:{}:{conversation}#0", instance.id)
}

#[tokio::test]
async fn update_preserves_session_history_and_validates_input() {
    let registry = InstanceRegistry::default();
    let instance = registry
        .create(draft("bot", true, &["qqofficial"]))
        .await
        .expect("create");

    registry
        .select_session(&instance.id, "c2c:user-1", 1)
        .await
        .expect("rotate");

    let updated = registry
        .update(
            &instance.id,
            InstanceDraft {
                name: "Renamed".to_string(),
                enabled: true,
                adapters: vec!["qqofficial".to_string()],
                persona_id: Some("assistant".to_string()),
                system_prompt: Some("  be nice  ".to_string()),
                model: Some("  deepseek-flash ".to_string()),
                reply_policy: None,
                context_policy: None,
                plugins: Default::default(),
                skills: Default::default(),
                mcp: Default::default(),
                ..Default::default()
            },
        )
        .await
        .expect("update");

    // A form submit must not reset runtime session state.
    assert_eq!(updated.session_generation("c2c:user-1"), 1);
    assert_eq!(updated.system_prompt.as_deref(), Some("be nice"));
    assert_eq!(updated.model.as_deref(), Some("deepseek-flash"));
    // A custom prompt wins over the selected catalog persona.
    assert_eq!(
        updated.effective_persona_id().as_deref(),
        Some("instance:bot")
    );

    let blank = registry
        .update(&instance.id, draft("   ", true, &[]))
        .await
        .expect_err("blank name must be rejected");
    assert!(matches!(blank, InstanceError::Invalid(_)));

    let missing = registry
        .update("nope", draft("x", true, &[]))
        .await
        .expect_err("unknown instance must be rejected");
    assert!(matches!(missing, InstanceError::NotFound(_)));
}

#[tokio::test]
async fn instance_policies_survive_a_restart() {
    use kanon_core::{ContextPolicy, ReplyMode, ReplyPolicy};

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");
    let registry = InstanceRegistry::open(&path).await.expect("open catalog");

    let mut submitted = draft("Policy Bot", true, &["qqofficial"]);
    submitted.reply_policy = Some(ReplyPolicy {
        mode: ReplyMode::Mention,
        probability: 0.5,
        ..Default::default()
    });
    submitted.context_policy = Some(ContextPolicy {
        include_channel_id: true,
        include_sender_id: false,
        include_timestamp: true,
        ..Default::default()
    });
    registry.create(submitted).await.expect("create instance");

    // Both policies are part of the instance record, so a restart applies them immediately instead
    // of waiting for a console visit.
    let reloaded = InstanceRegistry::open(&path).await.expect("reopen catalog");
    let instance = reloaded
        .list()
        .await
        .into_iter()
        .next()
        .expect("instance stored");

    let reply = instance.reply_policy.expect("reply policy stored");
    assert_eq!(reply.mode, ReplyMode::Mention);
    let context = instance.context_policy.expect("context policy stored");
    assert!(context.include_channel_id);
    assert!(context.include_timestamp);
    assert!(!context.include_sender_id);
}

#[tokio::test]
async fn an_unknown_agent_is_refused_on_save_and_on_load() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");
    let registry = InstanceRegistry::open(&path).await.expect("open catalog");

    let mut builtin = draft("Agent Bot", true, &["qqofficial"]);
    builtin.agent = Some(" builtin ".to_string());
    let stored = registry.create(builtin).await.expect("builtin agent");
    assert_eq!(stored.agent.as_deref(), Some("builtin"));

    let mut unknown = draft("Other Bot", false, &[]);
    unknown.agent = Some("dify".to_string());
    let err = registry
        .create(unknown)
        .await
        .expect_err("an agent the node cannot run must be refused");
    assert!(
        matches!(err, InstanceError::Invalid(_)),
        "unexpected: {err}"
    );

    // A hand-edited catalog naming an unknown agent stops startup instead of quietly answering
    // with the built-in agent.
    std::fs::write(
        &path,
        r#"{"version": 1, "instances": [{"id": "a", "name": "A", "enabled": true, "agent": "dify"}]}"#,
    )
    .expect("seed catalog");
    let err = InstanceRegistry::open(&path)
        .await
        .expect_err("unknown agent must be rejected on load");
    assert!(
        matches!(err, InstanceError::Invalid(_)),
        "unexpected: {err}"
    );
}
