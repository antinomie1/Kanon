//! A stopped turn ends at its next wait, abandons the work it was waiting on, and leaves a history
//! the provider still accepts, so the session goes on working after `/stop`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use kanon_llm::agent::{Agent, NativeTool, STOPPED_TOOL_RESULT};
use kanon_llm::error::GatewayError;
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse, Role, ToolCall, ToolDefinition};
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
    agent: Agent,
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
    let agent = Agent::builder(
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
        [Role::User, Role::Assistant, Role::Tool, Role::Tool],
        "{history:?}"
    );
    for (message, id) in history[2..].iter().zip(["call-a", "call-b"]) {
        assert_eq!(message.tool_call_id.as_deref(), Some(id));
        assert_eq!(message.content.as_deref(), Some(STOPPED_TOOL_RESULT));
    }

    // The session keeps working: the next turn sees the stopped round and appends to it.
    let reply = fixture.agent.run("session", "again", &[]).await.unwrap();
    assert_eq!(reply.content, "done");
    let history = fixture.memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history.len(), 6);
    assert_eq!(history[4].content.as_deref(), Some("again"));
}

#[tokio::test]
async fn stopping_a_model_wait_keeps_the_question_and_adds_nothing() {
    let fixture = fixture();
    let outcome = run_and_stop(&fixture, "hang").await;
    assert!(matches!(outcome, Err(AgentError::Stopped)), "{outcome:?}");

    let history = fixture.memory.snapshot("session").await.unwrap().messages;
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].content.as_deref(), Some("hang"));

    let reply = fixture.agent.run("session", "again", &[]).await.unwrap();
    assert_eq!(reply.content, "done");
}
