//! Tests for skill storage and the two policy layers that gate it.
//!
//! A skill is installed on disk but only reaches the model when both the node-wide switch and the
//! instance's own override allow it. These tests cover the store, the catalog hook that injects
//! only descriptions, and the `read_skill` tool that refuses a skill the caller may not use.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry, ItemPolicy};
use kanon_core::skill::{MAX_SKILL_BYTES, ReadSkillTool, SkillCatalogHook, SkillError, SkillStore};
use kanon_core::toggle::{SKILL_SECTION, ToggleStore};
use kanon_llm::BuiltinAgent;
use kanon_llm::agent::{Agent, AgentHook, AgentTool};
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse, Role};
use kanon_llm::memory::InMemory;
use kanon_llm::{GatewayError, LlmProvider, PersonaRegistry, SessionManager};

/// Writes one installable skill and returns the store root.
fn write_skill(root: &Path, id: &str, description: &str, body: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("skill body");
}

/// Creates an instance carrying one skill override.
async fn instance_with_policy(
    registry: &InstanceRegistry,
    skill_id: &str,
    policy: ItemPolicy,
    adapter: &str,
) -> String {
    registry
        .create(
            InstanceDraft {
                name: format!("Skill Bot {adapter}"),
                enabled: true,
                adapters: vec![adapter.to_string()],
                persona_id: None,
                system_prompt: None,
                model: None,
                reply_policy: None,
                context_policy: None,
                plugins: Default::default(),
                skills: HashMap::from([(skill_id.to_string(), policy)]),
                mcp: Default::default(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create instance")
        .id
}

#[tokio::test]
async fn install_read_and_remove_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("source");
    write_skill(dir.path(), "source", "Source skill", "Body text");

    let store = SkillStore::new(dir.path().join("skills"));
    let meta = store
        .install_from_dir(&source, "installed")
        .expect("install");
    assert_eq!(meta.id, "installed");
    assert_eq!(meta.description, "Source skill");
    assert!(store.read("installed").expect("read").contains("Body text"));

    assert_eq!(store.list().expect("list").len(), 1);

    // Replacing from the installed directory itself must stage a copy before moving it.
    let installed = store.root().join("installed");
    store.install_from_dir(&installed, "installed").unwrap();
    assert!(store.read("installed").unwrap().contains("Body text"));

    write_skill(dir.path(), "source", "Updated skill", "Updated body");
    store.install_from_dir(&source, "installed").unwrap();
    assert!(store.read("installed").unwrap().contains("Updated body"));
    assert_eq!(store.list().unwrap().len(), 1);

    store.remove("installed").expect("remove");
    assert_eq!(store.list().expect("list").len(), 0);
    assert!(matches!(
        store.read("installed").err(),
        Some(SkillError::NotFound(_))
    ));
}

#[test]
fn failed_skill_replacement_preserves_the_installed_body_and_resources() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    write_skill(dir.path(), "source", "Original", "Original body");
    std::fs::write(source.join("resource.txt"), "original resource").unwrap();
    let store = SkillStore::new(dir.path().join("skills"));
    store.install_from_dir(&source, "installed").unwrap();
    let original = store.read("installed").unwrap();

    for invalid in [vec![0xff], vec![b'x'; MAX_SKILL_BYTES + 1]] {
        std::fs::write(source.join("SKILL.md"), invalid).unwrap();
        assert!(store.install_from_dir(&source, "installed").is_err());
        assert_eq!(store.read("installed").unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(store.root().join("installed/resource.txt")).unwrap(),
            "original resource"
        );
        assert_eq!(store.list().unwrap().len(), 1);
    }

    // A source containing its destination would recursively copy its own staging directory.
    std::fs::write(store.root().join("SKILL.md"), "ancestor source").unwrap();
    assert!(matches!(
        store.install_from_dir(store.root(), "installed"),
        Err(SkillError::InvalidSource(_))
    ));
    assert_eq!(store.read("installed").unwrap(), original);

    #[cfg(unix)]
    {
        write_skill(dir.path(), "source", "Replacement", "Replacement body");
        std::os::unix::fs::symlink(&source, source.join("recursive-link")).unwrap();
        assert!(matches!(
            store.install_from_dir(&source, "installed"),
            Err(SkillError::InvalidSource(_))
        ));
        assert_eq!(store.read("installed").unwrap(), original);
        assert_eq!(store.list().unwrap().len(), 1);
    }
}

#[test]
fn directory_commit_failure_restores_the_previous_installation() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("installed");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("SKILL.md"), "original").unwrap();
    let staged = tempfile::tempdir_in(root.path()).unwrap();
    // Simulate a candidate disappearing after validation but before the commit rename.
    std::fs::remove_dir(staged.path()).unwrap();
    let error = kanon_core::directory::swap_into_place(staged, &target, root.path()).unwrap_err();
    assert!(error.previous_in_place);
    assert_eq!(
        std::fs::read_to_string(target.join("SKILL.md")).unwrap(),
        "original"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn identifiers_are_rejected_before_touching_the_filesystem() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("source");
    write_skill(dir.path(), "source", "Source skill", "Body");
    let store = SkillStore::new(dir.path().join("skills"));

    for bad in ["", "../escape", ".hidden", "bad id", "a/b"] {
        assert!(
            matches!(store.read(bad).err(), Some(SkillError::InvalidId(_))),
            "'{bad}' must be refused"
        );
    }

    // A directory without SKILL.md is a client mistake, reported as such.
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).expect("dir");
    assert!(matches!(
        store.install_from_dir(&empty, "empty").err(),
        Some(SkillError::InvalidSource(_))
    ));
    assert!(source.is_dir(), "the fixture source must still exist");
}

#[tokio::test]
async fn an_oversized_skill_body_is_refused_when_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_skill(
        dir.path(),
        "big",
        "Big skill",
        &"x".repeat(MAX_SKILL_BYTES + 1),
    );
    let store = SkillStore::new(dir.path());

    assert!(matches!(
        store.read("big").err(),
        Some(SkillError::TooLarge { .. })
    ));
}

#[tokio::test]
async fn the_catalog_hook_injects_descriptions_into_an_instance_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_skill(
        dir.path(),
        "alpha",
        "Alpha description",
        "Alpha body with secret detail",
    );
    let store = Arc::new(SkillStore::new(dir.path()));
    let toggles = Arc::new(ToggleStore::in_memory());
    let registry = Arc::new(InstanceRegistry::default());
    let instance_id =
        instance_with_policy(&registry, "alpha", ItemPolicy::Inherit, "qqofficial").await;

    let hook = SkillCatalogHook::new(store.clone(), toggles.clone(), registry.clone());
    let tools =
        vec![ReadSkillTool::new(store.clone(), toggles.clone(), registry.clone()).definition()];
    let mut injected = "You are a bot".to_string();

    hook.on_system_prompt(
        &format!("instance:{instance_id}:group:1:user:1#0"),
        &mut injected,
        &tools,
    )
    .await
    .expect("hook");

    assert!(injected.contains("Alpha description"), "{injected}");
    // Progressive disclosure: the body must not be spent from the context window up front.
    assert!(
        !injected.contains("Alpha body with secret detail"),
        "only the description may enter the prompt"
    );

    let mut without_tools = "You are a bot".to_string();
    hook.on_system_prompt(
        &format!("instance:{instance_id}:group:1:user:1#0"),
        &mut without_tools,
        &[],
    )
    .await
    .expect("hook without tools");
    assert_eq!(without_tools, "You are a bot");

    // The global switch removes it from the catalog entirely.
    toggles
        .set_enabled(SKILL_SECTION, "alpha", false)
        .await
        .expect("disable");
    let mut disabled = "You are a bot".to_string();
    hook.on_system_prompt(
        &format!("instance:{instance_id}:group:1:user:1#0"),
        &mut disabled,
        &tools,
    )
    .await
    .expect("hook");
    assert_eq!(
        disabled, "You are a bot",
        "no catalog when nothing is enabled"
    );
}

#[tokio::test]
async fn read_skill_enforces_the_instance_override() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_skill(dir.path(), "alpha", "Alpha description", "Alpha body");
    write_skill(dir.path(), "beta", "Beta description", "Beta body");
    let store = Arc::new(SkillStore::new(dir.path()));
    let toggles = Arc::new(ToggleStore::in_memory());
    let registry = Arc::new(InstanceRegistry::default());
    let instance_id =
        instance_with_policy(&registry, "alpha", ItemPolicy::Disable, "qqofficial").await;

    let tool = ReadSkillTool::new(store, toggles, registry);
    let session = format!("instance:{instance_id}:group:1:user:1#0");

    // A disabled skill is refused even though the model guessed the right name.
    let err = tool
        .call(&session, serde_json::json!({ "name": "alpha" }))
        .await
        .err()
        .expect("disabled skill must be refused");
    assert!(err.contains("not available"), "{err}");

    let body = tool
        .call(&session, serde_json::json!({ "name": "beta" }))
        .await
        .expect("allowed skill");
    assert!(body.text.contains("Beta body"));

    let missing = tool
        .call(&session, serde_json::json!({}))
        .await
        .err()
        .expect("name is required");
    assert!(missing.contains("name"), "{missing}");
}

/// Provider that records the messages of the request the agent actually sends.
struct RecordingProvider {
    seen: std::sync::Arc<std::sync::Mutex<Vec<ChatMessage>>>,
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        *self.seen.lock().expect("recording lock") = request.messages.clone();
        Ok(ChatResponse {
            content: Some("ok".to_string()),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn the_skill_catalog_reaches_the_model_alongside_the_persona() {
    // End-to-end version of the hook-order guarantee: with the real catalog hook registered the
    // way the node registers it, a conversation that has no system prompt yet must still receive
    // both the persona and the catalog. Injecting the catalog without this test is how it went
    // missing in production.
    let dir = tempfile::tempdir().expect("temp dir");
    write_skill(dir.path(), "alpha", "Alpha description", "Alpha body");
    let store = Arc::new(SkillStore::new(dir.path()));
    let toggles = Arc::new(ToggleStore::in_memory());
    let registry = Arc::new(InstanceRegistry::default());

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let personas = Arc::new(PersonaRegistry::default());
    sessions
        .set_persona("instance:ai:group:1:user:1#0", "assistant")
        .unwrap();

    let agent = BuiltinAgent::builder(
        "catalog-integration",
        Arc::new(RecordingProvider { seen: seen.clone() }),
    )
    .memory(memory)
    .session_manager(sessions)
    .persona_registry(personas)
    .tool(ReadSkillTool::new(
        store.clone(),
        toggles.clone(),
        registry.clone(),
    ))
    .hook(SkillCatalogHook::new(store, toggles, registry))
    .model("test-model")
    .build();

    agent
        .run("instance:ai:group:1:user:1#0", "你好", &[])
        .await
        .expect("agent run");

    let messages = seen.lock().expect("recording lock").clone();
    let system_messages: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| message.content.as_deref())
        .collect();

    assert!(
        system_messages
            .iter()
            .any(|content| content.contains("Alpha description")),
        "the catalog must be in the request: {system_messages:?}"
    );
    assert!(
        system_messages
            .iter()
            .any(|content| content.starts_with(kanon_llm::BASE_PERSONA_PROMPT)),
        "the persona must still lead the static system block: {system_messages:?}"
    );
}
