//! Invalid provider tool calls must fail before any tool in the response executes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::response::IntoResponse;
use axum::{Json, Router, routing::post};
use kanon_llm::gateway::providers::{OpenAiChatProvider, OpenAiResponsesProvider};
use kanon_llm::{
    Agent, BuiltinAgent, ChatMessage, ChatRequest, LlmProvider, NativeTool, ToolDefinition,
};
use serde_json::{Value, json};
use tokio_stream::StreamExt;

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
    provider_with_body(responses, body).await
}

async fn provider_with_body(
    responses: bool,
    body: Value,
) -> (Arc<dyn LlmProvider>, tokio::task::JoinHandle<()>) {
    let app = Router::new().fallback(post(move |Json(request): Json<Value>| {
        let body = body.clone();
        async move {
            if responses && request["stream"] == true {
                let mut events = String::new();
                for (index, item) in body["output"].as_array().unwrap().iter().enumerate() {
                    let event = json!({
                        "type": "response.output_item.done", "output_index": index, "item": item,
                    });
                    events.push_str(&format!("data: {event}\n\n"));
                }
                events.push_str("data: {\"type\":\"response.completed\"}\n\n");
                ([("content-type", "text/event-stream")], events).into_response()
            } else {
                Json(body).into_response()
            }
        }
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

#[tokio::test]
async fn responses_tool_calls_require_call_ids_in_complete_and_streamed_replies() {
    for identity in [
        json!({}),
        json!({"id": "fc_item"}),
        json!({"call_id": null, "id": "fc_item"}),
        json!({"call_id": "", "id": "fc_item"}),
        json!({"call_id": " \t", "id": "fc_item"}),
        json!({"call_id": "call_real"}),
        json!({"call_id": "call_real", "id": ""}),
        json!({"call_id": "call_real", "id": "fc_distinct_item"}),
    ] {
        let valid = identity["call_id"] == "call_real";
        let mut candidate = json!({"type": "function_call", "name": "lookup", "arguments": "{}"});
        candidate
            .as_object_mut()
            .unwrap()
            .extend(identity.as_object().unwrap().clone());
        let body = json!({"output": [
            {"type": "function_call", "call_id": "call_first", "name": "lookup", "arguments": "{}"},
            candidate,
        ]});
        for streaming in [false, true] {
            let (provider, server) = provider_with_body(true, body.clone()).await;
            let request = ChatRequest {
                model: "test-model".into(),
                messages: vec![ChatMessage::user("look up a city")],
                tools: vec![],
                temperature: None,
                max_tokens: None,
            };
            let result = if streaming {
                provider
                    .chat_stream(&request)
                    .await
                    .unwrap()
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .map(|chunks| {
                        chunks
                            .into_iter()
                            .flat_map(|chunk| chunk.tool_calls)
                            .collect()
                    })
            } else {
                provider
                    .chat(&request)
                    .await
                    .map(|response| response.tool_calls)
            };
            server.abort();
            if valid {
                let calls = result.unwrap();
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, "call_first");
                assert_eq!(calls[1].id, "call_real");
            } else {
                let error = result.expect_err("an invalid call_id must reject the entire response");
                assert!(
                    error.to_string().contains("function call has no call_id"),
                    "identity={identity}, streaming={streaming}: {error}"
                );
            }
        }
    }
}
