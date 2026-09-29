//! Integration tests for token budget estimation and context summary compression.

use async_trait::async_trait;
use std::sync::Arc;

use kanon_llm::agent::Agent;
use kanon_llm::error::GatewayError;

use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{ChatMessage, ChatRequest, ChatResponse, Role, ToolCall};
use kanon_llm::memory::{Memory, SlidingWindowMemory};
use kanon_llm::summary::{ContextSummarizer, SummaryConfig, SummaryHook};
use kanon_llm::token::{
    estimate_conversation_tokens, estimate_message_tokens, estimate_text_tokens,
};

/// Mock provider that inspects whether the request is a summarization request or standard query.
struct MockSummarizingProvider;

#[async_trait]
impl LlmProvider for MockSummarizingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let first_msg = request
            .messages
            .first()
            .and_then(|m| m.content.as_deref())
            .unwrap_or_default();
        if first_msg.contains("Provide a concise, factual summary") {
            Ok(ChatResponse {
                content: Some("User asked for multiple calculation operations.".to_string()),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: Some("Final assistant answer.".to_string()),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            })
        }
    }
}

#[test]
fn test_token_estimator_heuristics() {
    // 1. Plain ASCII text (~4 chars per token)
    let ascii_text = "Hello world from Rust"; // 21 chars -> 6 tokens
    let tokens = estimate_text_tokens(ascii_text);
    assert_eq!(tokens, 6);

    // 2. CJK text (~1 token per character)
    let cjk_text = "你好世界，人工智能"; // 9 characters
    let cjk_tokens = estimate_text_tokens(cjk_text);
    assert_eq!(cjk_tokens, 9);

    // 3. Mixed text
    let mixed = "Hello 你好 123";
    let mixed_tokens = estimate_text_tokens(mixed);
    assert!(mixed_tokens >= 4);

    // 4. Message framing overhead (~4 tokens + content)
    let msg = ChatMessage::user("Hello");
    let msg_tokens = estimate_message_tokens(&msg);
    assert_eq!(msg_tokens, 4 + estimate_text_tokens("Hello")); // 4 + 2 = 6

    // 5. Tool call overhead
    let tool_call = ToolCall {
        id: "call_1".to_string(),
        name: "calculate".to_string(),
        arguments: serde_json::json!({ "expr": "2+2" }),
    };
    let assistant_msg = ChatMessage::assistant_tool_calls(vec![tool_call], None);
    let tool_msg_tokens = estimate_message_tokens(&assistant_msg);
    assert!(tool_msg_tokens > 10);

    // 6. Entire conversation priming (+2 tokens)
    let conv = vec![ChatMessage::system("Sys"), ChatMessage::user("Hi")];
    let conv_tokens = estimate_conversation_tokens(&conv);
    assert!(conv_tokens >= 12);
}

#[tokio::test]
async fn test_context_summarizer_compression_lifecycle() {
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(50));
    let provider = Arc::new(MockSummarizingProvider);

    let session_id = "test_summary_session";
    memory
        .set_system_prompt(session_id, "Act as an expert math tutor.".to_string())
        .await
        .unwrap();

    // Fill the conversation with 8 turns
    for i in 1..=8 {
        memory
            .push_message(
                session_id,
                ChatMessage::user(format!(
                    "Can you please solve problem equation number {i} in detail?"
                )),
            )
            .await
            .unwrap();
        memory
            .push_message(
                session_id,
                ChatMessage::assistant(format!(
                    "Here is the step by step detailed solution for {i}."
                )),
            )
            .await
            .unwrap();
    }

    let initial_msgs = memory.get_messages(session_id).await.unwrap();
    let initial_tokens = estimate_conversation_tokens(&initial_msgs);
    assert!(initial_tokens > 200);

    // Configure summarizer with a trigger budget of 100 tokens, preserving 4 recent messages
    let config = SummaryConfig {
        enabled: true,
        trigger_token_budget: 100,
        preserve_recent_messages: 4,
        summary_model: None,
        custom_instruction: None,
    };

    let summarizer = ContextSummarizer::new(config, provider, memory.clone());
    let did_compress = summarizer
        .compress_session(session_id)
        .await
        .expect("Summarization failed");
    assert!(did_compress);

    let compressed_msgs = memory.get_messages(session_id).await.unwrap();

    // Structure of compressed messages:
    // 0: System prompt ("Act as an expert math tutor.")
    // 1: Summary message ("--- Context Summary of Previous Conversation ---\n...")
    // 2..=5: Preserved recent messages (last 4 messages)
    assert_eq!(compressed_msgs.len(), 6);
    assert_eq!(compressed_msgs[0].role, Role::System);
    assert_eq!(
        compressed_msgs[0].content.as_deref(),
        Some("Act as an expert math tutor.")
    );

    assert_eq!(compressed_msgs[1].role, Role::System);
    let summary_content = compressed_msgs[1].content.as_deref().unwrap();
    assert!(summary_content.contains("Context Summary of Previous Conversation"));
    assert!(summary_content.contains("User asked for multiple calculation operations."));

    // Check that recent messages are indeed the latest ones
    assert!(
        compressed_msgs[5]
            .content
            .as_deref()
            .unwrap()
            .contains("solution for 8")
    );
}

#[tokio::test]
async fn test_summary_hook_with_agent_integration() {
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(50));
    let provider = Arc::new(MockSummarizingProvider);

    let session_id = "agent_hook_session";
    memory
        .set_system_prompt(session_id, "Agent Persona".to_string())
        .await
        .unwrap();

    // Pre-populate with older dialogue
    for i in 1..=6 {
        memory
            .push_message(session_id, ChatMessage::user(format!("History item {i}")))
            .await
            .unwrap();
    }

    let config = SummaryConfig {
        enabled: true,
        trigger_token_budget: 50,
        preserve_recent_messages: 2,
        summary_model: None,
        custom_instruction: None,
    };
    let summarizer = Arc::new(ContextSummarizer::new(
        config,
        provider.clone(),
        memory.clone(),
    ));
    let summary_hook = Arc::new(SummaryHook::new(summarizer));

    let agent = Agent::builder("test_agent", provider)
        .system_prompt("Agent Persona")
        .memory(memory.clone())
        .hook_arc(summary_hook)
        .model("test-model")
        .build();

    let output = agent
        .run_standalone(session_id, "Latest user query")
        .await
        .expect("Agent execution failed");

    assert_eq!(output.content, "Final assistant answer.");

    // Memory should contain compressed summary block + recent messages + latest query & response
    let msgs = memory.get_messages(session_id).await.unwrap();
    assert!(msgs.iter().any(|m| {
        m.content
            .as_deref()
            .map(|c| c.contains("Context Summary of Previous Conversation"))
            .unwrap_or(false)
    }));
}

#[tokio::test]
async fn test_agent_builder_summary_config_fluent_api() {
    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(50));
    let provider = Arc::new(MockSummarizingProvider);

    let session_id = "fluent_summary_session";
    memory
        .set_system_prompt(session_id, "Tutor".to_string())
        .await
        .unwrap();

    for i in 1..=6 {
        memory
            .push_message(session_id, ChatMessage::user(format!("Question {i}")))
            .await
            .unwrap();
    }

    // Use fluent .summary_config(...) directly on AgentBuilder
    let agent = Agent::builder("fluent_summary_agent", provider)
        .system_prompt("Tutor")
        .memory(memory.clone())
        .summary_config(SummaryConfig {
            enabled: true,
            trigger_token_budget: 40,
            preserve_recent_messages: 2,
            summary_model: None,
            custom_instruction: None,
        })
        .build();

    let output = agent
        .run_standalone(session_id, "What about question 7?")
        .await
        .expect("Agent execution failed");

    assert_eq!(output.content, "Final assistant answer.");

    let msgs = memory.get_messages(session_id).await.unwrap();
    assert!(msgs.iter().any(|m| {
        m.content
            .as_deref()
            .map(|c| c.contains("Context Summary of Previous Conversation"))
            .unwrap_or(false)
    }));
}

/// Verifies that `SummaryHook` modifies the in-flight `ChatRequest.messages` on the very
/// first turn that exceeds the token budget, preventing the bloated uncompressed context
/// from leaking to the model.
#[tokio::test]
async fn test_summary_hook_modifies_inflight_request_on_first_overbudget_turn() {
    use tokio::sync::Mutex;

    struct InFlightCapturingProvider {
        captured_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    }

    #[async_trait]
    impl LlmProvider for InFlightCapturingProvider {
        async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
            let first_msg = request
                .messages
                .first()
                .and_then(|m| m.content.as_deref())
                .unwrap_or_default();
            if first_msg.contains("Provide a concise, factual summary") {
                Ok(ChatResponse {
                    content: Some("Summary of items 1 to 6.".to_string()),
                    tool_calls: vec![],
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                })
            } else {
                // Record the actual messages passed to the main model call
                self.captured_messages
                    .lock()
                    .await
                    .push(request.messages.clone());
                Ok(ChatResponse {
                    content: Some("Answer after compression.".to_string()),
                    tool_calls: vec![],
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                })
            }
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(InFlightCapturingProvider {
        captured_messages: captured.clone(),
    });

    let memory: Arc<dyn Memory> = Arc::new(SlidingWindowMemory::new(50));
    let session_id = "inflight_test_sess";

    memory
        .set_system_prompt(session_id, "System persona".to_string())
        .await
        .unwrap();

    // Populate with 6 long dialogue turns so token budget is exceeded
    for i in 1..=6 {
        memory
            .push_message(
                session_id,
                ChatMessage::user(format!("Lengthy user prompt {i} with lots of tokens")),
            )
            .await
            .unwrap();
        memory
            .push_message(
                session_id,
                ChatMessage::assistant(format!("Lengthy assistant response {i} with tokens")),
            )
            .await
            .unwrap();
    }

    let agent = Agent::builder("inflight_agent", provider)
        .system_prompt("System persona")
        .memory(memory.clone())
        .summary_config(SummaryConfig {
            enabled: true,
            trigger_token_budget: 60, // Very low budget to trigger compression immediately
            preserve_recent_messages: 2,
            summary_model: None,
            custom_instruction: None,
        })
        .build();

    let output = agent
        .run_standalone(session_id, "Latest inbound user message")
        .await
        .expect("Agent execution failed");

    assert_eq!(output.content, "Answer after compression.");

    let recorded = captured.lock().await;
    assert_eq!(
        recorded.len(),
        1,
        "Expected exactly 1 main chat request to be captured"
    );

    let inflight_messages = &recorded[0];
    // Check that the in-flight request was compressed. The persona and the summary block form one
    // static system message, followed by the preserved assistant message and the latest user
    // message = 3 messages (instead of the uncompressed 1 System + 12 history + 1 user = 14).
    assert_eq!(inflight_messages.len(), 3);
    assert_eq!(inflight_messages[0].role, Role::System);
    let system_text = inflight_messages[0].content.as_deref().unwrap();
    assert!(system_text.starts_with("System persona"));
    assert!(system_text.contains("Context Summary of Previous Conversation"));
    assert!(system_text.contains("Summary of items 1 to 6."));

    // Second message is preserved assistant turn 6
    assert_eq!(inflight_messages[1].role, Role::Assistant);

    // Last message in request must be the latest user input
    assert_eq!(inflight_messages[2].role, Role::User);
    assert_eq!(
        inflight_messages[2].content.as_deref(),
        Some("Latest inbound user message")
    );
}
