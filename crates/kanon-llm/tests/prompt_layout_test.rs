//! Tests for the static-first request layout and the prefix stability it exists to provide.
//!
//! Providers cache a prompt by its prefix, so what these tests pin is not a formatting preference
//! but a cost property: from one turn to the next only the tail of the request may change.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::routing::post;
use serde_json::{Value, json};

use kanon_llm::BuiltinAgent;
use kanon_llm::agent::{Agent, AgentHook, NativeTool};
use kanon_llm::error::GatewayError;
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::providers::{
    AnthropicMessagesProvider, OpenAiChatProvider, OpenAiResponsesProvider,
};
use kanon_llm::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, ToolCall, ToolDefinition,
};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::prompt::{BASE_PERSONA_PROMPT, Persona, PersonaRegistry};
use kanon_llm::session::SessionManager;
use kanon_llm::{TurnOptions, canonical_json, canonical_tools, normalize_request};

/// Simulates a hook whose configuration changes after the turn's final request.
struct ChangingPrefix {
    changed: Arc<AtomicBool>,
    preparations: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentHook for ChangingPrefix {
    async fn on_system_prompt(
        &self,
        _session: &str,
        prompt: &mut String,
        _tools: &[ToolDefinition],
    ) -> Result<(), kanon_llm::AgentError> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        let suffix = if self.changed.load(Ordering::SeqCst) {
            "new configuration"
        } else {
            "original configuration"
        };
        prompt.push_str(&format!("\n\n{suffix}"));
        Ok(())
    }

    async fn on_llm_request(
        &self,
        _session: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let suffix = if self.changed.load(Ordering::SeqCst) {
            "new configuration"
        } else {
            "original configuration"
        };
        request.tools[0].description = suffix.to_string();
        Ok(())
    }
}

/// Changes the persona between tool rounds and the hook configuration before compaction.
struct PersonaChangeProvider {
    requests: Mutex<Vec<ChatRequest>>,
    personas: Arc<PersonaRegistry>,
    changed: Arc<AtomicBool>,
    compacted: tokio::sync::Notify,
}

#[async_trait]
impl LlmProvider for PersonaChangeProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let mut requests = self.requests.lock().unwrap();
        let round = requests.len();
        requests.push(request.clone());
        if round == 0 {
            self.personas.remove("inherited").unwrap();
            return Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "inspect".to_string(),
                    arguments: json!({}),
                }],
                finish_reason: Some("tool_calls".to_string()),
                ..ChatResponse::default()
            });
        }
        if round == 1 {
            self.changed.store(true, Ordering::SeqCst);
        } else {
            self.compacted.notify_one();
        }
        Ok(ChatResponse {
            content: Some(if round == 1 { "answer" } else { "summary" }.to_string()),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        })
    }
}

#[tokio::test]
async fn inherited_persona_and_final_hook_prefix_survive_tool_rounds_and_compaction() {
    let personas = Arc::new(PersonaRegistry::new());
    let inherited =
        Persona::custom("inherited", "Inherited", "", "Instance instructions.").unwrap();
    personas.register(inherited.clone()).unwrap();
    personas
        .register(Persona::custom("chosen", "Chosen", "", "Session instructions.").unwrap())
        .unwrap();
    let sessions = Arc::new(SessionManager::new(Arc::new(InMemory::new())));
    sessions.set_persona("s", "chosen").unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let preparations = Arc::new(AtomicUsize::new(0));
    let request_hooks = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(PersonaChangeProvider {
        requests: Mutex::new(Vec::new()),
        personas: personas.clone(),
        changed: changed.clone(),
        compacted: tokio::sync::Notify::new(),
    });
    let agent = BuiltinAgent::builder("snapshot", provider.clone())
        .session_manager(sessions.clone())
        .persona_registry(personas)
        .tool(tool("inspect"))
        .hook(ChangingPrefix {
            changed,
            preparations: preparations.clone(),
            requests: request_hooks.clone(),
        })
        .compaction(Some(kanon_llm::CompactionPolicy {
            trigger_ratio: 1.0,
            default_context_tokens: 1,
            min_messages: 2,
        }))
        .build();
    let writing = sessions.try_write("s").unwrap();
    writing
        .scope(agent.run_message_with(
            "s",
            ChatMessage::user("hello"),
            &[],
            TurnOptions {
                persona: Some(inherited),
                ..TurnOptions::default()
            },
        ))
        .await
        .unwrap();
    let later = vec![
        ChatMessage::user("later turn"),
        ChatMessage::assistant("later answer"),
    ];
    // A subsequent writer may append before the queued compaction gets the mutex. Its messages
    // belong to another prefix and must remain outside the captured summary's covered boundary.
    sessions
        .memory()
        .extend_messages("s", later.clone())
        .await
        .unwrap();
    drop(writing);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.compacted.notified(),
    )
    .await
    .unwrap();
    let _writing = sessions.write("s").await;
    assert_eq!(
        sessions
            .memory()
            .snapshot("s")
            .await
            .unwrap()
            .summary
            .as_deref(),
        Some("summary")
    );
    assert_eq!(sessions.get_persona("s").as_deref(), Some("chosen"));
    assert_eq!(
        sessions.memory().snapshot("s").await.unwrap().messages,
        later
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(preparations.load(Ordering::SeqCst), 1);
    assert_eq!(
        request_hooks.load(Ordering::SeqCst),
        2,
        "ordinary request hooks run for both tool rounds"
    );
    assert_eq!(
        requests[0].messages[0].content.as_deref(),
        Some("Instance instructions.\n\noriginal configuration")
    );
    for request in &requests[1..] {
        assert_eq!(request.messages[0], requests[0].messages[0]);
        assert_eq!(
            serde_json::to_value(&request.tools).unwrap(),
            serde_json::to_value(&requests[0].tools).unwrap()
        );
    }
    assert!(
        !requests[2]
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("later turn"))
    );
}

#[tokio::test]
async fn manual_compaction_uses_explicit_inheritance_without_changing_the_session_choice() {
    let provider = Arc::new(Recorder::default());
    let sessions = Arc::new(SessionManager::new(Arc::new(InMemory::new())));
    let personas = Arc::new(PersonaRegistry::new());
    personas
        .register(Persona::custom("chosen", "Chosen", "", "Session instructions.").unwrap())
        .unwrap();
    sessions.set_persona("s", "chosen").unwrap();
    let agent = BuiltinAgent::builder("manual", provider.clone())
        .session_manager(sessions.clone())
        .persona_registry(personas)
        .tool(tool("inspect"))
        .compaction(None)
        .build();
    let options = TurnOptions {
        persona: Some(
            Persona::custom("inherited", "Inherited", "", "Instance instructions.").unwrap(),
        ),
        without_tools: true,
        ..TurnOptions::default()
    };
    for message in ["first", "second"] {
        agent
            .run_message_with("s", ChatMessage::user(message), &[], options.clone())
            .await
            .unwrap();
    }
    assert!(agent.compact_session_with("s", &[], options).await.unwrap());
    assert_eq!(sessions.get_persona("s").as_deref(), Some("chosen"));
    agent.run("s", "third", &[]).await.unwrap();
    let requests = provider.requests.lock().unwrap();
    for request in &requests[..3] {
        assert_eq!(
            request.messages[0].content.as_deref(),
            Some("Instance instructions.")
        );
        assert!(request.tools.is_empty());
    }
    assert!(
        requests[3].messages[0]
            .content
            .as_deref()
            .unwrap()
            .starts_with("Session instructions.\n\n")
    );
}

/// Host-owned runtime status enriches the originating turn, never tools or the system block.
struct Availability(Arc<AtomicBool>);

#[async_trait]
impl AgentHook for Availability {
    async fn on_user_message(
        &self,
        _session: &str,
        message: &mut ChatMessage,
    ) -> Result<(), kanon_llm::AgentError> {
        message.content.get_or_insert_default().push_str(&format!(
            "\n\nbash available: {}",
            self.0.load(Ordering::SeqCst)
        ));
        Ok(())
    }
}

#[tokio::test]
async fn runtime_tool_permission_changes_only_the_request_tail() {
    let recorder = Arc::new(Recorder::default());
    let allowed = Arc::new(AtomicBool::new(true));
    let agent = BuiltinAgent::builder("permission-prefix", recorder.clone())
        .model("test")
        .system_prompt("Fixed persona")
        .tool(tool("bash"))
        .hook(Availability(allowed.clone()))
        .build();
    agent.run("same-message-a", "hello", &[]).await.unwrap();
    allowed.store(false, Ordering::SeqCst);
    agent.run("same-message-a", "hello", &[]).await.unwrap();
    let requests = recorder.requests.lock().unwrap();
    assert_eq!(
        serde_json::to_string(&requests[0].tools).unwrap(),
        serde_json::to_string(&requests[1].tools).unwrap()
    );
    let n = requests[0].messages.len();
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1,
        "availability belongs inside the originating user turn"
    );
    assert_eq!(
        serde_json::to_string(&requests[0].messages[..n - 1]).unwrap(),
        serde_json::to_string(&requests[1].messages[..n - 1]).unwrap()
    );
    assert_ne!(
        requests[0].messages.last().unwrap().content,
        requests[1].messages.last().unwrap().content
    );
    assert_eq!(
        &requests[1].messages[..n],
        requests[0].messages.as_slice(),
        "the changed status must not rewrite any earlier message"
    );
    assert!(requests[1].tools.iter().any(|tool| tool.name == "bash"));
}

/// Provider that records every request it receives.
#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl LlmProvider for Recorder {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let mut requests = self.requests.lock().expect("recorder lock");
        let turn = requests.len();
        requests.push(request.clone());
        Ok(ChatResponse {
            content: Some(format!("reply {turn}")),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        })
    }
}

/// A tool whose schema keys are written in reverse-alphabetical order on purpose.
fn tool(name: &str) -> NativeTool {
    let schema: Value = serde_json::from_str(
        r#"{"type":"object","required":["b","a"],"properties":{"z":{"type":"string"},"a":{"type":"integer","description":"d"}}}"#,
    )
    .expect("schema");
    NativeTool::new(
        ToolDefinition {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: schema,
        },
        |_session, _args| async { Ok("ok".to_string()) },
    )
}

/// Builds an agent over a recorder with tools registered in the given order.
fn agent_with_tools(recorder: Arc<Recorder>, order: &[&str]) -> BuiltinAgent {
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let mut builder = BuiltinAgent::builder("layout", recorder)
        .memory(memory)
        .session_manager(sessions)
        .persona_registry(Arc::new(PersonaRegistry::default()))
        .model("test-model");
    for name in order {
        builder = builder.tool(tool(name));
    }
    builder.build()
}

#[test]
fn canonical_json_sorts_every_object_and_keeps_array_order() {
    let messy: Value =
        serde_json::from_str(r#"{"b":[{"z":1,"a":2},3],"a":{"y":true,"x":null}}"#).expect("json");
    let canonical = canonical_json(messy);

    assert_eq!(
        canonical.to_string(),
        r#"{"a":{"x":null,"y":true},"b":[{"a":2,"z":1},3]}"#
    );
}

#[test]
fn canonical_tools_are_sorted_by_name_whatever_the_input_order() {
    let make = |name: &str| ToolDefinition {
        name: name.to_string(),
        description: String::new(),
        parameters: json!({"type": "object"}),
    };
    let tools = canonical_tools(vec![
        make("read_skill"),
        make("alpha__zap"),
        make("alpha__add"),
    ]);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["alpha__add", "alpha__zap", "read_skill"]);
}

#[tokio::test]
async fn the_tool_list_is_byte_identical_whatever_the_registration_order() {
    let first = Arc::new(Recorder::default());
    let second = Arc::new(Recorder::default());
    let forward = agent_with_tools(first.clone(), &["alpha", "beta", "gamma"]);
    let backward = agent_with_tools(second.clone(), &["gamma", "beta", "alpha"]);

    forward.run("s", "hello", &[]).await.expect("forward run");
    backward.run("s", "hello", &[]).await.expect("backward run");

    let a = serde_json::to_string(&first.requests.lock().unwrap()[0].tools).expect("json");
    let b = serde_json::to_string(&second.requests.lock().unwrap()[0].tools).expect("json");
    assert_eq!(
        a, b,
        "registration order must not reshape the top of the prompt"
    );

    let tools = first.requests.lock().unwrap()[0].tools.clone();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    // Sorted keys, all the way down.
    assert_eq!(
        tools[0].parameters.to_string(),
        r#"{"properties":{"a":{"description":"d","type":"integer"},"z":{"type":"string"}},"required":["b","a"],"type":"object"}"#
    );
}

#[tokio::test]
async fn each_request_extends_the_previous_one_and_only_the_tail_changes() {
    let recorder = Arc::new(Recorder::default());
    let agent = agent_with_tools(recorder.clone(), &["alpha", "beta"]);

    for turn in 1..=4 {
        agent
            .run("session", &format!("message {turn}"), &[])
            .await
            .expect("turn");
    }

    let requests = recorder.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);

    for pair in requests.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);

        // Layer 1: the tools never change between turns.
        assert_eq!(
            serde_json::to_string(&before.tools).unwrap(),
            serde_json::to_string(&after.tools).unwrap()
        );
        // Layer 2: neither does the system block.
        assert_eq!(before.messages[0], after.messages[0]);
        assert_eq!(before.messages[0].role, Role::System);
        // Layers 3-4: the whole previous request is an exact prefix of the next one; the next one
        // only appends the assistant reply and the new user message.
        assert_eq!(
            &after.messages[..before.messages.len()],
            before.messages.as_slice(),
            "history must be append-only"
        );
        assert_eq!(after.messages.len(), before.messages.len() + 2);
    }
}

/// Calls the `alpha` tool when a request ends with the user's message and answers otherwise.
#[derive(Default)]
struct ToolThenAnswer {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl LlmProvider for ToolThenAnswer {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.messages.last().map(|message| message.role) == Some(Role::User) {
            return Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: format!("call-{}", request.messages.len()),
                    name: "alpha".into(),
                    arguments: json!({}),
                }],
                ..ChatResponse::default()
            });
        }
        Ok(ChatResponse {
            content: Some("done".into()),
            finish_reason: Some("stop".into()),
            ..ChatResponse::default()
        })
    }
}

#[tokio::test]
async fn a_turns_images_go_out_with_that_turn_only() {
    // Platform image URLs are signed and expire. Re-sent with every later request, one the
    // provider can no longer download fails the whole session; history keeps the words only.
    let model = Arc::new(ToolThenAnswer::default());
    let memory = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder("media", model.clone())
        .memory(memory.clone())
        .model("test")
        .tool(tool("alpha"))
        .build();
    let image = ContentPart::image_url("https://cdn.example/a.png?rkey=signed", None);
    agent
        .run_message(
            "session",
            ChatMessage::user_multimodal("[图片] what is this", vec![image.clone()]),
            &[],
        )
        .await
        .unwrap();
    agent.run("session", "and now?", &[]).await.unwrap();

    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4, "each turn is a tool call and an answer");
    // The model sees the picture in every request of its turn, tool loop included.
    for request in &requests[..2] {
        let user = request.messages.iter().find(|m| m.role == Role::User);
        assert_eq!(user.unwrap().parts.as_ref(), Some(&vec![image.clone()]));
    }
    let history = memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history[0].content.as_deref(), Some("[图片] what is this"));
    assert!(history.iter().all(|message| message.parts.is_none()));

    // The next turn re-sends the previous request without the picture, then only appends; from
    // there on each request extends the one before exactly.
    let mut previous = requests[1].messages.clone();
    for message in &mut previous {
        message.parts = None;
    }
    assert_eq!(&requests[2].messages[..previous.len()], previous.as_slice());
    assert_eq!(
        &requests[3].messages[..requests[2].messages.len()],
        requests[2].messages.as_slice()
    );
    assert!(
        requests[2..]
            .iter()
            .flat_map(|request| &request.messages)
            .all(|message| message.parts.is_none())
    );
}

#[tokio::test]
async fn the_request_reads_static_to_dynamic() {
    let recorder = Arc::new(Recorder::default());
    let agent = agent_with_tools(recorder.clone(), &["alpha"]);

    agent.run("session", "first", &[]).await.expect("turn 1");
    agent.run("session", "second", &[]).await.expect("turn 2");

    let request = recorder.requests.lock().unwrap()[1].clone();
    let roles: Vec<Role> = request.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::System, Role::User, Role::Assistant, Role::User],
        "system block, then history, then the current turn"
    );
    assert_eq!(
        request.messages[0].content.as_deref(),
        Some(BASE_PERSONA_PROMPT)
    );
    assert_eq!(
        request.messages.last().and_then(|m| m.content.as_deref()),
        Some("second"),
        "the user's message is the very last thing in the request"
    );
}

/// Adds two system messages the way hooks do, with untidy whitespace.
struct SloppyHooks;

#[async_trait]
impl AgentHook for SloppyHooks {
    async fn on_llm_request(
        &self,
        _session_id: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        request
            .messages
            .insert(1, ChatMessage::system("  catalog line\n\n"));
        request.messages.insert(2, ChatMessage::system("   "));
        Ok(())
    }
}

#[tokio::test]
async fn the_static_block_is_one_trimmed_message_however_many_hooks_contribute() {
    let recorder = Arc::new(Recorder::default());
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let agent = BuiltinAgent::builder("layout", recorder.clone())
        .memory(memory)
        .session_manager(sessions)
        .persona_registry(Arc::new(PersonaRegistry::default()))
        .hook(SloppyHooks)
        .build();

    agent.run("s", "hi", &[]).await.expect("run");

    let request = recorder.requests.lock().unwrap()[0].clone();
    assert_eq!(
        request.messages.len(),
        2,
        "one system block + the user turn"
    );
    assert_eq!(
        request.messages[0].content.as_deref(),
        Some(format!("{BASE_PERSONA_PROMPT}\n\ncatalog line").as_str()),
        "parts are trimmed, blanks dropped, joined with one fixed separator"
    );
}

/// Replaces the whole system block, the way a plugin's `OnLlmRequest` rewrite is applied.
struct Rewriter;

#[async_trait]
impl AgentHook for Rewriter {
    async fn on_llm_request(
        &self,
        _session_id: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        let text = kanon_llm::layout::system_text(&request.messages);
        let leading = request
            .messages
            .iter()
            .take_while(|message| message.role == Role::System)
            .count();
        request.messages.drain(..leading);
        request
            .messages
            .insert(0, ChatMessage::system(format!("{text}\n\nrewritten")));
        Ok(())
    }
}

#[tokio::test]
async fn a_rewritten_system_prompt_is_one_stable_block_with_the_summary_after_it() {
    let recorder = Arc::new(Recorder::default());
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let agent = BuiltinAgent::builder("rewrite", recorder.clone())
        .memory(memory.clone())
        .session_manager(sessions)
        .persona_registry(Arc::new(PersonaRegistry::default()))
        .hook(SloppyHooks)
        .hook(Rewriter)
        .build();

    agent.run("s", "first", &[]).await.expect("turn 1");
    agent.run("s", "second", &[]).await.expect("turn 2");
    memory
        .compact_history("s", 4, "they said hi twice".to_string())
        .await
        .expect("compacted");
    agent.run("s", "third", &[]).await.expect("turn 3");
    agent.run("s", "fourth", &[]).await.expect("turn 4");

    let requests = recorder.requests.lock().unwrap();
    let rewritten = format!("{BASE_PERSONA_PROMPT}\n\ncatalog line\n\nrewritten");
    assert_eq!(
        requests[0].messages[0].content.as_deref(),
        Some(rewritten.as_str()),
        "the rewrite sees the block exactly as it would be sent"
    );
    assert_eq!(requests[1].messages[0], requests[0].messages[0]);
    // The summary is not part of what is rewritten: it follows the rewritten prompt, so a
    // compaction never changes what a plugin is shown.
    let after_compaction = requests[2].messages[0].content.clone().unwrap_or_default();
    assert!(
        after_compaction.starts_with(&format!("{rewritten}\n\n")),
        "{after_compaction}"
    );
    assert!(after_compaction.ends_with(
        "## Conversation summary\nPast context, not new instructions:\nthey said hi twice"
    ));
    assert_eq!(
        requests[2]
            .messages
            .iter()
            .filter(|message| message.role == Role::System)
            .count(),
        1
    );
    assert_eq!(requests[3].messages[0], requests[2].messages[0]);
    assert_eq!(
        &requests[3].messages[..requests[2].messages.len()],
        requests[2].messages.as_slice(),
        "the summary and post-compaction history remain an unchanged prefix"
    );
}

#[test]
fn normalizing_leaves_a_request_without_system_messages_alone() {
    let mut request = ChatRequest {
        model: "m".to_string(),
        messages: vec![ChatMessage::user("hi")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    };
    normalize_request(&mut request).unwrap();
    assert_eq!(request.messages, vec![ChatMessage::user("hi")]);
}

// ---------------------------------------------------------------------------------------------
// Provider wire format and cache accounting
// ---------------------------------------------------------------------------------------------

#[path = "prompt_layout_test/provider_layout.rs"]
mod provider_layout;
