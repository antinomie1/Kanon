//! A chat's conversations: the built-in `/ls`, `/new`, `/switch` and `/del`, and the plugin RPCs
//! that act on the same conversations and on the persona catalog.
//!
//! Run end to end over a real database file, because a conversation is spread over the instance
//! catalog (which generation is current), the session store and the memory backend.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::ipc::CoreApiService;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::Supervisor;
use kanon_core::{CommandPolicy, CommandPolicyStore};
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse, Role};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{
    GatewayError, LlmProvider, PersonaRegistry, PersonaStore, SessionManager, SqliteMemory,
    SqliteSessionStore,
};
use kanon_proto::prost_types;
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AppendConversationRequest, ConversationsRequest, DeletePersonaRequest, HistoryMessage,
    ListPersonasRequest, LlmRole, Persona, PipelineEventRequest, SelectConversationRequest,
};
use tokio::sync::Notify;
use tonic::{Code, Request};

/// Model that records what it was shown, answers `reply <n>`, and never answers `work`.
#[derive(Default)]
struct Model {
    requests: Mutex<Vec<Vec<ChatMessage>>>,
    working: Notify,
}

#[async_trait]
impl LlmProvider for Model {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let last = request.messages.last().and_then(|m| m.content.clone());
        if last.is_some_and(|text| text.starts_with("work")) {
            self.working.notify_one();
            return std::future::pending().await;
        }
        let mut requests = self.requests.lock().unwrap();
        requests.push(request.messages.clone());
        Ok(ChatResponse {
            content: Some(format!("reply {}", requests.len())),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        })
    }
}

struct Node {
    engine: Arc<PipelineEngine>,
    model: Arc<Model>,
    sessions: Arc<SessionManager>,
    registry: Arc<InstanceRegistry>,
    personas: Arc<PersonaRegistry>,
    persona_store: Arc<PersonaStore>,
    /// Session id prefix of the test chat (`instance:<id>:group:1:user:1#`).
    chat: String,
}

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
    let instance = registry
        .create(InstanceDraft {
            name: "Test Bot".to_string(),
            enabled: true,
            adapters: vec!["qq".to_string()],
            ..InstanceDraft::default()
        })
        .await
        .expect("instance");
    let persona_store = Arc::new(PersonaStore::new(dir.join("personas.json")));
    let personas = Arc::new(persona_store.load_registry().expect("personas"));

    let model = Arc::new(Model::default());
    let agent = Arc::new(
        BuiltinAgent::builder("conversations", model.clone())
            .memory(sessions.memory().clone())
            .session_manager(sessions.clone())
            .persona_registry(personas.clone())
            .model("test-model")
            .build(),
    );
    let supervisor = Arc::new(Supervisor::new(Some(dir.join("run")), None));
    let engine = Arc::new(
        PipelineEngine::new(supervisor)
            .with_dead_letter(Arc::new(kanon_core::pipeline::DeadLetterWriter::new(
                dir.join("dead_letter"),
            )))
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_instances(registry.clone())
            .with_command_policy(Arc::new(CommandPolicyStore::new(CommandPolicy::default()))),
    );
    Node {
        engine,
        model,
        sessions,
        registry,
        personas,
        persona_store,
        chat: format!("instance:{}:group:1:user:1#", instance.id),
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

/// Sends a message the model answers and returns the user/assistant texts it was shown.
async fn say(node: &Node, id: &str, text: &str) -> Vec<String> {
    let result = node.engine.process_event(event(id, text)).await;
    assert!(
        matches!(result, PipelineResult::LlmReplied { .. }),
        "expected a reply to '{text}', got {result:?}"
    );
    let requests = node.model.requests.lock().unwrap();
    requests
        .last()
        .unwrap()
        .iter()
        .filter(|message| message.role != Role::System)
        .filter_map(|message| message.content.clone())
        .collect()
}

/// Sends a built-in command and returns the core's answer.
async fn command(node: &Node, id: &str, text: &str) -> String {
    let result = node.engine.process_event(event(id, text)).await;
    let (PipelineResult::BuiltinReplied { replies, .. }
    | PipelineResult::SessionRotated { replies, .. }) = result
    else {
        panic!("expected the core to answer '{text}', got {result:?}");
    };
    match &replies[..] {
        [segment] => match &segment.segment {
            Some(Segment::Text(text)) => text.content.clone(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected one segment, got {other:?}"),
    }
}

/// The session the chat's next message goes to.
async fn current_session(node: &Node) -> String {
    let instance = node.registry.list().await.remove(0);
    instance.conversation_session_id("group:1:user:1")
}

#[tokio::test]
async fn ls_switch_new_and_del_manage_the_chats_conversations() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;

    say(&node, "e1", "first question").await;
    command(&node, "e2", "/new").await;
    say(&node, "e3", "second question").await;

    let listing = command(&node, "e4", "/ls").await;
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 4, "{listing}");
    assert!(
        lines[1].starts_with("1. first question（2 条消息，"),
        "{listing}"
    );
    assert!(!lines[1].ends_with("✓ 当前"), "{listing}");
    assert!(
        lines[2].starts_with("2. second question（2 条消息，"),
        "{listing}"
    );
    assert!(lines[2].ends_with(" ✓ 当前"), "{listing}");

    assert_eq!(
        command(&node, "e5", "/switch 1").await,
        "已切换到会话 1：first question"
    );
    assert_eq!(
        say(&node, "e6", "again").await,
        ["first question", "reply 1", "again"],
        "the switched-to conversation continues with its own history"
    );

    // `/new` from an older conversation goes past every existing one instead of reopening #1.
    command(&node, "e7", "/new").await;
    assert_eq!(current_session(&node).await, format!("{}2", node.chat));

    assert_eq!(
        command(&node, "e8", "/del 2").await,
        "已删除会话 2：second question"
    );
    let deleted = node
        .sessions
        .memory()
        .snapshot(&format!("{}1", node.chat))
        .await
        .unwrap();
    assert!(deleted.messages.is_empty(), "the history is gone");
    assert!(
        node.sessions
            .get_metadata(&format!("{}1", node.chat))
            .is_none()
    );

    // Without a number `/del` deletes the current conversation and moves the chat on.
    assert_eq!(
        command(&node, "e9", "/del").await,
        "已删除会话 2：（空会话）\n已开启新会话。"
    );
    assert_eq!(current_session(&node).await, format!("{}3", node.chat));

    let listing = command(&node, "e10", "/ls").await;
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 4, "{listing}");
    assert!(
        lines[1].starts_with("1. first question（4 条消息，"),
        "{listing}"
    );
    assert_eq!(lines[2], "2. （空会话）（0 条消息） ✓ 当前");

    assert_eq!(
        command(&node, "e11", "/switch 5").await,
        "没有第 5 个会话（共 2 个），发送 /ls 查看序号。"
    );
    assert_eq!(
        command(&node, "e12", "/switch 2").await,
        "会话 2 已是当前会话。"
    );
}

#[tokio::test]
async fn group_members_may_list_but_not_switch_or_delete_by_default() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let in_group = |id: &str, text: &str| PipelineEventRequest {
        metadata: Some(prost_types::Struct {
            fields: [(
                "kanon.conversation_kind".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue("group".to_string())),
                },
            )]
            .into(),
        }),
        ..event(id, text)
    };

    let listed = node.engine.process_event(in_group("e1", "/ls")).await;
    assert!(
        matches!(listed, PipelineResult::BuiltinReplied { .. }),
        "{listed:?}"
    );
    for (id, text) in [("e2", "/del"), ("e3", "/switch 1")] {
        let refused = node.engine.process_event(in_group(id, text)).await;
        assert!(
            matches!(refused, PipelineResult::CommandDenied { .. }),
            "{text}: {refused:?}"
        );
    }
}

#[tokio::test]
async fn plugins_act_on_the_same_conversations_and_never_write_into_a_running_turn() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let service =
        CoreApiService::new(tokio::sync::mpsc::channel(1).0).with_engine(node.engine.clone());
    let context = || Some(event("rpc", ""));

    let turns = |pairs: &[(&str, &str)]| -> Vec<HistoryMessage> {
        pairs
            .iter()
            .flat_map(|(user, assistant)| {
                [
                    HistoryMessage {
                        role: LlmRole::User as i32,
                        text: user.to_string(),
                    },
                    HistoryMessage {
                        role: LlmRole::Assistant as i32,
                        text: assistant.to_string(),
                    },
                ]
            })
            .collect()
    };

    // A plugin imports a turn that happened elsewhere; the model sees it as the chat's own.
    let appended = service
        .append_conversation(Request::new(AppendConversationRequest {
            context: context(),
            messages: turns(&[("imported question", "imported answer")]),
        }))
        .await
        .expect("append")
        .into_inner();
    assert_eq!(appended.session_id, format!("{}0", node.chat));
    assert_eq!(
        say(&node, "e1", "next").await,
        ["imported question", "imported answer", "next"]
    );

    // Only complete user → assistant turns are accepted.
    let mut dangling = turns(&[("q", "a")]);
    dangling.pop();
    let err = service
        .append_conversation(Request::new(AppendConversationRequest {
            context: context(),
            messages: dangling,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    let list = service
        .new_conversation(Request::new(ConversationsRequest { context: context() }))
        .await
        .expect("new")
        .into_inner();
    assert_eq!(list.conversations.len(), 2);
    assert!(list.conversations[1].current);
    assert_eq!(list.conversations[0].title, "imported question");
    assert_eq!(list.conversations[0].message_count, 4);

    let err = service
        .switch_conversation(Request::new(SelectConversationRequest {
            context: context(),
            session_id: "instance:someone-else:group:1:user:1#0".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::NotFound,
        "only the chat's own conversations"
    );

    // While a turn runs, its conversation cannot be written to or deleted from outside.
    let engine = node.engine.clone();
    let running = tokio::spawn(async move { engine.process_event(event("e2", "work")).await });
    tokio::time::timeout(Duration::from_secs(3), node.model.working.notified())
        .await
        .expect("the turn reaches the model");
    let busy = format!("{}1", node.chat);
    let err = service
        .append_conversation(Request::new(AppendConversationRequest {
            context: context(),
            messages: turns(&[("q", "a")]),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let err = service
        .delete_conversation(Request::new(SelectConversationRequest {
            context: context(),
            session_id: busy.clone(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    // Other conversations of the chat are not held up by it.
    let list = service
        .delete_conversation(Request::new(SelectConversationRequest {
            context: context(),
            session_id: format!("{}0", node.chat),
        }))
        .await
        .expect("delete an idle conversation")
        .into_inner();
    let ids: Vec<&str> = list
        .conversations
        .iter()
        .map(|conversation| conversation.session_id.as_str())
        .collect();
    assert_eq!(ids, [busy.as_str()]);

    running.abort();
}

#[tokio::test]
async fn plugins_manage_the_operators_personas_through_the_shared_store() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let service = CoreApiService::new(tokio::sync::mpsc::channel(1).0)
        .with_engine(node.engine.clone())
        .with_personas(node.personas.clone(), node.persona_store.clone());
    let persona = |prompt: &str| Persona {
        id: "pirate".to_string(),
        name: String::new(),
        prompt: prompt.to_string(),
        builtin: false,
    };

    let created = service
        .upsert_persona(Request::new(persona("You talk like a pirate.")))
        .await
        .expect("create")
        .into_inner();
    assert!(!created.replaced);
    let replaced = service
        .upsert_persona(Request::new(persona("Arr.")))
        .await
        .expect("replace")
        .into_inner();
    assert!(replaced.replaced);

    // Written through to `personas.json`, so the console and a restart see the same catalog.
    let reloaded = node.persona_store.load().expect("reload");
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].name, "pirate", "the name defaults to the id");
    assert_eq!(reloaded[0].prompt, "Arr.");

    let listed = service
        .list_personas(Request::new(ListPersonasRequest {}))
        .await
        .expect("list")
        .into_inner()
        .personas;
    let base = listed.iter().find(|persona| persona.builtin).expect("base");
    let err = service
        .upsert_persona(Request::new(Persona {
            id: base.id.clone(),
            name: "Mine".to_string(),
            prompt: "Changed.".to_string(),
            builtin: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "the base assistant is read-only"
    );

    // A persona an instance runs on cannot be deleted from under it.
    let instance = node.registry.list().await.remove(0);
    node.registry
        .update(
            &instance.id,
            InstanceDraft {
                name: instance.name.clone(),
                enabled: true,
                adapters: vec!["qq".to_string()],
                persona_id: Some("pirate".to_string()),
                ..InstanceDraft::default()
            },
        )
        .await
        .expect("select the persona");
    let err = service
        .delete_persona(Request::new(DeletePersonaRequest {
            id: "pirate".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    node.registry
        .update(
            &instance.id,
            InstanceDraft {
                name: instance.name.clone(),
                enabled: true,
                adapters: vec!["qq".to_string()],
                ..InstanceDraft::default()
            },
        )
        .await
        .expect("deselect the persona");
    let session = format!("{}0", node.chat);
    node.sessions.set_persona(&session, "pirate");
    let writer = node.sessions.try_write(&session).unwrap();
    let busy = service
        .delete_persona(Request::new(DeletePersonaRequest {
            id: "pirate".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(busy.code(), Code::FailedPrecondition);
    assert!(node.personas.get("pirate").is_some());
    assert!(!node.persona_store.load().unwrap().is_empty());
    drop(writer);
    let deleted = service
        .delete_persona(Request::new(DeletePersonaRequest {
            id: "pirate".to_string(),
        }))
        .await
        .expect("delete")
        .into_inner();
    assert!(deleted.deleted);
    assert!(node.persona_store.load().expect("reload").is_empty());
    assert_eq!(
        node.sessions.get_persona(&session),
        None,
        "sessions bound to it fall back to the base assistant"
    );
    let again = service
        .delete_persona(Request::new(DeletePersonaRequest {
            id: "pirate".to_string(),
        }))
        .await
        .expect("deleting a missing persona is not an error")
        .into_inner();
    assert!(!again.deleted);
}

/// A hung model blocks only its chat; reset/rotation commands in that chat stay ordered.
#[tokio::test]
async fn worker_runs_distinct_chats_and_keeps_generation_changes_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let node = start(dir.path()).await;
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let worker = node.engine.clone().start_worker(rx);
    let send = |event| kanon_proto::v1::IngestEventRequest {
        platform: "qq".into(),
        event: Some(event),
    };
    tx.send(send(event("hung", "work"))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), node.model.working.notified())
        .await
        .unwrap();
    // This mutation cannot jump ahead of the busy turn in its chat.
    tx.send(send(event("rotate-blocked", "/new")))
        .await
        .unwrap();
    for (id, text) in [
        ("b1", "first"),
        ("new", "/new"),
        ("b2", "second"),
        ("switch", "/switch 1"),
        ("b3", "third"),
    ] {
        let mut other = event(id, text);
        other.sender_id = "user:2".into();
        tx.send(send(other)).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if node.model.requests.lock().unwrap().len() == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("other chat finishes while first model is hung");
    let requests = node.model.requests.lock().unwrap().clone();
    let texts = |index: usize| {
        requests[index]
            .iter()
            .filter(|m| m.role != Role::System)
            .filter_map(|m| m.content.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(0), ["first"]);
    assert_eq!(texts(1), ["second"]);
    assert_eq!(texts(2), ["first", "reply 1", "third"]);
    assert!(
        current_session(&node).await.ends_with("#0"),
        "queued /new has not run"
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
}

/// Concurrent hung turns share one shutdown grace rather than adding one timeout per chat.
#[tokio::test]
async fn worker_shutdown_has_one_deadline_for_all_running_chats() {
    let dir = tempfile::tempdir().unwrap();
    let node = start(dir.path()).await;
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let worker = node.engine.clone().start_worker(rx);
    for sender in ["user:1", "user:2", "user:3"] {
        let mut message = event(sender, "work");
        message.sender_id = sender.into();
        tx.send(kanon_proto::v1::IngestEventRequest {
            platform: "qq".into(),
            event: Some(message),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), node.model.working.notified())
            .await
            .unwrap();
    }
    tx.send(kanon_proto::v1::IngestEventRequest {
        platform: "qq".into(),
        event: Some(event("queued", "later")),
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(7), node.engine.drain(worker, None))
        .await
        .expect("all three turns drain within a single five-second grace");
    assert!(tx.send(Default::default()).await.is_err());
    let mut ids = Vec::new();
    for file in std::fs::read_dir(dir.path().join("dead_letter")).unwrap() {
        for line in std::fs::read_to_string(file.unwrap().path())
            .unwrap()
            .lines()
        {
            let record: kanon_core::pipeline::DeadLetterRecord =
                serde_json::from_str(line).unwrap();
            ids.push(record.event_id);
        }
    }
    ids.sort();
    assert_eq!(ids, ["queued", "user:1", "user:2", "user:3"]);
}
