//! A tool that generates a file must deliver it without leaking its node-local path.
//!
//! The attachment is what the platform sends; the path is node-internal. These tests pin both
//! halves of that rule: the attachment survives, and the path never appears in the transcript fed
//! back to the model.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::BuiltinAgent;
use kanon_llm::tool_router::{ToolHost, json_to_prost_struct};
use kanon_llm::{
    Agent, ChatRequest, ChatResponse, GatewayError, InMemory, LlmProvider, Memory, ToolCall,
};
use kanon_proto::v1::{
    PluginMeta, ToolAttachment, ToolCallRequest, ToolCallResponse, ToolMeta, tool_call_response,
};

/// Constant path the fake tool reports for its generated file.
const SECRET_PATH: &str = "/var/lib/kanon/data/attachments/secret-card.png";

/// Provider replaying a fixed script and recording every request.
struct ScriptedProvider {
    responses: Mutex<std::collections::VecDeque<ChatResponse>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
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

/// Host declaring one drawing tool that reports a file path in its structured result.
struct DrawingHost;

#[async_trait]
impl ToolHost for DrawingHost {
    fn host_id(&self) -> &str {
        "host_drawing"
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        vec![PluginMeta {
            id: "maimai".to_string(),
            name: "maimai".to_string(),
            version: "1.0.0".to_string(),
            author: "test".to_string(),
            description: "fixture".to_string(),
            commands: Vec::new(),
            tools: vec![ToolMeta {
                name: "draw_card".to_string(),
                description: "Draws a card".to_string(),
                parameters: None,
            }],
            ..Default::default()
        }]
    }

    async fn call_tool(&self, _req: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        let payload = json_to_prost_struct(&serde_json::json!({
            "status": "ok",
            "card_path": SECRET_PATH,
        }))
        .expect("object payload");

        Ok(ToolCallResponse {
            call_id: "call_1".to_string(),
            success: true,
            error_message: String::new(),
            payload: Some(tool_call_response::Payload::StructuredResult(payload)),
            attachments: vec![ToolAttachment {
                mime_type: "image/png".to_string(),
                file_path: Some(SECRET_PATH.to_string()),
                url: None,
            }],
        })
    }
}

#[tokio::test]
async fn the_generated_file_is_delivered_but_its_path_never_reaches_the_model() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(
            vec![
                ChatResponse {
                    reasoning_content: None,
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "draw_card".to_string(),
                        arguments: serde_json::json!({}),
                    }],
                    finish_reason: Some("tool_calls".to_string()),
                    usage: None,
                },
                ChatResponse {
                    reasoning_content: None,
                    content: Some("画好了".to_string()),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                },
            ]
            .into(),
        ),
        requests: requests.clone(),
    });

    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder("redaction", provider)
        .memory(memory)
        .max_iterations(3)
        .build();

    let hosts: Vec<Arc<dyn ToolHost>> = vec![Arc::new(DrawingHost)];
    let output = agent
        .run("session", "draw a card", &hosts)
        .await
        .expect("run");

    // The file is still delivered: the attachment is what the pipeline turns into an outbound image.
    assert_eq!(output.attachments.len(), 1);
    assert_eq!(
        output.attachments[0].file_path.as_deref(),
        Some(SECRET_PATH)
    );

    // The tool result recorded in memory must not contain the path.
    let requests = requests.lock().expect("requests lock");
    let follow_up = &requests[1];
    let tool_message = follow_up
        .messages
        .iter()
        .find(|message| message.role == kanon_llm::Role::Tool)
        .expect("tool response recorded");
    let content = tool_message.content.clone().unwrap_or_default();

    assert!(
        !content.contains(SECRET_PATH),
        "the filesystem path must be redacted, got: {content}"
    );
    assert!(
        !content.contains("attachments"),
        "no fragment of the path may survive, got: {content}"
    );
    assert!(
        content.contains("[attachment]"),
        "the replacement marker documents what was removed, got: {content}"
    );
    assert!(
        content.contains("\"status\":\"ok\""),
        "the rest of the result must be preserved, got: {content}"
    );
}
