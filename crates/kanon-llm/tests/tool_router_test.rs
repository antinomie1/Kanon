//! Unit tests for ToolRouter state machine loop and Tool Calling mechanics.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kanon_llm::GatewayError;
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse, ToolCall};
use kanon_llm::memory::InMemory;
use kanon_llm::tool_router::{ToolHost, ToolRouter, json_to_prost_struct, prost_struct_to_json};
use kanon_proto::v1::{
    PluginMeta, ToolAttachment, ToolCallRequest, ToolCallResponse, ToolMeta, tool_call_request,
    tool_call_response,
};

/// Mock LLM provider that simulates multi-turn reasoning and tool invocation responses.
struct MockLlmProvider {
    /// Invocations counter to simulate progression through reasoning turns.
    call_count: AtomicUsize,
    /// Scripted responses returned in order.
    responses: Vec<ChatResponse>,
}

impl MockLlmProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            responses,
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok(ChatResponse {
                reasoning_content: None,
                content: Some("Default fallback content".to_string()),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            })
        }
    }
}

/// Mock plugin host simulating a child process with registered tools.
struct MockToolHost {
    host_id: String,
    plugins: Vec<PluginMeta>,
    tool_calls_received: Arc<AtomicUsize>,
}

impl MockToolHost {
    fn new(host_id: &str, tools: Vec<ToolMeta>) -> Self {
        Self::with_plugin(host_id, "mock.plugin.test", tools)
    }

    fn with_plugin(host_id: &str, plugin_id: &str, tools: Vec<ToolMeta>) -> Self {
        Self {
            host_id: host_id.to_string(),
            plugins: vec![PluginMeta {
                id: plugin_id.to_string(),
                name: plugin_id.to_string(),
                version: "1.0.0".to_string(),
                author: "Test".to_string(),
                description: "Test".to_string(),
                commands: vec![],
                tools,
                ..Default::default()
            }],
            tool_calls_received: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
#[async_trait::async_trait]
impl ToolHost for MockToolHost {
    fn host_id(&self) -> &str {
        &self.host_id
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        self.plugins.clone()
    }

    async fn call_tool(&self, req: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        self.tool_calls_received.fetch_add(1, Ordering::SeqCst);

        if req.tool_name == "search" {
            let result_struct =
                json_to_prost_struct(&serde_json::json!({ "found": true, "host": self.host_id }))
                    .unwrap();
            Ok(ToolCallResponse {
                call_id: req.call_id,
                success: true,
                error_message: String::new(),
                payload: Some(tool_call_response::Payload::StructuredResult(result_struct)),
                attachments: Vec::new(),
            })
        } else if req.tool_name == "draw_card" {
            // A drawing tool: the result is a file the platform must deliver, not just text.
            let result_struct =
                json_to_prost_struct(&serde_json::json!({ "content": "card rendered" })).unwrap();
            Ok(ToolCallResponse {
                call_id: req.call_id,
                success: true,
                error_message: String::new(),
                payload: Some(tool_call_response::Payload::StructuredResult(result_struct)),
                attachments: vec![ToolAttachment {
                    mime_type: "image/png".to_string(),
                    file_path: Some("/tmp/kanon-test/card.png".to_string()),
                    url: None,
                }],
            })
        } else if req.tool_name == "add_numbers" {
            let structured_args = match req.payload {
                Some(tool_call_request::Payload::StructuredArgs(s)) => prost_struct_to_json(s),
                _ => serde_json::Value::Null,
            };

            let a = structured_args
                .get("a")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let b = structured_args
                .get("b")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let sum = a + b;

            let result_json = serde_json::json!({ "sum": sum });
            let result_struct = json_to_prost_struct(&result_json).unwrap();

            Ok(ToolCallResponse {
                call_id: req.call_id,
                success: true,
                error_message: String::new(),
                payload: Some(tool_call_response::Payload::StructuredResult(result_struct)),
                attachments: Vec::new(),
            })
        } else {
            Err(tonic::Status::not_found(format!(
                "Unknown tool: {}",
                req.tool_name
            )))
        }
    }
}

#[tokio::test]
async fn test_tool_router_direct_text_no_tools() {
    let mock_provider = Arc::new(MockLlmProvider::new(vec![ChatResponse {
        reasoning_content: None,
        content: Some("Hello! How can I assist you today?".to_string()),
        tool_calls: vec![],
        finish_reason: Some("stop".to_string()),
        usage: None,
    }]));

    let memory = Arc::new(InMemory::new());
    let router = ToolRouter::new(mock_provider, memory.clone(), "test-model");

    let hosts: Vec<Arc<dyn ToolHost>> = vec![];
    let output = router
        .execute("session_1", "Hello there", &hosts)
        .await
        .expect("Execution should succeed");

    assert_eq!(output.content, "Hello! How can I assist you today?");
    assert!(output.executed_tools.is_empty());

    // Verify conversation memory contains both user and assistant messages
    let history = memory.get_messages("session_1");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content.as_deref(), Some("Hello there"));
    assert_eq!(
        history[1].content.as_deref(),
        Some("Hello! How can I assist you today?")
    );
}

#[tokio::test]
async fn test_tool_router_successful_tool_loop() {
    // Turn 1: Model asks to call `add_numbers` with arguments { a: 15, b: 27 }
    let turn1 = ChatResponse {
        reasoning_content: None,
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_math_1".to_string(),
            name: "add_numbers".to_string(),
            arguments: serde_json::json!({ "a": 15.0, "b": 27.0 }),
        }],
        finish_reason: Some("tool_calls".to_string()),
        usage: None,
    };

    // Turn 2: After receiving { sum: 42 }, model generates final reply
    let turn2 = ChatResponse {
        reasoning_content: None,
        content: Some("The sum of 15 and 27 is 42.".to_string()),
        tool_calls: vec![],
        finish_reason: Some("stop".to_string()),
        usage: None,
    };

    let mock_provider = Arc::new(MockLlmProvider::new(vec![turn1, turn2]));
    let memory = Arc::new(InMemory::new());
    let router = ToolRouter::new(mock_provider, memory.clone(), "test-model");

    let tool_meta = ToolMeta {
        name: "add_numbers".to_string(),
        description: "Adds two floating numbers".to_string(),
        parameters: json_to_prost_struct(&serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "number" },
                "b": { "type": "number" }
            }
        })),
    };

    let host = Arc::new(MockToolHost::new("host_math", vec![tool_meta]));
    let hosts: Vec<Arc<dyn ToolHost>> = vec![host.clone()];

    let output = router
        .execute("session_math", "What is 15 + 27?", &hosts)
        .await
        .expect("Tool loop execution should succeed");

    assert_eq!(output.content, "The sum of 15 and 27 is 42.");
    assert_eq!(output.executed_tools.len(), 1);
    assert_eq!(output.executed_tools[0].tool_name, "add_numbers");
    assert!(output.executed_tools[0].success);
    assert_eq!(host.tool_calls_received.load(Ordering::SeqCst), 1);

    // Verify complete memory sequence: User -> Assistant (with tool_calls) -> Tool -> Assistant
    let history = memory.get_messages("session_math");
    assert_eq!(history.len(), 4);
    assert_eq!(history[0].content.as_deref(), Some("What is 15 + 27?"));
    assert!(history[1].tool_calls.is_some());
    assert_eq!(history[2].role, kanon_llm::gateway::Role::Tool);
    assert!(history[2].content.as_ref().unwrap().contains("\"sum\":42"));
    assert_eq!(
        history[3].content.as_deref(),
        Some("The sum of 15 and 27 is 42.")
    );
}

#[tokio::test]
async fn test_tool_router_surfaces_tool_attachments() {
    let tool = ToolMeta {
        name: "draw_card".to_string(),
        description: "Draws a card".to_string(),
        parameters: None,
    };
    let host = Arc::new(MockToolHost::with_plugin(
        "host_draw",
        "org.draw",
        vec![tool],
    ));

    let provider = Arc::new(MockLlmProvider::new(vec![
        ChatResponse {
            reasoning_content: None,
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_draw_1".to_string(),
                name: "draw_card".to_string(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some("tool_calls".to_string()),
            usage: None,
        },
        ChatResponse {
            reasoning_content: None,
            content: Some("Here is your card.".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        },
    ]));

    let memory = Arc::new(InMemory::new());
    let router = ToolRouter::new(provider, memory, "test-model");
    let hosts: Vec<Arc<dyn ToolHost>> = vec![host];

    let output = router
        .execute("session-attach", "draw me a card", &hosts)
        .await
        .expect("router execution");

    // The attachment must survive the reasoning loop: the pipeline turns it into a message
    // segment, so dropping it here would silently reduce a picture to its caption.
    assert_eq!(output.attachments.len(), 1);
    assert_eq!(output.attachments[0].mime_type, "image/png");
    assert_eq!(
        output.attachments[0].file_path.as_deref(),
        Some("/tmp/kanon-test/card.png")
    );
    assert_eq!(output.content, "Here is your card.");
}

#[tokio::test]
async fn test_tool_router_max_recursion_limit() {
    // Model keeps returning tool calls in an infinite loop
    let infinite_turn = ChatResponse {
        reasoning_content: None,
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_inf".to_string(),
            name: "add_numbers".to_string(),
            arguments: serde_json::json!({ "a": 1.0, "b": 1.0 }),
        }],
        finish_reason: Some("tool_calls".to_string()),
        usage: None,
    };

    let mock_provider = Arc::new(MockLlmProvider::new(vec![
        infinite_turn.clone(),
        infinite_turn.clone(),
        infinite_turn.clone(),
    ]));

    let memory = Arc::new(InMemory::new());
    // Set max iterations = 2
    let router = ToolRouter::new(mock_provider, memory, "test-model").with_max_iterations(2);

    let tool_meta = ToolMeta {
        name: "add_numbers".to_string(),
        description: "Adds numbers".to_string(),
        parameters: None,
    };

    let host = Arc::new(MockToolHost::new("host_math", vec![tool_meta]));
    let hosts: Vec<Arc<dyn ToolHost>> = vec![host.clone()];

    let output = router
        .execute("session_inf", "Keep calculating", &hosts)
        .await
        .expect("Execution should terminate gracefully without error");

    // Loop must halt after exactly 2 iterations
    assert_eq!(host.tool_calls_received.load(Ordering::SeqCst), 2);
    assert!(output.content.contains("recursion limit reached"));
}

#[test]
fn test_prost_json_roundtrip() {
    let json_input = serde_json::json!({
        "name": "calc",
        "count": 42.0,
        "active": true,
        "nested": {
            "tags": ["rust", "plugin"]
        }
    });

    let prost_struct = json_to_prost_struct(&json_input).expect("Valid JSON object");
    let json_output = prost_struct_to_json(prost_struct);

    assert_eq!(json_input, json_output);
}

#[tokio::test]
async fn test_aggregate_tools_disambiguates_duplicate_names() {
    use kanon_llm::tool_router::aggregate_tools;

    let tool_a = ToolMeta {
        name: "search".to_string(),
        description: "Search weather reports".to_string(),
        parameters: None,
    };
    let tool_b = ToolMeta {
        name: "search".to_string(),
        description: "Search github repositories".to_string(),
        parameters: None,
    };
    let unique_tool = ToolMeta {
        name: "calc".to_string(),
        description: "Unique calculator tool".to_string(),
        parameters: None,
    };

    let host_a = Arc::new(MockToolHost::with_plugin(
        "host_weather",
        "org.weather",
        vec![tool_a],
    ));
    let host_b = Arc::new(MockToolHost::with_plugin(
        "host_github",
        "org.github",
        vec![tool_b, unique_tool],
    ));

    let hosts: Vec<Arc<dyn ToolHost>> = vec![host_a, host_b];
    let defs = aggregate_tools(&hosts);
    assert_eq!(defs.len(), 3);

    // The attributed form is what the console renders; it must agree with the model's view name by
    // name, which is why both go through one implementation.
    let resolved = kanon_llm::tool_router::resolve_tools(&hosts);
    let projected: Vec<_> = resolved
        .iter()
        .map(|tool| tool.definition.clone())
        .collect();
    assert_eq!(projected, defs);
    let github = resolved
        .iter()
        .find(|tool| tool.definition.name == "org_github__search")
        .expect("namespaced tool");
    assert_eq!(github.plugin_id, "org.github");
    assert_eq!(github.host_id, "host_github");

    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    // Unique tool remains bare
    assert!(
        names.contains(&"calc".to_string()),
        "Unique tool must retain bare name"
    );
    // Colliding tool names must be namespaced using model-safe identifiers
    assert!(
        names.contains(&"org_weather__search".to_string()),
        "Colliding tool must be namespaced"
    );
    assert!(
        names.contains(&"org_github__search".to_string()),
        "Colliding tool must be namespaced"
    );
}

#[tokio::test]
async fn test_tool_router_namespaced_duplicate_dispatch() {
    let tool_call = ToolCall {
        id: "call_github_1".to_string(),
        name: "org_github__search".to_string(),
        arguments: serde_json::json!({ "query": "kanon" }),
    };

    let mock_provider = Arc::new(MockLlmProvider::new(vec![
        ChatResponse {
            reasoning_content: None,
            content: None,
            tool_calls: vec![tool_call],
            finish_reason: Some("tool_calls".to_string()),
            usage: None,
        },
        ChatResponse {
            reasoning_content: None,
            content: Some("Search completed via github plugin".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        },
    ]));

    let memory = Arc::new(InMemory::new());
    let router = ToolRouter::new(mock_provider, memory, "test-model");

    let tool_a = ToolMeta {
        name: "search".to_string(),
        description: "Search weather reports".to_string(),
        parameters: None,
    };
    let tool_b = ToolMeta {
        name: "search".to_string(),
        description: "Search github repositories".to_string(),
        parameters: None,
    };

    let host_a = Arc::new(MockToolHost::with_plugin(
        "host_weather",
        "org.weather",
        vec![tool_a],
    ));
    let host_b = Arc::new(MockToolHost::with_plugin(
        "host_github",
        "org.github",
        vec![tool_b],
    ));
    let hosts: Vec<Arc<dyn ToolHost>> = vec![host_a.clone(), host_b.clone()];

    let output = router
        .execute("session_ns", "Search kanon on github", &hosts)
        .await
        .expect("Execution should succeed");

    assert_eq!(output.content, "Search completed via github plugin");
    // Host B should have received the invocation with unnamespaced "search"
    assert_eq!(host_b.tool_calls_received.load(Ordering::SeqCst), 1);
    // Host A should NOT have been invoked
    assert_eq!(host_a.tool_calls_received.load(Ordering::SeqCst), 0);
    assert_eq!(output.executed_tools.len(), 1);
    assert_eq!(output.executed_tools[0].plugin_id, "org.github");
    assert_eq!(output.executed_tools[0].host_id, "host_github");
}
