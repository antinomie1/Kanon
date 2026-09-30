//! End-to-end test of the textual tool-call recovery inside the agent loop.
//!
//! The model in this test never emits a structured `tool_calls` array; it answers with the markup a
//! MiMo-style endpoint produces. The agent must execute the tool and continue the turn instead of
//! returning the markup as the assistant's answer — which is the bug this feature fixes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::agent::{Agent, NativeTool};
use kanon_llm::{
    ChatMessage, ChatRequest, ChatResponse, GatewayError, InMemory, LlmProvider, Memory,
    ToolDefinition,
};

/// Provider that replays a fixed response script and records every request it receives.
struct ScriptedProvider {
    responses: Mutex<VecDeque<ChatResponse>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> (Self, Arc<Mutex<Vec<ChatRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                responses: Mutex::new(responses.into()),
                requests: requests.clone(),
            },
            requests,
        )
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| GatewayError::InvalidResponse("script exhausted".to_string()))
    }
}

fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "play_score".to_string(),
        description: "Play a chart for a song".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "song_name": { "type": "string" } }
        }),
    }
}

#[tokio::test]
async fn markup_tool_calls_are_executed_and_the_turn_continues() {
    let (provider, requests) = ScriptedProvider::new(vec![
        ChatResponse {
            reasoning_content: None,
            // Exactly the shape reported from a QQ adapter: no structured call, only markup.
            content: Some(
                "<tool_call><function=play_score><parameter=song_name>忙シー日</parameter>\
                 </function></tool_call>"
                    .to_string(),
            ),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        },
        ChatResponse {
            reasoning_content: None,
            content: Some("已完成。".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        },
    ]);

    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Agent::builder("test", Arc::new(provider))
        .memory(memory)
        .max_iterations(3)
        .tool(NativeTool::new(
            tool_definition(),
            |_session, arguments| async move {
                Ok(format!(
                    "played {}",
                    arguments
                        .get("song_name")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                ))
            },
        ))
        .build();

    let output = agent
        .run("session", "play a song", &[])
        .await
        .expect("agent run");

    assert_eq!(output.content, "已完成。");
    assert_eq!(output.executed_tools.len(), 1);
    assert_eq!(output.executed_tools[0].tool_name, "play_score");
    assert!(output.executed_tools[0].success);

    // The second request must contain the assistant call *and* its tool result, which is what
    // makes the model produce a real answer instead of repeating the markup.
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2, "one call per reasoning turn");
    let follow_up = &requests[1];
    assert!(
        follow_up
            .messages
            .iter()
            .any(|message| message.tool_calls.is_some()),
        "the recovered call must be recorded as an assistant tool call"
    );
    assert!(
        follow_up
            .messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("played 忙シー日") }),
        "the tool result must be fed back to the model"
    );
}

#[tokio::test]
async fn plain_text_without_markup_is_returned_unchanged() {
    let (provider, _requests) = ScriptedProvider::new(vec![ChatResponse {
        reasoning_content: None,
        content: Some("hello there".to_string()),
        tool_calls: Vec::new(),
        finish_reason: Some("stop".to_string()),
        usage: None,
    }]);

    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Agent::builder("test", Arc::new(provider))
        .memory(memory)
        .build();

    let output = agent.run("session", "hi", &[]).await.expect("agent run");
    assert_eq!(output.content, "hello there");
    assert!(output.executed_tools.is_empty());
}

/// Guards the memory helper used by the recovery path: a multimodal message still records its
/// textual projection, so history stays readable for a text-only consumer.
#[test]
fn multimodal_messages_keep_their_textual_projection() {
    use kanon_llm::ContentPart;

    let message = ChatMessage::user_multimodal(
        "look [图片]",
        vec![ContentPart::image_url(
            "https://example.invalid/a.png",
            None,
        )],
    );

    assert_eq!(message.content.as_deref(), Some("look [图片]"));
    assert!(message.has_parts());
    assert_eq!(
        message
            .parts
            .as_deref()
            .and_then(|parts| parts.first())
            .and_then(ContentPart::resolved_image_url)
            .as_deref(),
        Some("https://example.invalid/a.png")
    );
}
