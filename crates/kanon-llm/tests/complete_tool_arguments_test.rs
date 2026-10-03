//! Invalid provider arguments must fail before any tool in the response executes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Json, Router, routing::post};
use kanon_llm::gateway::providers::{OpenAiChatProvider, OpenAiResponsesProvider};
use kanon_llm::{
    Agent, BuiltinAgent, ChatMessage, ChatRequest, LlmProvider, NativeTool, ToolDefinition,
};
use serde_json::{Value, json};

async fn provider(
    responses: bool,
    arguments: Value,
) -> (Arc<dyn LlmProvider>, tokio::task::JoinHandle<()>) {
    // A valid call precedes the malformed one: parsing must reject the entire response.
    let body = if responses {
        json!({"output": [
            {"type": "function_call", "call_id": "first", "name": "lookup", "arguments": "{}"},
            {"type": "function_call", "call_id": "second", "name": "lookup", "arguments": arguments}
        ]})
    } else {
        json!({"choices": [{"index": 0, "message": {"role": "assistant", "tool_calls": [
            {"id": "first", "type": "function", "function": {"name": "lookup", "arguments": "{}"}},
            {"id": "second", "type": "function", "function": {"name": "lookup", "arguments": arguments}}
        ]}, "finish_reason": "tool_calls"}]})
    };
    let app = Router::new().fallback(post(move || {
        let body = body.clone();
        async { Json(body) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider: Arc<dyn LlmProvider> = if responses {
        Arc::new(OpenAiResponsesProvider::new("").with_base_url(base_url))
    } else {
        Arc::new(OpenAiChatProvider::new(base_url, None, "test-model"))
    };
    (provider, server)
}

#[tokio::test]
async fn malformed_complete_arguments_never_execute_tools() {
    for (responses, arguments) in [
        (false, json!("{\"city\":")),
        (true, json!("{\"city\":")),
        (true, Value::Null),
        (true, json!([])),
    ] {
        let (provider, server) = provider(responses, arguments.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let agent = BuiltinAgent::builder("arguments", provider)
            .compaction(None)
            .max_iterations(1)
            .tool(NativeTool::new(
                ToolDefinition {
                    name: "lookup".into(),
                    description: "Look up a city".into(),
                    parameters: json!({"type": "object"}),
                },
                move |_, _| {
                    called.fetch_add(1, Ordering::SeqCst);
                    async { Ok("executed".into()) }
                },
            ))
            .build();
        let result = agent.run("session", "look up a city", &[]).await;
        server.abort();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "responses={responses}, arguments={arguments}"
        );
        assert!(
            result.is_err(),
            "responses={responses}, arguments={arguments}"
        );
    }
}

#[tokio::test]
async fn complete_arguments_preserve_encoded_json_and_responses_objects() {
    let expected = json!({"city": "Tokyo", "options": {"units": "C"}});
    for (responses, arguments) in [
        (false, json!(expected.to_string())),
        (true, json!(expected.to_string())),
        (true, expected.clone()),
    ] {
        let (provider, server) = provider(responses, arguments).await;
        let result = provider
            .chat(&ChatRequest {
                model: "test-model".into(),
                messages: vec![ChatMessage::user("look up a city")],
                tools: vec![],
                temperature: None,
                max_tokens: None,
            })
            .await;
        server.abort();
        let response = result.unwrap();
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[1].arguments, expected);
    }
}
