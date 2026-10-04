//! A conversation continues where it left off: after the node restarts, and after the bot
//! instance that serves it is edited.
//!
//! The pieces that make this true live in different crates — the instance catalog (session key
//! and generation), the session store (persona binding, counters) and the memory backend (history)
//! — so the guarantee is tested end to end through the pipeline, over a real database file.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse, Role};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{
    GatewayError, LlmProvider, Persona, PersonaRegistry, SessionManager, SqliteMemory,
    SqliteSessionStore,
};
use kanon_proto::v1::PipelineEventRequest;

/// Provider that records the messages of every request.
#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait]
impl LlmProvider for Recorder {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request.messages.clone());
        Ok(ChatResponse {
            content: Some(format!("reply {}", requests.len())),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        })
    }
}

/// One running node: everything that is rebuilt when the process restarts.
struct Node {
    engine: PipelineEngine,
    provider: Arc<Recorder>,
    sessions: Arc<SessionManager>,
    registry: Arc<InstanceRegistry>,
}

/// Starts a node whose state lives under `dir`, the way the composition root does.
async fn start(dir: &Path) -> Node {
    let db = dir.join("sessions.db");
    let sessions = Arc::new(
        SessionManager::new(Arc::new(SqliteMemory::open(&db).expect("memory")))
            .with_store(Arc::new(SqliteSessionStore::open(&db).expect("store")))
            .expect("stored sessions load"),
    );
    let registry = Arc::new(
        InstanceRegistry::open(dir.join("instances.json"))
            .await
            .expect("instance catalog"),
    );

    let personas = Arc::new(PersonaRegistry::default());
    personas
        .register(Persona::custom("pirate", "Pirate", "", "You talk like a pirate.").unwrap())
        .unwrap();

    let provider = Arc::new(Recorder::default());
    let agent = Arc::new(
        BuiltinAgent::builder("continuity", provider.clone())
            .memory(sessions.memory().clone())
            .session_manager(sessions.clone())
            .persona_registry(personas)
            .model("test-model")
            .build(),
    );

    let temp = tempfile::tempdir().expect("run dir");
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().to_path_buf()), None));
    std::mem::forget(temp);

    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
        .with_instances(registry.clone());

    Node {
        engine,
        provider,
        sessions,
        registry,
    }
}

fn event(id: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "user:1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: None,
    }
}

/// Sends a message and returns what the model was shown for it.
async fn say(node: &Node, id: &str, text: &str) -> Vec<ChatMessage> {
    let result = node.engine.process_event(event(id, text)).await;
    assert!(
        matches!(result, PipelineResult::LlmReplied { .. }),
        "expected a reply to '{text}', got {result:?}"
    );
    node.provider
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}

/// The user/assistant messages of a request, as plain text (the system block is left out).
fn dialogue(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role != Role::System)
        .filter_map(|message| message.content.clone())
        .collect()
}

fn draft(name: &str) -> InstanceDraft {
    InstanceDraft {
        name: name.to_string(),
        enabled: true,
        adapters: vec!["qq".to_string()],
        ..InstanceDraft::default()
    }
}

#[tokio::test]
async fn a_restarted_node_continues_the_conversation_it_was_having() {
    let dir = tempfile::tempdir().expect("dir");

    let (session, id) = {
        let node = start(dir.path()).await;
        let id = node.registry.create(draft("Test Bot")).await.unwrap().id;

        say(&node, "e1", "first").await;
        say(&node, "e2", "second").await;

        // `/new` moves the conversation to a fresh session; the old one is kept.
        let rotated = node.engine.process_event(event("e3", "/new")).await;
        let PipelineResult::SessionRotated { session_id, .. } = rotated else {
            panic!("expected a rotation, got {rotated:?}");
        };
        say(&node, "e4", "third").await;

        // An operator binds a persona to that session from the console.
        node.sessions.set_persona(&session_id, "pirate").unwrap();
        (session_id, id)
    };
    assert_eq!(session, format!("instance:{id}:group:1:user:1#1"));

    // ---- the process restarts ----
    let node = start(dir.path()).await;

    // Before anything is said, the console already lists what was there.
    let listed: Vec<String> = node
        .sessions
        .list_sessions()
        .into_iter()
        .map(|meta| meta.session_key)
        .collect();
    assert!(listed.contains(&session), "{listed:?}");
    assert!(listed.contains(&format!("instance:{id}:group:1:user:1#0")));
    assert_eq!(
        node.sessions.get_persona(&session).as_deref(),
        Some("pirate")
    );
    assert_eq!(node.sessions.get_metadata(&session).unwrap().turn_count, 1);

    let shown = say(&node, "e5", "fourth").await;

    // Same session (generation 1, not the retired generation 0), with its history and its persona.
    assert_eq!(dialogue(&shown), vec!["third", "reply 3", "fourth"]);
    assert_eq!(
        shown[0].content.as_deref(),
        Some("You talk like a pirate."),
        "the persona binding survived the restart"
    );
    assert_eq!(node.sessions.get_metadata(&session).unwrap().turn_count, 2);
}

#[tokio::test]
async fn editing_an_instance_does_not_reset_its_conversations() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let id = node.registry.create(draft("Test Bot")).await.unwrap().id;

    say(&node, "e1", "first").await;

    // The operator renames the bot, changes its reply policy and adds a platform.
    let mut edited = draft("Renamed Bot");
    edited.adapters.push("other-platform".to_string());
    edited.reply_policy = Some(kanon_core::ReplyPolicy::new(kanon_core::ReplyMode::Always));
    node.registry.update(&id, edited).await.unwrap();

    let shown = say(&node, "e2", "second").await;
    assert_eq!(
        dialogue(&shown),
        vec!["first", "reply 1", "second"],
        "the edit must not start a new conversation"
    );
    assert_eq!(
        node.sessions.session_count(),
        1,
        "no second session appeared"
    );

    // ... and it still holds after a restart that follows the edit.
    drop(node);
    let node = start(dir.path()).await;
    let shown = say(&node, "e3", "third").await;
    assert_eq!(
        dialogue(&shown),
        vec!["first", "reply 1", "second", "reply 2", "third"]
    );
    assert_eq!(node.sessions.session_count(), 1);
    assert_eq!(
        node.registry.get(&id).await.unwrap().name,
        "Renamed Bot",
        "the instance edit itself was persisted"
    );
}

#[tokio::test]
async fn a_compacted_conversation_keeps_its_summary_across_a_restart() {
    // Compaction produces the only state that is not messages: the summary. It must come back too.
    let dir = tempfile::tempdir().expect("dir");
    let key = "instance:x:group:1:user:1#0";
    {
        let node = start(dir.path()).await;
        let memory = node.sessions.memory();
        for i in 1..=3 {
            memory
                .push_message(key, ChatMessage::user(format!("q{i}")))
                .await
                .unwrap();
            memory
                .push_message(key, ChatMessage::assistant(format!("a{i}")))
                .await
                .unwrap();
        }
        memory
            .compact_history(key, 4, "they discussed q1 and q2".to_string())
            .await
            .unwrap();
    }

    let node = start(dir.path()).await;
    let snapshot = node.sessions.memory().snapshot(key).await.unwrap();
    assert_eq!(
        snapshot.summary.as_deref(),
        Some("they discussed q1 and q2")
    );
    let kept: Vec<String> = snapshot
        .messages
        .into_iter()
        .filter_map(|m| m.content)
        .collect();
    assert_eq!(kept, vec!["q3", "a3"]);
}
