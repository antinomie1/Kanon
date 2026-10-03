//! Integration tests for streaming LLM provider protocol implementations (SSE).

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use std::net::SocketAddr;
use tokio_stream::StreamExt;

use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::LlmProvider;
use kanon_llm::gateway::providers::{
    AnthropicMessagesProvider, OpenAiChatProvider, OpenAiResponsesProvider,
};
use kanon_llm::gateway::types::{ChatMessage, ChatRequest};

#[tokio::test]
async fn test_openai_chat_streaming_sse() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let sse_body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" streaming\"},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world!\"},\"finish_reason\":\"stop\"}]}\n\n\
                            data: [DONE]\n\n";

            ([("content-type", "text/event-stream")], sse_body).into_response()
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = OpenAiChatProvider::new(
        format!("http://{addr}/v1"),
        Some("test_key".to_string()),
        "gpt-4o-mini",
    );

    let request = ChatRequest {
        model: "gpt-4o-mini".to_string(),
        messages: vec![ChatMessage::user("Hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };

    let mut stream = provider
        .chat_stream(&request)
        .await
        .expect("Failed to start stream");

    let mut accumulated = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in chunk stream");
        accumulated.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finished = true;
            assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        }
    }

    assert!(finished, "Stream did not signal completion");
    assert_eq!(accumulated, "Hello streaming world!");
}

#[tokio::test]
async fn test_openai_responses_streaming_sse() {
    let app = Router::new().route(
        "/v1/responses",
        post(|| async {
            let sse_body = "event: response.output_text.delta\n\
                            data: {\"type\":\"response.output_text.delta\",\"delta\":\"Modern\"}\n\n\
                            event: response.output_text.delta\n\
                            data: {\"type\":\"response.output_text.delta\",\"delta\":\" responses\"}\n\n\
                            event: response.completed\n\
                            data: {\"type\":\"response.completed\"}\n\n";

            ([("content-type", "text/event-stream")], sse_body).into_response()
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider =
        OpenAiResponsesProvider::new("test_key").with_base_url(format!("http://{addr}/v1"));

    let request = ChatRequest {
        model: "gpt-4o".to_string(),
        messages: vec![ChatMessage::user("Hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };

    let mut stream = provider
        .chat_stream(&request)
        .await
        .expect("Failed to start stream");

    let mut accumulated = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in chunk stream");
        accumulated.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finished = true;
        }
    }

    assert!(finished);
    assert_eq!(accumulated, "Modern responses");
}

#[tokio::test]
async fn test_anthropic_messages_streaming_sse() {
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let sse_body = "event: content_block_delta\n\
                            data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Claude\"}}\n\n\
                            event: content_block_delta\n\
                            data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" streaming\"}}\n\n\
                            event: message_delta\n\
                            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
                            event: message_stop\n\
                            data: {\"type\":\"message_stop\"}\n\n";

            ([("content-type", "text/event-stream")], sse_body).into_response()
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = AnthropicMessagesProvider::new(
        format!("http://{addr}/v1"),
        Some("test_key".to_string()),
        "claude-3-5-sonnet",
    );

    let request = ChatRequest {
        model: "claude-3-5-sonnet".to_string(),
        messages: vec![ChatMessage::user("Hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };

    let mut stream = provider
        .chat_stream(&request)
        .await
        .expect("Failed to start stream");

    let mut accumulated = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in chunk stream");
        accumulated.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finished = true;
            assert_eq!(chunk.finish_reason.as_deref(), Some("end_turn"));
        }
    }

    assert!(finished);
    assert_eq!(accumulated, "Claude streaming");
}

#[tokio::test]
async fn test_agent_run_standalone_stream() {
    use async_trait::async_trait;
    use kanon_llm::agent::Agent;
    use kanon_llm::error::GatewayError;
    use kanon_llm::gateway::ChatChunkStream;
    use kanon_llm::gateway::types::{ChatChunk, ChatResponse};
    use kanon_llm::memory::InMemory;
    use std::sync::Arc;

    struct MockAgentStreamProvider;

    #[async_trait]
    impl LlmProvider for MockAgentStreamProvider {
        async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
            Ok(ChatResponse {
                reasoning_content: None,
                content: Some("Full content".to_string()),
                tool_calls: vec![],
                finish_reason: Some("stop".to_string()),
                usage: None,
            })
        }

        async fn chat_stream(
            &self,
            _request: &ChatRequest,
        ) -> Result<ChatChunkStream, GatewayError> {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(ChatChunk::delta("Token 1, "))).await;
                let _ = tx.send(Ok(ChatChunk::delta("Token 2, "))).await;
                let _ = tx.send(Ok(ChatChunk::delta("Token 3"))).await;
                let _ = tx.send(Ok(ChatChunk::done(Some("stop".to_string())))).await;
            });
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }
    }

    let memory: Arc<dyn kanon_llm::memory::Memory> = Arc::new(InMemory::new());
    let provider = Arc::new(MockAgentStreamProvider);
    let agent = BuiltinAgent::builder("stream_bot", provider)
        .system_prompt("You are a streaming bot.")
        .memory(memory.clone())
        .build();

    let session_id = "agent_stream_sess";
    let mut stream = agent
        .run_standalone_stream(session_id, "Tell me something")
        .await
        .expect("Failed to start agent stream");

    let mut accumulated = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in agent chunk stream");
        accumulated.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finished = true;
            assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        }
    }

    assert!(finished);
    assert_eq!(accumulated, "Token 1, Token 2, Token 3");

    // Verify that memory automatically committed the assistant's response upon stream finish
    let messages = memory.get_messages(session_id).await.unwrap();
    assert_eq!(messages.len(), 2); // 1 User + 1 Assistant: instructions are never stored
    assert_eq!(messages[0].role, kanon_llm::gateway::types::Role::User);
    assert_eq!(messages[0].content.as_deref(), Some("Tell me something"));
    assert_eq!(messages[1].role, kanon_llm::gateway::types::Role::Assistant);
    assert_eq!(
        messages[1].content.as_deref(),
        Some("Token 1, Token 2, Token 3")
    );
}

/// Verifies that OpenAI-compatible streaming decodes reasoning_content alongside text content.
#[tokio::test]
async fn test_openai_chat_streaming_reasoning_sse() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let sse_body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":null,\"reasoning_content\":\"I think \"},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"reasoning_content\":\"therefore \"},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"The answer is \",\"reasoning_content\":null},\"finish_reason\":null}]}\n\n\
                            data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"42.\",\"reasoning_content\":null},\"finish_reason\":\"stop\"}]}\n\n\
                            data: [DONE]\n\n";

            ([("content-type", "text/event-stream")], sse_body).into_response()
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = OpenAiChatProvider::new(
        format!("http://{addr}/v1"),
        Some("test_key".to_string()),
        "deepseek-reasoner",
    );

    let request = ChatRequest {
        model: "deepseek-reasoner".to_string(),
        messages: vec![ChatMessage::user("What is the answer?")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };

    let mut stream = provider
        .chat_stream(&request)
        .await
        .expect("Failed to start stream");

    let mut accumulated_content = String::new();
    let mut accumulated_reasoning = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in chunk stream");
        accumulated_content.push_str(&chunk.delta_text);
        if let Some(r) = chunk.reasoning_text {
            accumulated_reasoning.push_str(&r);
        }
        if chunk.is_finished {
            finished = true;
            assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        }
    }

    assert!(finished, "Stream did not signal completion");
    assert_eq!(accumulated_reasoning, "I think therefore ");
    assert_eq!(accumulated_content, "The answer is 42.");
}

/// All protocols expose complete calls once, never partial JSON disguised as empty arguments.
#[tokio::test]
async fn streamed_tool_arguments_are_assembled_and_malformed_arguments_fail() {
    for protocol in ["openai", "responses", "anthropic"] {
        for malformed in [false, true] {
            let tail = if malformed { "Tokyo" } else { "Tokyo\"}" };
            let events = match protocol {
                "openai" => vec![
                    serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"weather","arguments":"{\"city\":\""}}]},"finish_reason":null}]}),
                    serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":tail}}]},"finish_reason":"tool_calls"}]}),
                ],
                "responses" => vec![
                    serde_json::json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call-1","name":"weather","arguments":""}}),
                    serde_json::json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"city\":\""}),
                    serde_json::json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":tail}),
                    serde_json::json!({"type":"response.completed"}),
                ],
                _ => vec![
                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"weather","input":{}}}),
                    serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\""}}),
                    serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":tail}}),
                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
                    serde_json::json!({"type":"message_stop"}),
                ],
            };
            let body: String = events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect();
            let route = match protocol {
                "openai" => "/v1/chat/completions",
                "responses" => "/v1/responses",
                _ => "/v1/messages",
            };
            let app = Router::new().route(
                route,
                post(move || {
                    let body = body.clone();
                    async move { ([("content-type", "text/event-stream")], body) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let provider: Box<dyn LlmProvider> = match protocol {
                "openai" => Box::new(OpenAiChatProvider::new(base, None, "fixture")),
                "responses" => Box::new(OpenAiResponsesProvider::new("").with_base_url(base)),
                _ => Box::new(AnthropicMessagesProvider::new(base, None, "fixture")),
            };
            let request = ChatRequest {
                model: "fixture".into(),
                messages: vec![ChatMessage::user("weather?")],
                tools: vec![kanon_llm::ToolDefinition {
                    name: "weather".into(),
                    description: "weather".into(),
                    parameters: serde_json::json!({"type":"object"}),
                }],
                temperature: None,
                max_tokens: None,
            };
            let mut stream = provider.chat_stream(&request).await.unwrap();
            let mut calls = Vec::new();
            let mut finished = 0;
            let mut failed = false;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => {
                        assert!(chunk.delta_text.is_empty());
                        calls.extend(chunk.tool_calls);
                        finished += usize::from(chunk.is_finished);
                    }
                    Err(_) => failed = true,
                }
            }
            assert_eq!(failed, malformed, "{protocol}");
            assert_eq!(finished, usize::from(!malformed), "{protocol}");
            if malformed {
                assert!(
                    calls.is_empty(),
                    "{protocol}: malformed arguments must not execute"
                );
            } else {
                assert_eq!(calls.len(), 1, "{protocol}");
                assert_eq!(calls[0].id, "call-1");
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments, serde_json::json!({"city":"Tokyo"}));
            }
        }
    }
}
