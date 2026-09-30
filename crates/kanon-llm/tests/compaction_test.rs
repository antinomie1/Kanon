//! Tests for cache-safe context compaction.
//!
//! The behaviour under test is the *shape* of the traffic to the provider: the summarization
//! request must be the conversation's own request plus one appended message (so the provider's cache
//! serves it), and after it the prompt prefix must be stable again.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;

use kanon_llm::agent::{Agent, NativeTool};
use kanon_llm::error::{AgentError, GatewayError};
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, Role, TokenUsage, ToolCall, ToolDefinition,
};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::prompt::{BASE_PERSONA_PROMPT, PersonaRegistry};
use kanon_llm::session::SessionManager;
use kanon_llm::{COMPACTION_INSTRUCTION, CompactionPolicy};

/// What the provider answers when asked to summarize.
#[derive(Clone)]
enum SummaryMode {
    Summary(&'static str),
    Empty,
    ToolCall,
    Fail,
}

/// Provider that records every request and tells summarization requests from ordinary turns.
struct ScriptedProvider {
    requests: Mutex<Vec<ChatRequest>>,
    /// Prompt size reported for ordinary turns.
    reported_prompt_tokens: u32,
    summary: SummaryMode,
    /// When set, a summarization request waits here, so a test can act while it is in flight.
    gate: Option<Arc<Notify>>,
}

impl ScriptedProvider {
    fn new(reported_prompt_tokens: u32, summary: SummaryMode) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            reported_prompt_tokens,
            summary,
            gate: None,
        }
    }

    fn gated(reported_prompt_tokens: u32, gate: Arc<Notify>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(reported_prompt_tokens, SummaryMode::Summary("SUMMARY-TEXT"))
        }
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn compaction_requests(&self) -> Vec<ChatRequest> {
        self.requests().into_iter().filter(is_compaction).collect()
    }
}

fn is_compaction(request: &ChatRequest) -> bool {
    request.messages.last().is_some_and(|message| {
        message.role == Role::User && message.content.as_deref() == Some(COMPACTION_INSTRUCTION)
    })
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let turn = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };

        if !is_compaction(request) {
            return Ok(ChatResponse {
                content: Some(format!("reply {turn}")),
                finish_reason: Some("stop".to_string()),
                usage: Some(TokenUsage {
                    prompt_tokens: self.reported_prompt_tokens,
                    completion_tokens: 10,
                    total_tokens: self.reported_prompt_tokens + 10,
                    ..TokenUsage::default()
                }),
                ..ChatResponse::default()
            });
        }

        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        match &self.summary {
            SummaryMode::Summary(text) => Ok(ChatResponse {
                content: Some((*text).to_string()),
                finish_reason: Some("stop".to_string()),
                ..ChatResponse::default()
            }),
            SummaryMode::Empty => Ok(ChatResponse {
                content: Some("   ".to_string()),
                ..ChatResponse::default()
            }),
            SummaryMode::ToolCall => Ok(ChatResponse {
                content: Some("I will use a tool instead".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "noop".to_string(),
                    arguments: serde_json::json!({}),
                }],
                ..ChatResponse::default()
            }),
            SummaryMode::Fail => Err(GatewayError::InvalidResponse("boom".to_string())),
        }
    }
}

/// An agent with a 1,000-token window (so compaction starts at 700 tokens), one native tool and the
/// default persona.
fn agent(provider: Arc<ScriptedProvider>, memory: Arc<InMemory>) -> Agent {
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    Agent::builder("compaction", provider)
        .memory(memory)
        .session_manager(sessions)
        .persona_registry(Arc::new(PersonaRegistry::default()))
        .tool(NativeTool::new(
            ToolDefinition {
                name: "noop".to_string(),
                description: "does nothing".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
            |_session, _args| async { Ok("ok".to_string()) },
        ))
        .model("test-model")
        .context_length(Some(1_000))
        .build()
}

/// Waits until a session has been compacted, or fails the test.
async fn wait_for_summary(memory: &InMemory, session: &str) {
    for _ in 0..200 {
        if memory.snapshot(session).await.unwrap().summary.is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the session was never compacted");
}

#[test]
fn the_policy_triggers_at_a_fraction_of_the_window_and_assumes_one_when_unknown() {
    let policy = CompactionPolicy::default();

    assert_eq!(policy.trigger_tokens(Some(100_000)), 70_000);
    assert_eq!(policy.trigger_tokens(Some(1_000)), 700);
    // An unknown window is assumed small rather than large.
    assert_eq!(policy.trigger_tokens(None), 22_938);
    assert_eq!(policy.trigger_tokens(Some(0)), 22_938);

    assert!(!policy.is_exceeded(699, Some(1_000)));
    assert!(policy.is_exceeded(700, Some(1_000)));

    assert!(policy.validate().is_ok());
    for bad in [0.0, -0.5, 1.5, f32::NAN] {
        let broken = CompactionPolicy {
            trigger_ratio: bad,
            ..policy
        };
        assert!(broken.validate().is_err(), "ratio {bad} must be rejected");
    }
}

#[tokio::test]
async fn a_large_context_is_compacted_in_the_background_using_the_conversations_own_prefix() {
    let provider = Arc::new(ScriptedProvider::new(
        800,
        SummaryMode::Summary("SUMMARY-TEXT"),
    ));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    agent.run("s", "turn 1", &[]).await.unwrap();
    // One exchange is too short to be worth summarizing.
    assert!(provider.compaction_requests().is_empty());

    agent.run("s", "turn 2", &[]).await.unwrap();
    wait_for_summary(&memory, "s").await;

    let requests = provider.requests();
    let turn2 = &requests[1];
    let compaction = provider.compaction_requests();
    assert_eq!(compaction.len(), 1);
    let compaction = &compaction[0];

    // The point of the design: everything the summarization request sends before its last message
    // is what the provider already holds from turn 2 (plus turn 2's reply), so it is served from
    // the cache. The tools — the very top of the prompt — are identical too.
    assert_eq!(
        serde_json::to_string(&compaction.tools).unwrap(),
        serde_json::to_string(&turn2.tools).unwrap()
    );
    let shared = turn2.messages.len();
    assert_eq!(&compaction.messages[..shared], turn2.messages.as_slice());
    assert_eq!(compaction.messages[shared].role, Role::Assistant);
    assert_eq!(
        compaction.messages[shared].content.as_deref(),
        Some("reply 2")
    );
    assert_eq!(compaction.messages.len(), shared + 2);
    assert_eq!(
        compaction
            .messages
            .last()
            .and_then(|m| m.content.as_deref()),
        Some(COMPACTION_INSTRUCTION)
    );

    // The four messages were folded into the summary.
    let snapshot = memory.snapshot("s").await.unwrap();
    assert_eq!(snapshot.summary.as_deref(), Some("SUMMARY-TEXT"));
    assert!(snapshot.messages.is_empty());

    // The next request mounts the summary in the static block, after the persona, and the tools
    // are unchanged: only the history got shorter.
    agent.run("s", "turn 3", &[]).await.unwrap();
    let next = provider
        .requests()
        .into_iter()
        .rfind(|request| !is_compaction(request))
        .unwrap();
    let roles: Vec<Role> = next.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::System, Role::User]);
    let system = next.messages[0].content.as_deref().unwrap();
    assert!(system.starts_with(BASE_PERSONA_PROMPT), "{system}");
    assert!(system.contains("## Conversation summary"), "{system}");
    assert!(system.ends_with("SUMMARY-TEXT"), "{system}");
    assert_eq!(next.messages[1].content.as_deref(), Some("turn 3"));
    assert_eq!(
        serde_json::to_string(&next.tools).unwrap(),
        serde_json::to_string(&turn2.tools).unwrap()
    );
}

#[tokio::test]
async fn the_prefix_is_stable_again_after_a_compaction() {
    let provider = Arc::new(ScriptedProvider::new(
        800,
        SummaryMode::Summary("SUMMARY-TEXT"),
    ));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    agent.run("s", "a", &[]).await.unwrap();
    agent.run("s", "b", &[]).await.unwrap();
    wait_for_summary(&memory, "s").await;

    // After the compaction, consecutive requests extend one another again.
    let mark = provider.requests().len();
    agent.run("s", "c", &[]).await.unwrap();
    // Only one exchange since the compaction, so nothing new is scheduled.
    agent.run("s", "d", &[]).await.unwrap();

    let after: Vec<ChatRequest> = provider.requests()[mark..].to_vec();
    let (first, second) = (&after[0], &after[1]);
    assert_eq!(
        first.messages[0], second.messages[0],
        "system block unchanged"
    );
    assert_eq!(
        &second.messages[..first.messages.len()],
        first.messages.as_slice(),
        "history extends the previous request"
    );
}

#[tokio::test]
async fn a_small_context_is_never_compacted() {
    let provider = Arc::new(ScriptedProvider::new(50, SummaryMode::Summary("unused")));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    for turn in 1..=6 {
        agent.run("s", &format!("turn {turn}"), &[]).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(provider.compaction_requests().is_empty());
    let snapshot = memory.snapshot("s").await.unwrap();
    assert!(snapshot.summary.is_none());
    assert_eq!(
        snapshot.messages.len(),
        12,
        "history is append-only and untouched"
    );
}

#[tokio::test]
async fn compaction_can_be_switched_off() {
    let provider = Arc::new(ScriptedProvider::new(900, SummaryMode::Summary("unused")));
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let agent = Agent::builder("off", provider.clone())
        .memory(memory.clone())
        .session_manager(sessions)
        .model("m")
        .context_length(Some(1_000))
        .compaction(None)
        .build();

    for turn in 1..=4 {
        agent.run("s", &format!("turn {turn}"), &[]).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(provider.compaction_requests().is_empty());
    assert_eq!(memory.snapshot("s").await.unwrap().messages.len(), 8);
}

#[tokio::test]
async fn a_summary_that_is_not_one_leaves_the_history_exactly_as_it_was() {
    for (mode, expected) in [
        (SummaryMode::Empty, "no summary"),
        (
            SummaryMode::Summary("<think>private-a</think><think>private-b</think>"),
            "no summary",
        ),
        (
            SummaryMode::Summary("<think>private-a</think><think>unfinished"),
            "no summary",
        ),
        (SummaryMode::ToolCall, "no summary"),
        (SummaryMode::Fail, "boom"),
    ] {
        let provider = Arc::new(ScriptedProvider::new(50, mode));
        let memory = Arc::new(InMemory::new());
        let agent = agent(provider.clone(), memory.clone());
        for turn in 1..=2 {
            agent.run("s", &format!("turn {turn}"), &[]).await.unwrap();
        }
        let before = memory.snapshot("s").await.unwrap();

        let error = agent
            .compact_session("s", &[])
            .await
            .expect_err("no usable summary");
        assert!(error.to_string().contains(expected), "{error}");
        if expected == "no summary" {
            assert!(matches!(error, AgentError::Compaction(_)), "{error}");
        }

        assert_eq!(
            memory.snapshot("s").await.unwrap(),
            before,
            "folding the history away on a bad answer would lose the conversation"
        );
    }
}

#[tokio::test]
async fn only_a_history_at_a_clean_stopping_point_is_compacted() {
    let provider = Arc::new(ScriptedProvider::new(50, SummaryMode::Summary("S")));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    // Too short.
    Memory::push_message(&*memory, "short", ChatMessage::user("hi"))
        .await
        .unwrap();
    Memory::push_message(&*memory, "short", ChatMessage::assistant("hello"))
        .await
        .unwrap();
    assert!(!agent.compact_session("short", &[]).await.unwrap());

    // A turn still waiting for its answer: the last message is the user's.
    for message in [
        ChatMessage::user("1"),
        ChatMessage::assistant("2"),
        ChatMessage::user("3"),
        ChatMessage::assistant("4"),
        ChatMessage::user("5"),
    ] {
        Memory::push_message(&*memory, "waiting", message)
            .await
            .unwrap();
    }
    assert!(!agent.compact_session("waiting", &[]).await.unwrap());

    // Mid tool-loop: the last message is a tool call awaiting its result.
    for message in [
        ChatMessage::user("1"),
        ChatMessage::assistant("2"),
        ChatMessage::user("3"),
        ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "c".to_string(),
                name: "noop".to_string(),
                arguments: serde_json::json!({}),
            }],
            None,
        ),
    ] {
        Memory::push_message(&*memory, "tooling", message)
            .await
            .unwrap();
    }
    assert!(!agent.compact_session("tooling", &[]).await.unwrap());

    assert!(
        provider.requests().is_empty(),
        "nothing was sent to the model for any of them"
    );
}

#[tokio::test]
async fn a_burst_of_turns_schedules_a_single_compaction() {
    let gate = Arc::new(Notify::new());
    let provider = Arc::new(ScriptedProvider::gated(800, gate.clone()));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    agent.run("s", "turn 1", &[]).await.unwrap();
    agent.run("s", "turn 2", &[]).await.unwrap(); // schedules a compaction, which blocks on the gate
    agent.run("s", "turn 3", &[]).await.unwrap(); // would schedule another
    agent.run("s", "turn 4", &[]).await.unwrap(); // and another

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        provider.compaction_requests().len(),
        1,
        "one compaction per session at a time"
    );

    gate.notify_one();
    wait_for_summary(&memory, "s").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(provider.compaction_requests().len(), 1);
}

#[tokio::test]
async fn messages_that_arrive_while_a_compaction_runs_survive_it() {
    let gate = Arc::new(Notify::new());
    let provider = Arc::new(ScriptedProvider::gated(800, gate.clone()));
    let memory = Arc::new(InMemory::new());
    let agent = agent(provider.clone(), memory.clone());

    agent.run("s", "turn 1", &[]).await.unwrap();
    agent.run("s", "turn 2", &[]).await.unwrap(); // compaction starts, covering these four messages
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The conversation goes on while the summary is being written.
    agent.run("s", "arrived meanwhile", &[]).await.unwrap();

    gate.notify_one();
    wait_for_summary(&memory, "s").await;

    let snapshot = memory.snapshot("s").await.unwrap();
    let kept: Vec<&str> = snapshot
        .messages
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(snapshot.summary.as_deref(), Some("SUMMARY-TEXT"));
    assert_eq!(kept.len(), 2, "the new exchange is not swallowed: {kept:?}");
    assert_eq!(kept[0], "arrived meanwhile");
}
