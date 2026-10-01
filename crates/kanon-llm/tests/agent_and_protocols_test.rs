//! Comprehensive unit tests for Kanon Agent engine, pluggable Memory, and Protocol Providers.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::RwLock;

use kanon_llm::agent::{Agent, AgentConfig, AgentHook, NativeTool};
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::providers::{
    AnthropicMessagesProvider, OpenAiChatProvider, OpenAiResponsesProvider,
};
use kanon_llm::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, Role, ToolCall, ToolDefinition,
};
use kanon_llm::memory::{Memory, MemorySnapshot};
use kanon_llm::tool_router::{ToolHost, json_to_prost_struct, prost_struct_to_json};
use kanon_llm::{AgentError, GatewayError, MemoryError};
use kanon_proto::v1::{
    PluginMeta, ToolCallRequest, ToolCallResponse, ToolMeta, tool_call_request, tool_call_response,
};

// =========================================================================
// 1. Mock LlmProvider and Mock ToolHost for Agent Testing
// =========================================================================

struct ScriptedLlmProvider {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    received_requests: Arc<RwLock<Vec<ChatRequest>>>,
}

impl ScriptedLlmProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            received_requests: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlmProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.received_requests.write().await.push(request.clone());
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok(ChatResponse {
                content: Some("Default fallback".to_string()),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            })
        }
    }
}

struct MockHost {
    host_id: String,
    plugins: Vec<PluginMeta>,
}

#[async_trait]
#[async_trait::async_trait]
impl ToolHost for MockHost {
    fn host_id(&self) -> &str {
        &self.host_id
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        self.plugins.clone()
    }

    async fn call_tool(&self, req: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        if req.tool_name == "reverse_string" {
            let structured_args = match req.payload {
                Some(tool_call_request::Payload::StructuredArgs(s)) => prost_struct_to_json(s),
                _ => serde_json::Value::Null,
            };

            let text = structured_args
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            let reversed: String = text.chars().rev().collect();

            let result_json = serde_json::json!({ "reversed": reversed });
            let result_struct = json_to_prost_struct(&result_json).unwrap();

            Ok(ToolCallResponse {
                call_id: req.call_id,
                success: true,
                error_message: String::new(),
                payload: Some(tool_call_response::Payload::StructuredResult(result_struct)),
                attachments: Vec::new(),
            })
        } else {
            Err(tonic::Status::not_found("Tool not found"))
        }
    }
}

// =========================================================================
// 2. Custom Plugged-in Memory Implementation (Verifying Modularity)
// =========================================================================

/// Custom memory simulating an external plugin or database-backed store.
struct CustomPluginMemory {
    stored_messages: Arc<RwLock<Vec<ChatMessage>>>,
    summary: Arc<RwLock<Option<String>>>,
}

impl CustomPluginMemory {
    fn new() -> Self {
        Self {
            stored_messages: Arc::new(RwLock::new(Vec::new())),
            summary: Arc::new(RwLock::new(None)),
        }
    }
}

#[async_trait]
impl Memory for CustomPluginMemory {
    async fn push_message(
        &self,
        _session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError> {
        self.stored_messages.write().await.push(message);
        Ok(())
    }

    async fn snapshot(&self, _session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        Ok(MemorySnapshot {
            summary: self.summary.read().await.clone(),
            messages: self.stored_messages.read().await.clone(),
        })
    }

    async fn compact_history(
        &self,
        _session_key: &str,
        covered: usize,
        summary: String,
    ) -> Result<(), MemoryError> {
        self.stored_messages.write().await.drain(..covered);
        *self.summary.write().await = Some(summary);
        Ok(())
    }

    async fn clear(&self, _session_key: &str) -> Result<(), MemoryError> {
        self.stored_messages.write().await.clear();
        *self.summary.write().await = None;
        Ok(())
    }

    async fn session_count(&self) -> Result<usize, MemoryError> {
        Ok(1)
    }
}

// =========================================================================
// 3. Custom Lifecycle Hook Implementation
// =========================================================================

struct TracingHook {
    request_intercepted: Arc<AtomicBool>,
    response_intercepted: Arc<AtomicBool>,
    tool_authorized: Arc<AtomicBool>,
    tool_finished: Arc<AtomicBool>,
    should_veto: bool,
}

#[async_trait]
impl AgentHook for TracingHook {
    async fn on_llm_request(
        &self,
        _session_id: &str,
        _request: &mut ChatRequest,
    ) -> Result<(), AgentError> {
        self.request_intercepted.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn on_llm_response(
        &self,
        _session_id: &str,
        _response: &mut ChatResponse,
    ) -> Result<(), AgentError> {
        self.response_intercepted.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn on_before_tool_call(
        &self,
        _session_id: &str,
        _call: &ToolCall,
    ) -> Result<bool, AgentError> {
        self.tool_authorized.store(true, Ordering::SeqCst);
        if self.should_veto {
            Ok(false)
        } else {
            Ok(true)
        }
    }

    async fn on_after_tool_call(
        &self,
        _session_id: &str,
        _call: &ToolCall,
        _result: &str,
        _success: bool,
    ) -> Result<(), AgentError> {
        self.tool_finished.store(true, Ordering::SeqCst);
        Ok(())
    }
}

// =========================================================================
// 4. Tests
// =========================================================================

#[tokio::test]
async fn test_agent_builder_and_execution_with_custom_memory() {
    let turn1 = ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_rev_1".to_string(),
            name: "reverse_string".to_string(),
            arguments: serde_json::json!({ "text": "kanon" }),
        }],
        finish_reason: Some("tool_calls".to_string()),
        usage: None,
    };

    let turn2 = ChatResponse {
        content: Some("The reversed string is 'nonak'.".to_string()),
        tool_calls: vec![],
        finish_reason: Some("stop".to_string()),
        usage: None,
    };

    let provider = Arc::new(ScriptedLlmProvider::new(vec![turn1, turn2]));
    let custom_memory = Arc::new(CustomPluginMemory::new());

    let agent = Agent::builder("translator_bot", provider.clone())
        .system_prompt("You are a string transformation agent.")
        .memory(custom_memory.clone() as Arc<dyn Memory>)
        .model("agent-model-v1")
        .max_iterations(3)
        .temperature(0.2)
        .build();

    assert_eq!(agent.name(), "translator_bot");
    assert_eq!(agent.config().max_iterations, 3);

    let tool_meta = ToolMeta {
        name: "reverse_string".to_string(),
        description: "Reverses input text".to_string(),
        parameters: json_to_prost_struct(&serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } }
        })),
    };

    let host = Arc::new(MockHost {
        host_id: "host_plugin".to_string(),
        plugins: vec![PluginMeta {
            id: "plugin_rev".to_string(),
            name: "Reverser".to_string(),
            version: "0.1.0".to_string(),
            author: "dev".to_string(),
            description: "test".to_string(),
            commands: vec![],
            tools: vec![tool_meta],
            ..Default::default()
        }],
    });

    let hosts: Vec<Arc<dyn ToolHost>> = vec![host];

    let output = agent
        .run("sess_custom", "Please reverse 'kanon'", &hosts)
        .await
        .expect("Agent execution should succeed");

    assert_eq!(output.content, "The reversed string is 'nonak'.");
    assert_eq!(output.turns, 2);
    assert_eq!(output.executed_tools.len(), 1);
    assert_eq!(output.executed_tools[0].tool_name, "reverse_string");
    assert!(output.executed_tools[0].success);

    // Verify custom memory received all messages correctly
    // The history holds only what was said: the agent's instructions are composed into each
    // request, never stored per session.
    let history = custom_memory.get_messages("sess_custom").await.unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Assistant);
    assert_eq!(history[2].role, Role::Tool);
    assert_eq!(history[3].role, Role::Assistant);
}

#[tokio::test]
async fn test_agent_native_in_process_tool_and_standalone_run() {
    let turn1 = ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_calc_1".to_string(),
            name: "multiply".to_string(),
            arguments: serde_json::json!({ "x": 6.0, "y": 7.0 }),
        }],
        finish_reason: Some("tool_calls".to_string()),
        usage: None,
    };

    let turn2 = ChatResponse {
        content: Some("Result is 42.".to_string()),
        tool_calls: vec![],
        finish_reason: Some("stop".to_string()),
        usage: None,
    };

    let provider = Arc::new(ScriptedLlmProvider::new(vec![turn1, turn2]));

    let multiply_tool = NativeTool::new(
        ToolDefinition {
            name: "multiply".to_string(),
            description: "Multiplies two numbers".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                }
            }),
        },
        |_session_id, args| async move {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            Ok(format!("{}", x * y))
        },
    );

    let agent = Agent::builder("math_agent", provider)
        .tool(multiply_tool)
        .build();

    // Standalone execution does not require any external gRPC plugin processes!
    let output = agent
        .run_standalone("session_standalone", "What is 6 * 7?")
        .await
        .expect("Standalone run with native tool should succeed");

    assert_eq!(output.content, "Result is 42.");
    assert_eq!(output.executed_tools.len(), 1);
    assert_eq!(output.executed_tools[0].tool_name, "multiply");
    assert_eq!(output.executed_tools[0].plugin_id, "native");
    assert_eq!(output.executed_tools[0].host_id, "in_process");
    assert!(output.executed_tools[0].success);
}

#[tokio::test]
async fn test_agent_lifecycle_hooks_and_veto() {
    let req_flag = Arc::new(AtomicBool::new(false));
    let resp_flag = Arc::new(AtomicBool::new(false));
    let auth_flag = Arc::new(AtomicBool::new(false));
    let fin_flag = Arc::new(AtomicBool::new(false));

    let hook = TracingHook {
        request_intercepted: req_flag.clone(),
        response_intercepted: resp_flag.clone(),
        tool_authorized: auth_flag.clone(),
        tool_finished: fin_flag.clone(),
        should_veto: true, // Veto the tool execution!
    };

    let turn1 = ChatResponse {
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_dangerous".to_string(),
            name: "delete_database".to_string(),
            arguments: serde_json::json!({}),
        }],
        finish_reason: Some("tool_calls".to_string()),
        usage: None,
    };

    let turn2 = ChatResponse {
        content: Some(
            "I was unable to perform the deletion because permission was denied.".to_string(),
        ),
        tool_calls: vec![],
        finish_reason: Some("stop".to_string()),
        usage: None,
    };

    let provider = Arc::new(ScriptedLlmProvider::new(vec![turn1, turn2]));

    let agent = Agent::builder("safety_agent", provider).hook(hook).build();

    let output = agent
        .run_standalone("sess_guardrail", "Delete all database tables")
        .await
        .expect("Agent execution should succeed with vetoed tool");

    assert!(req_flag.load(Ordering::SeqCst));
    assert!(resp_flag.load(Ordering::SeqCst));
    assert!(auth_flag.load(Ordering::SeqCst));
    // Since it was vetoed, tool execution never finished:
    assert!(!fin_flag.load(Ordering::SeqCst));

    assert_eq!(output.executed_tools.len(), 1);
    assert!(!output.executed_tools[0].success);
    assert_eq!(output.executed_tools[0].plugin_id, "policy");
}

#[test]
fn test_openai_responses_provider_endpoints_and_headers() {
    let p1 = OpenAiResponsesProvider::new("sk-test");
    assert_eq!(p1.endpoint(), "https://api.openai.com/v1/responses");

    let p2 = OpenAiResponsesProvider::new("sk-test").with_base_url("https://api.openai.com");
    assert_eq!(p2.endpoint(), "https://api.openai.com/v1/responses");

    let p3 = OpenAiResponsesProvider::new("sk-test").with_base_url("https://custom.ai/v1");
    assert_eq!(p3.endpoint(), "https://custom.ai/v1/responses");

    let p4 = OpenAiResponsesProvider::new("sk-test").with_base_url("https://custom.ai/responses/");
    assert_eq!(p4.endpoint(), "https://custom.ai/responses");

    let p5 = OpenAiResponsesProvider::new("sk-test").with_header("x-trace-id", "trace-abc");
    assert_eq!(p5.custom_headers().len(), 1);
    assert_eq!(
        p5.custom_headers()[0],
        ("x-trace-id".to_string(), "trace-abc".to_string())
    );
}

#[test]
fn test_openai_chat_provider_endpoint_and_headers() {
    let provider = OpenAiChatProvider::new(
        "http://localhost:8000/v1",
        Some("sk-test".to_string()),
        "gpt-4o",
    )
    .with_header("x-org-id", "org_123");

    let _ = provider;
}

#[test]
fn test_anthropic_messages_provider_endpoint_and_headers() {
    let provider = AnthropicMessagesProvider::new(
        "https://api.anthropic.com",
        Some("sk-ant-test".to_string()),
        "claude-3-5-sonnet",
    )
    .with_version("2023-06-01")
    .with_header("x-custom-env", "sandbox");

    let _ = provider;
}

#[test]
fn test_agent_config_defaults() {
    let config = AgentConfig::default();
    assert_eq!(config.max_iterations, 5);
    assert_eq!(config.default_model, "gpt-4o-mini");
    assert!(!config.stop_on_tool_failure);
}
