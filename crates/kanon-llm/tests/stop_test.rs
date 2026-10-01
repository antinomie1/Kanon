//! A turn that ends without an answer, stopped or failed, leaves a history the provider still
//! accepts and that closes the turn, so the next message is answered on its own instead of
//! restarting the work that just failed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use kanon_llm::BuiltinAgent;
use kanon_llm::STOPPED_TOOL_RESULT;
use kanon_llm::agent::{Agent, NativeTool};
use kanon_llm::error::GatewayError;
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, Role, ToolCall, ToolDefinition,
};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::{AgentError, StopSignal, with_stop_signal};

/// Model whose behaviour is chosen by the user's words: `hang` never answers, `tools` asks for two
/// slow tool calls, anything else (and every tool-result round) answers at once.
struct Scripted {
    waiting: Arc<Notify>,
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let last = request.messages.last().expect("request has messages");
        match (last.role, last.content.as_deref()) {
            (Role::User, Some("hang")) => {
                self.waiting.notify_one();
                std::future::pending().await
            }
            (Role::User, Some("tools")) => Ok(ChatResponse {
                tool_calls: ["call-a", "call-b"]
                    .map(|id| ToolCall {
                        id: id.into(),
                        name: "slow".into(),
                        arguments: json!({}),
                    })
                    .into(),
                ..ChatResponse::default()
            }),
            _ => Ok(ChatResponse {
                content: Some("done".into()),
                finish_reason: Some("stop".into()),
                ..ChatResponse::default()
            }),
        }
    }
}

/// Sets its flag when dropped, which is how an abandoned tool future shows it was cancelled.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct Fixture {
    agent: BuiltinAgent,
    memory: Arc<InMemory>,
    waiting: Arc<Notify>,
    tool_dropped: Arc<AtomicBool>,
}

fn fixture() -> Fixture {
    let waiting = Arc::new(Notify::new());
    let tool_dropped = Arc::new(AtomicBool::new(false));
    let slow = {
        let waiting = waiting.clone();
        let dropped = tool_dropped.clone();
        NativeTool::new(
            ToolDefinition {
                name: "slow".into(),
                description: "never finishes".into(),
                parameters: json!({"type": "object", "properties": {}}),
            },
            move |_session, _args| {
                let guard = SetOnDrop(dropped.clone());
                let waiting = waiting.clone();
                async move {
                    let _guard = guard;
                    waiting.notify_one();
                    std::future::pending().await
                }
            },
        )
    };
    let memory = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder(
        "stop",
        Arc::new(Scripted {
            waiting: waiting.clone(),
        }),
    )
    .memory(memory.clone())
    .model("test")
    .tool(slow)
    .build();
    Fixture {
        agent,
        memory,
        waiting,
        tool_dropped,
    }
}

/// Runs `text` as a turn and stops it once the model or a tool starts waiting.
async fn run_and_stop(fixture: &Fixture, text: &str) -> Result<(), AgentError> {
    let signal = StopSignal::new();
    let stopper = {
        let signal = signal.clone();
        let waiting = fixture.waiting.clone();
        tokio::spawn(async move {
            waiting.notified().await;
            signal.stop();
        })
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        with_stop_signal(signal, fixture.agent.run("session", text, &[])),
    )
    .await
    .expect("a stopped turn ends promptly");
    stopper.await.unwrap();
    outcome.map(|_| ())
}

#[tokio::test]
async fn stopping_a_tool_round_answers_every_call_and_cancels_the_tool() {
    let fixture = fixture();
    let outcome = run_and_stop(&fixture, "tools").await;
    assert!(matches!(outcome, Err(AgentError::Stopped)), "{outcome:?}");
    assert!(
        fixture.tool_dropped.load(Ordering::SeqCst),
        "the running tool call is abandoned, not left running"
    );

    // Providers reject a tool call without a result, so the call that was running and the one
    // that never started both get one.
    let history = fixture.memory.snapshot("session").await.unwrap().messages;
    let roles: Vec<Role> = history.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::Tool,
            Role::Assistant
        ],
        "{history:?}"
    );
    for (message, id) in history[2..4].iter().zip(["call-a", "call-b"]) {
        assert_eq!(message.tool_call_id.as_deref(), Some(id));
        assert_eq!(message.content.as_deref(), Some(STOPPED_TOOL_RESULT));
    }
    assert_closed(&history[4], "was stopped by the user");

    // The session keeps working: the next turn sees the closed round and appends to it.
    let reply = fixture.agent.run("session", "again", &[]).await.unwrap();
    assert_eq!(reply.content, "done");
    let history = fixture.memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history.len(), 7);
    assert_eq!(history[5].content.as_deref(), Some("again"));
}

/// The closing note of an unanswered turn says what happened and that nothing retried it.
fn assert_closed(message: &ChatMessage, what: &str) {
    assert_eq!(message.role, Role::Assistant);
    assert!(message.tool_calls.is_none());
    let note = message.content.as_deref().unwrap_or_default();
    assert!(note.contains(what), "{note}");
    assert!(note.contains("It was not retried"), "{note}");
}

#[tokio::test]
async fn stopping_a_model_wait_keeps_the_question_and_closes_it() {
    let fixture = fixture();
    let outcome = run_and_stop(&fixture, "hang").await;
    assert!(matches!(outcome, Err(AgentError::Stopped)), "{outcome:?}");

    let history = fixture.memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history.len(), 2, "{history:?}");
    assert_eq!(history[0].content.as_deref(), Some("hang"));
    assert_closed(&history[1], "was stopped by the user");

    let reply = fixture.agent.run("session", "again", &[]).await.unwrap();
    assert_eq!(reply.content, "done");
}

/// Model that starts a long job with a tool call, then fails the request that would continue it,
/// the way a provider gives up on an answer that takes too long to write. Later questions are
/// answered at once.
#[derive(Default)]
struct FailsMidJob {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl LlmProvider for FailsMidJob {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        let last = request.messages.last().expect("request has messages");
        match (last.role, last.content.as_deref()) {
            (Role::User, Some("make music")) => Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "check".into(),
                    name: "quick".into(),
                    arguments: json!({}),
                }],
                ..ChatResponse::default()
            }),
            (Role::Tool, _) => Err(GatewayError::ApiStatus {
                status: 504,
                message: "upstream timed out".into(),
            }),
            _ => Ok(ChatResponse {
                content: Some("here is a picture".into()),
                finish_reason: Some("stop".into()),
                ..ChatResponse::default()
            }),
        }
    }
}

#[tokio::test]
async fn a_failed_turn_is_closed_so_the_next_message_does_not_restart_it() {
    let model = Arc::new(FailsMidJob::default());
    let memory = Arc::new(InMemory::new());
    let quick = NativeTool::new(
        ToolDefinition {
            name: "quick".into(),
            description: "answers at once".into(),
            parameters: json!({"type": "object", "properties": {}}),
        },
        |_session, _args| async { Ok("stdlib ok".to_string()) },
    );
    let agent = BuiltinAgent::builder("failure", model.clone())
        .memory(memory.clone())
        .model("test")
        .tool(quick)
        .build();

    let failed = agent.run("session", "make music", &[]).await;
    assert!(
        matches!(
            failed,
            Err(AgentError::Gateway(GatewayError::ApiStatus {
                status: 504,
                ..
            }))
        ),
        "{failed:?}"
    );
    let history = memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history.len(), 4, "{history:?}");
    assert_closed(&history[3], "failed: the model service answered HTTP 504");
    // The provider's own error body stays out of the history every later request carries.
    assert!(!history[3].content.as_deref().unwrap().contains("upstream"));

    let reply = agent.run("session", "draw a picture", &[]).await.unwrap();
    assert_eq!(reply.content, "here is a picture");

    // The next request shows the failed turn as finished, then asks only the new question.
    let requests = model.requests.lock().unwrap();
    let next = &requests.last().unwrap().messages;
    let tail: Vec<(Role, Option<&str>)> = next[next.len() - 3..]
        .iter()
        .map(|message| (message.role, message.content.as_deref()))
        .collect();
    assert_eq!(tail[0], (Role::Tool, Some("stdlib ok")));
    assert_eq!(tail[1].0, Role::Assistant);
    assert_eq!(tail[2], (Role::User, Some("draw a picture")));
}
