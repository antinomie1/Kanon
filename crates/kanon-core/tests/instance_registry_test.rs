//! Tests for the bot-instance catalog: persistence, adapter ownership and session rotation.

use kanon_core::instance::{BotInstance, InstanceDraft, InstanceError, InstanceRegistry};
use kanon_llm::memory::InMemory;
use kanon_llm::{
    Persona, PersonaRegistry, ProviderEntry, ProviderRegistry, SessionManager, SqliteSessionStore,
};
use std::sync::Arc;

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
async fn startup_restores_generated_personas_without_guessing_binding_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("instances.json");
    let db = Arc::new(SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap());
    let sessions = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(db.clone())
        .unwrap();
    let personas = PersonaRegistry::new();
    let providers = ProviderRegistry::new();
    personas
        .register(Persona::custom("custom", "Custom", "", "custom prompt").unwrap())
        .unwrap();
    let instances = InstanceRegistry::open(&path).await.unwrap();
    let mut owner = draft("Owner", false, &[]);
    owner.system_prompt = Some("owned prompt".into());
    instances
        .create(
            owner,
            Some(kanon_core::InstanceRuntime {
                personas: &personas,
                sessions: &sessions,
                providers: &providers,
                default_agent: "builtin",
            }),
        )
        .await
        .unwrap();
    let mut reader = draft("Reader", false, &[]);
    reader.persona_id = Some("instance:owner".into());
    instances
        .create(
            reader,
            Some(kanon_core::InstanceRuntime {
                personas: &personas,
                sessions: &sessions,
                providers: &providers,
                default_agent: "builtin",
            }),
        )
        .await
        .unwrap();
    for (key, persona) in [
        ("instance:owner:chat#0", "instance:owner"),
        ("instance:reader:chat#0", "custom"),
        ("old", "instance:gone"),
    ] {
        sessions.set_persona(key, persona).unwrap();
    }

    let restored = InstanceRegistry::open(&path).await.unwrap();
    let catalog = PersonaRegistry::new();
    catalog
        .register(Persona::custom("custom", "Custom", "", "custom prompt").unwrap())
        .unwrap();
    kanon_core::restore_instance_personas(&restored.list().await, &catalog, &sessions).unwrap();
    assert_eq!(
        restored
            .persona_for_instance("reader", Some(&catalog))
            .await
            .unwrap()
            .unwrap()
            .prompt,
        "owned prompt"
    );
    let reloaded = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(db)
        .unwrap();
    assert_eq!(
        reloaded.get_persona("instance:owner:chat#0").as_deref(),
        Some("instance:owner")
    );
    assert_eq!(
        reloaded.get_persona("instance:reader:chat#0").as_deref(),
        Some("custom")
    );
    assert!(reloaded.get_persona("old").is_none());

    // Valid legacy rows remain explicit choices; invalid custom references are not reset to base.
    sessions.set_persona("invalid", "missing-custom").unwrap();
    let error = kanon_core::restore_instance_personas(&restored.list().await, &catalog, &sessions)
        .unwrap_err();
    assert!(error.to_string().contains("missing-custom"));
    assert_eq!(
        sessions.get_persona("invalid").as_deref(),
        Some("missing-custom")
    );
    sessions.clear_persona("invalid").unwrap();
    let mut broken = draft("Broken", false, &[]);
    broken.persona_id = Some("missing-custom".into());
    restored.create(broken, None).await.unwrap();
    assert!(
        kanon_core::restore_instance_personas(&restored.list().await, &catalog, &sessions).is_err()
    );
}

#[tokio::test]
async fn create_persists_and_reloads_from_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("instances.json");

    let registry = InstanceRegistry::open(&path).await.expect("open catalog");
    assert!(registry.is_empty().await);

    let created = registry
        .create(draft("黑猪AI", true, &["qqofficial"]), None)
        .await
        .expect("create instance");
    assert!(created.enabled);
    assert_eq!(created.adapters, vec!["qqofficial".to_string()]);
    // A mostly non-ASCII name keeps whatever ASCII it contains, so the identifier stays
    // readable and stable (`黑猪AI` -> `ai`); a fully non-ASCII name falls back to `bot`.
    assert_eq!(created.id, "ai");

    // A second instance gets a distinct identifier.
    let second = registry
        .create(draft("Weather Bot", false, &[]), None)
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
        .create(draft("first", true, &["qqofficial"]), None)
        .await
        .expect("first instance");

    let conflict = registry
        .create(draft("second", true, &["qqofficial"]), None)
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
        .create(draft("third", false, &["qqofficial"]), None)
        .await
        .expect("disabled instance may reuse the adapter");
    assert!(!disabled.enabled);

    // ...until it is enabled, which must then be rejected.
    let err = registry
        .update(&disabled.id, draft("third", true, &["qqofficial"]), None)
        .await
        .expect_err("enabling a conflicting instance must fail");
    assert!(matches!(err, InstanceError::Conflict { .. }));
}

#[tokio::test]
async fn resolve_by_platform_only_returns_enabled_owners() {
    let registry = InstanceRegistry::default();

    let disabled = registry
        .create(draft("sleeping", false, &["qqofficial"]), None)
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
        .update(&disabled.id, draft("sleeping", true, &["qqofficial"]), None)
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
        .create(draft("bot", true, &["qqofficial"]), None)
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
        .create(draft("bot", true, &["qqofficial"]), None)
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
                model: Some("  deepseek/deepseek-flash ".to_string()),
                reply_policy: None,
                context_policy: None,
                plugins: Default::default(),
                skills: Default::default(),
                mcp: Default::default(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("update");

    // A form submit must not reset runtime session state.
    assert_eq!(updated.session_generation("c2c:user-1"), 1);
    assert_eq!(updated.system_prompt.as_deref(), Some("be nice"));
    assert_eq!(updated.model.as_deref(), Some("deepseek/deepseek-flash"));
    // A custom prompt wins over the selected catalog persona.
    assert_eq!(
        updated.effective_persona_id().as_deref(),
        Some("instance:bot")
    );

    let blank = registry
        .update(&instance.id, draft("   ", true, &[]), None)
        .await
        .expect_err("blank name must be rejected");
    assert!(matches!(blank, InstanceError::Invalid(_)));

    let missing = registry
        .update("nope", draft("x", true, &[]), None)
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
    registry
        .create(submitted, None)
        .await
        .expect("create instance");

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
    let stored = registry.create(builtin, None).await.expect("builtin agent");
    assert_eq!(stored.agent.as_deref(), Some("builtin"));

    let mut unknown = draft("Other Bot", false, &[]);
    unknown.agent = Some("dify".to_string());
    let err = registry
        .create(unknown, None)
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

#[tokio::test]
async fn persisted_instances_reject_ambiguous_or_invalid_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("instances.json");
    let valid = serde_json::json!({"id": "bot", "name": "Bot", "enabled": false});
    let mut invalid_records = Vec::new();
    for (field, value) in [
        ("id", serde_json::json!("bad:session")),
        ("name", serde_json::json!(" ")),
        ("adapters", serde_json::json!([""])),
        ("model", serde_json::json!("unqualified-model")),
        (
            "reply_policy",
            serde_json::json!({"mode": "probability", "probability": 5}),
        ),
        (
            "command_policy",
            serde_json::json!({"admins": ["missing-platform"]}),
        ),
    ] {
        let mut instance = valid.clone();
        instance[field] = value;
        invalid_records.push(serde_json::json!({"version": 1, "instances": [instance]}));
    }
    invalid_records
        .push(serde_json::json!({"version": 1, "instances": [valid.clone(), valid.clone()]}));
    invalid_records.push(serde_json::json!({"version": 2, "instances": [valid]}));
    for document in invalid_records {
        std::fs::write(&path, document.to_string()).unwrap();
        assert!(
            InstanceRegistry::open(&path).await.is_err(),
            "accepted {document}"
        );
    }
}

#[tokio::test]
async fn restored_instances_normalize_policy_without_resetting_session_generations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("instances.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "instances": [{
                "id": "bot", "name": " Bot ", "enabled": true,
                "adapters": [" onebot ", "onebot"],
                "command_policy": {"admins": [" onebot:42 "], "access": {" /HELP ": "admins"}},
                "session_generations": {"c2c:user": 9}
            }]
        })
        .to_string(),
    )
    .unwrap();
    let registry = InstanceRegistry::open(&path).await.unwrap();
    let restored = registry.get("bot").await.unwrap();
    assert_eq!(restored.name, "Bot");
    assert_eq!(restored.adapters, ["onebot"]);
    assert_eq!(restored.session_generation("c2c:user"), 9);
    let policy = restored.command_policy.unwrap();
    assert_eq!(policy.admins, ["onebot:42"]);
    assert!(policy.access.contains_key("help"));
    assert_eq!(
        serde_json::to_value(registry.get("bot").await.unwrap()).unwrap()["platform_sessions"],
        false
    );
    registry
        .update("bot", draft("Bot", true, &["onebot", "telegram"]), None)
        .await
        .unwrap();
    registry
        .update("bot", draft("Bot", true, &["onebot"]), None)
        .await
        .unwrap();
    let reloaded = InstanceRegistry::open(&path)
        .await
        .unwrap()
        .get("bot")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&reloaded).unwrap()["platform_sessions"],
        true,
        "removing an adapter must not reopen an ambiguous legacy transcript"
    );
    assert_eq!(reloaded.session_generation("c2c:user"), 9);
}

#[tokio::test]
async fn direct_model_changes_require_a_provider_and_preserve_the_previous_choice_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("instances.json");
    let registry = InstanceRegistry::open(&path).await.unwrap();
    let instance = registry
        .create(draft("Bot", false, &[]), None)
        .await
        .unwrap();
    let providers = ProviderRegistry::single(ProviderEntry::new(
        "endpoint",
        "openai",
        "http://127.0.0.1:9/v1",
    ))
    .unwrap();
    let updated = registry
        .set_model(
            &instance.id,
            Some(" endpoint / vendor/model ".into()),
            Some(&providers),
        )
        .await
        .unwrap();
    assert_eq!(updated.model.as_deref(), Some("endpoint/vendor/model"));
    let before = std::fs::read(&path).unwrap();
    assert!(
        registry
            .set_model(&instance.id, Some("model".into()), Some(&providers))
            .await
            .is_err()
    );
    assert_eq!(
        registry.get(&instance.id).await.unwrap().model,
        updated.model
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        registry
            .set_model(&instance.id, Some("missing/model".into()), Some(&providers))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    registry
        .with_model_users("endpoint", |users| assert_eq!(users, [instance.id.clone()]))
        .await;
    let restored = InstanceRegistry::open(&path).await.unwrap();
    restored
        .validate_models(&providers, "builtin")
        .await
        .unwrap();
    providers.replace(Vec::new()).unwrap();
    assert!(
        restored
            .validate_models(&providers, "builtin")
            .await
            .is_err()
    );
    assert!(
        registry
            .set_model(&instance.id, None, Some(&providers))
            .await
            .unwrap()
            .model
            .is_none()
    );
}

/// A model selected before endpoint deletion must be rechecked after the instance writer is acquired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_deletion_prevents_queued_instance_mutations_from_publishing_stale_models() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("instances.json");
    let registry = Arc::new(InstanceRegistry::open(&path).await.unwrap());
    let instance = registry
        .create(draft("Bot", false, &[]), None)
        .await
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let providers = Arc::new(
        ProviderRegistry::single(ProviderEntry::new(
            "endpoint",
            "openai",
            "http://127.0.0.1:9/v1",
        ))
        .unwrap(),
    );
    let personas = PersonaRegistry::new();
    let sessions = SessionManager::new(Arc::new(InMemory::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let deletion = tokio::spawn({
        let registry = registry.clone();
        let providers = providers.clone();
        async move {
            registry
                .with_model_users("endpoint", move |users| {
                    assert!(users.is_empty());
                    entered_tx.send(()).unwrap();
                    // Keep the existing deletion guard while all mutation paths queue behind it.
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    providers.replace(Vec::new()).unwrap();
                })
                .await;
        }
    });
    entered_rx.await.unwrap();
    let runtime = Some(kanon_core::InstanceRuntime {
        personas: &personas,
        sessions: &sessions,
        providers: providers.as_ref(),
        default_agent: "builtin",
    });
    let mut configured = draft("Bot", false, &[]);
    configured.model = Some("endpoint/model".into());
    let create = registry.create(configured.clone(), runtime);
    let update = registry.update(&instance.id, configured, runtime);
    let select = registry.set_model(
        &instance.id,
        Some("endpoint/model".into()),
        Some(&providers),
    );
    tokio::pin!(create, update, select);
    assert!(futures_util::poll!(&mut create).is_pending());
    assert!(futures_util::poll!(&mut update).is_pending());
    assert!(futures_util::poll!(&mut select).is_pending());
    release_tx.send(()).unwrap();
    deletion.await.unwrap();
    for result in [create.await, update.await, select.await] {
        assert!(matches!(result, Err(InstanceError::Invalid(_))));
    }
    assert_eq!(registry.list().await.len(), 1);
    assert!(registry.get(&instance.id).await.unwrap().model.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
