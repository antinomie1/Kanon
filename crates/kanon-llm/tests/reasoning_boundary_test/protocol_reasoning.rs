//! Existing protocol regressions, sharing the parent fixtures.

use super::*;

#[tokio::test]
async fn protocol_keeps_null_answers_and_default_stream_reasoning_separate() {
    let url = server(Router::new().route("/v1/chat/completions", post(|| async {
        Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"reasoning_content":"private"},"finish_reason":"length"}]}))
    }))).await;
    let provider = OpenAiChatProvider::new(url, None, "fixture");
    let response = provider
        .chat(&request(vec![ChatMessage::user("question")]))
        .await
        .unwrap();
    assert_eq!(response.content, None);
    assert_eq!(response.reasoning_content.as_deref(), Some("private"));
    struct DefaultStream(ChatResponse);
    #[async_trait]
    impl LlmProvider for DefaultStream {
        async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
            Ok(self.0.clone())
        }
    }
    let mut stream = DefaultStream(response)
        .chat_stream(&request(vec![]))
        .await
        .unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.delta_text, "");
    assert_eq!(first.reasoning_text.as_deref(), Some("private"));
    assert!(stream.next().await.unwrap().unwrap().is_finished);
}

#[tokio::test]
async fn provider_history_conversion_preserves_user_literals_and_separates_legacy_assistants() {
    use kanon_llm::{AnthropicMessagesProvider, OpenAiResponsesProvider};
    for protocol in ["openai", "anthropic", "responses"] {
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = received.clone();
        let (route, reply) = match protocol {
            "openai" => (
                "/v1/chat/completions",
                json!({"choices":[{"index":0,"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}]}),
            ),
            "anthropic" => (
                "/v1/messages",
                json!({"role":"assistant","content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn"}),
            ),
            _ => (
                "/v1/responses",
                json!({"output":[{"type":"message","content":[{"type":"output_text","text":"answer"}]}],"status":"completed"}),
            ),
        };
        let url = server(Router::new().route(
            route,
            post(move |Json(body): Json<Value>| {
                let capture = capture.clone();
                let reply = reply.clone();
                async move {
                    capture.lock().unwrap().push(body);
                    Json(reply)
                }
            }),
        ))
        .await;
        let provider: Box<dyn LlmProvider> = match protocol {
            "openai" => Box::new(OpenAiChatProvider::new(url, None, "fixture")),
            "anthropic" => Box::new(AnthropicMessagesProvider::new(url, None, "fixture")),
            _ => Box::new(OpenAiResponsesProvider::new("").with_base_url(url)),
        };
        let literal = "Explain `<think>literal</think>`";
        let response = provider
            .chat(&request(vec![
                ChatMessage::user(literal),
                ChatMessage::assistant("<think>private-a\n\nprivate-b</think>public"),
                ChatMessage::user("next"),
            ]))
            .await
            .unwrap();
        assert_eq!(response.content.as_deref(), Some("answer"));
        let requests = received.lock().unwrap();
        let body = &requests[0];
        if protocol == "openai" {
            assert_eq!(body["messages"][0]["content"], literal);
            assert_eq!(body["messages"][1]["content"], "public");
            assert!(body["messages"][1].get("reasoning_content").is_none());
        } else {
            let items = if protocol == "anthropic" {
                &body["messages"]
            } else {
                &body["input"]
            };
            assert_eq!(items[0]["content"][0]["text"], literal);
            assert_eq!(items[1]["content"][0]["text"], "public");
            assert!(!body.to_string().contains("private-"));
            assert!(!body.to_string().contains("reasoning_content"));
        }
    }
}

#[tokio::test]
async fn real_sse_separates_legacy_chunks_and_responses_reasoning_events() {
    use kanon_llm::OpenAiResponsesProvider;
    for protocol in ["openai", "responses"] {
        let (route, body) = if protocol == "openai" {
            let mut body = String::new();
            for text in ["<thi", "nk>private-a", "</think>", "answer"] {
                body.push_str(&format!(
                    "data: {}\n\n",
                    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]})
                ));
            }
            body.push_str("data: [DONE]\n\n");
            ("/v1/chat/completions", body)
        } else {
            let events = [
                json!({"type":"response.reasoning_summary_text.delta","delta":"private-a"}),
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call-1","name":"lookup","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"query\":"}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"not answer\"}"}),
                json!({"type":"response.future_metadata.delta","delta":"not answer either"}),
                json!({"type":"response.output_text.delta","delta":"answer"}),
                json!({"type":"response.completed"}),
            ];
            (
                "/v1/responses",
                events.iter().map(|v| format!("data: {v}\n\n")).collect(),
            )
        };
        let url = server(Router::new().route(
            route,
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body).into_response() }
            }),
        ))
        .await;
        let provider: Box<dyn LlmProvider> = if protocol == "openai" {
            Box::new(OpenAiChatProvider::new(url, None, "fixture"))
        } else {
            Box::new(OpenAiResponsesProvider::new("").with_base_url(url))
        };
        let mut stream = provider
            .chat_stream(&request(vec![ChatMessage::user("question")]))
            .await
            .unwrap();
        let mut answer = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(!chunk.delta_text.contains("private"));
            answer.push_str(&chunk.delta_text);
            reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
            calls.extend(chunk.tool_calls);
        }
        if protocol == "responses" {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call-1");
            assert_eq!(calls[0].name, "lookup");
            assert_eq!(calls[0].arguments, json!({"query":"not answer"}));
        } else {
            assert!(calls.is_empty());
        }
        assert_eq!(answer, "answer");
        assert!(reasoning.contains("private-a"));
    }
}

#[test]
fn native_reasoning_makes_content_authoritative_even_when_it_contains_tags() {
    let mut response = ChatResponse {
        content: Some("<think>echoed-private</think><think>more-echo</think>answer".into()),
        reasoning_content: Some("  native-private\n".into()),
        ..ChatResponse::default()
    };
    let content = response.content.clone();
    response.separate_reasoning();
    assert_eq!(response.content, content);
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some("  native-private\n")
    );
    let mut message = response.assistant_message();
    message.separate_reasoning();
    assert_eq!(message.reasoning_content, response.reasoning_content);
}

#[tokio::test]
async fn responses_refusal_deltas_are_visible_and_persisted_once() {
    for event_header in [false, true] {
        let events = [
            json!({"type":"response.reasoning_summary_text.delta","delta":"private"}),
            json!({"type":"response.future_arguments.delta","delta":"internal arguments"}),
            json!({"type":"response.future_metadata.delta","delta":"internal metadata"}),
            json!({"type":"response.refusal.delta","delta":"synthetic "}),
            json!({"type":"response.refusal.delta","delta":"refusal"}),
            json!({"type":"response.refusal.done","refusal":"synthetic refusal"}),
            json!({"type":"response.completed"}),
        ];
        let body: String = events
            .iter()
            .map(|event| {
                let header = if event_header {
                    format!("event: {}\n", event["type"].as_str().unwrap())
                } else {
                    String::new()
                };
                format!("{header}data: {event}\n\n")
            })
            .collect();
        let url = server(Router::new().route(
            "/v1/responses",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        ))
        .await;
        let memory = Arc::new(InMemory::new());
        let agent = BuiltinAgent::builder(
            "fixture",
            Arc::new(kanon_llm::OpenAiResponsesProvider::new("").with_base_url(url)),
        )
        .memory(memory.clone())
        .compaction(None)
        .build();
        let mut stream = agent
            .run_standalone_stream("s", "synthetic question")
            .await
            .unwrap();
        let mut answer = String::new();
        let mut reasoning = String::new();
        let mut finished = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            answer.push_str(&chunk.delta_text);
            reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
            finished += usize::from(chunk.is_finished);
            if chunk.is_finished {
                assert_eq!(chunk.finish_reason.as_deref(), Some("refusal"));
            }
        }
        assert_eq!(answer, "synthetic refusal");
        assert_eq!(reasoning, "private");
        assert_eq!(finished, 1);
        let messages = Memory::get_messages(memory.as_ref(), "s").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.as_deref(), Some("synthetic refusal"));
        assert_eq!(messages[1].reasoning_content.as_deref(), Some("private"));
    }
}

#[tokio::test]
async fn chat_refusal_remains_visible_and_cannot_replace_history() {
    for terminal in ["complete", "stop", "[DONE]"] {
        let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| async move {
            if body["stream"] == true {
                let events = [
                    json!({"choices":[{"index":0,"delta":{"reasoning_content":"private"},"finish_reason":null}]}),
                    json!({"choices":[{"index":0,"delta":{"content":null,"refusal":"synthetic "},"finish_reason":null}]}),
                    json!({"choices":[{"index":0,"delta":{"refusal":"refusal"},"finish_reason":null}]}),
                ];
                let mut sse: String = events.iter().map(|event| format!("data: {event}\n\n")).collect();
                if terminal == "stop" {
                    let event = json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                    sse.push_str(&format!("data: {event}\n\n"));
                }
                sse.push_str("data: [DONE]\n\n");
                ([("content-type", "text/event-stream")], sse).into_response()
            } else {
                Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"refusal":"synthetic refusal","reasoning_content":"private"},"finish_reason":"stop"}]})).into_response()
            }
        }))).await;
        let memory = Arc::new(InMemory::new());
        let agent = BuiltinAgent::builder(
            "fixture",
            Arc::new(OpenAiChatProvider::new(url, None, "fixture")),
        )
        .memory(memory.clone())
        .compaction(None)
        .build();
        if terminal == "complete" {
            let output = agent.run_standalone("s", "question").await.unwrap();
            assert_eq!(output.content, "synthetic refusal");
            assert_eq!(output.finish_reason.as_deref(), Some("refusal"));
        } else {
            let chunks = agent
                .run_standalone_stream("s", "question")
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                chunks
                    .iter()
                    .map(|chunk| chunk.delta_text.as_str())
                    .collect::<String>(),
                "synthetic refusal"
            );
            assert_eq!(chunks.iter().filter(|chunk| chunk.is_finished).count(), 1);
            assert_eq!(
                chunks.last().unwrap().finish_reason.as_deref(),
                Some("refusal")
            );
        }
        let messages = memory.get_messages("s");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.as_deref(), Some("synthetic refusal"));
        assert_eq!(messages[1].reasoning_content.as_deref(), Some("private"));
        agent.run_standalone("s", "another question").await.unwrap();
        let before = memory.snapshot("s").await.unwrap();
        let error = agent.compact_session("s", &[]).await.unwrap_err();
        assert!(error.to_string().contains("refusal"));
        assert_eq!(memory.snapshot("s").await.unwrap(), before);
    }
}

#[tokio::test]
async fn chat_refusal_with_tool_calls_rejects_the_entire_reply() {
    let url = server(Router::new().route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
        let message = json!({"role":"assistant","refusal":"synthetic refusal","tool_calls":[{
            "index":0,"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{}"}
        }]});
        if body["stream"] == true {
            let event = json!({"choices":[{"index":0,"delta":message,"finish_reason":"tool_calls"}]});
            ([("content-type", "text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n")).into_response()
        } else {
            Json(json!({"choices":[{"index":0,"message":message,"finish_reason":"tool_calls"}]})).into_response()
        }
    }))).await;
    let provider = OpenAiChatProvider::new(url, None, "fixture");
    let req = request(vec![ChatMessage::user("question")]);
    let error = provider.chat(&req).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refused response contains tool calls")
    );
    let chunks = provider
        .chat_stream(&req)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(chunks.iter().any(Result::is_err));
    assert!(
        chunks
            .iter()
            .filter_map(|chunk| chunk.as_ref().ok())
            .all(|chunk| chunk.tool_calls.is_empty() && !chunk.is_finished)
    );
}

#[tokio::test]
async fn responses_refusal_remains_visible_and_cannot_replace_history() {
    let url = server(Router::new().route("/v1/responses", post(|| async {
        Json(json!({"output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"synthetic refusal"}]}],"status":"completed"}))
    }))).await;
    let memory = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder(
        "fixture",
        Arc::new(kanon_llm::OpenAiResponsesProvider::new("").with_base_url(url)),
    )
    .memory(memory.clone())
    .compaction(None)
    .build();
    let output = agent
        .run_standalone("s", "synthetic question")
        .await
        .unwrap();
    assert_eq!(output.content, "synthetic refusal");
    assert_eq!(output.finish_reason.as_deref(), Some("refusal"));
    let messages = Memory::get_messages(memory.as_ref(), "s").await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].content.as_deref(), Some("synthetic refusal"));
    assert!(messages[1].reasoning_content.is_none());

    // A Responses refusal has status=completed on the wire. That status alone must not let
    // manual or background compaction replace the conversation with the refusal text.
    agent.run_standalone("s", "another question").await.unwrap();
    let before = memory.snapshot("s").await.unwrap();
    let error = agent.compact_session("s", &[]).await.unwrap_err();
    assert!(matches!(error, kanon_llm::AgentError::Compaction(_)));
    assert!(error.to_string().contains("refusal"));
    assert_eq!(memory.snapshot("s").await.unwrap(), before);
}

#[tokio::test]
async fn replay_toggle_preserves_history_and_new_reasoning_across_restarts() {
    use axum::response::IntoResponse;
    use kanon_llm::{ModelRef, ProviderEntry, ProviderRegistry};

    for streaming in [false, true] {
        for protocol in ["openai", "openai_reasoning"] {
            let received = Arc::new(Mutex::new(Vec::<Value>::new()));
            let captured = received.clone();
            let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    let n = {
                        let mut requests = captured.lock().unwrap();
                        let n = requests.len();
                        requests.push(body.clone());
                        n
                    };
                    let reasoning = format!("new-private-{n}");
                    if body["stream"] == true {
                        let delta = json!({"choices":[{"index":0,"delta":{"content":"answer","reasoning_content":reasoning},"finish_reason":"stop"}]});
                        ([("content-type", "text/event-stream")], format!("data: {delta}\n\ndata: [DONE]\n\n")).into_response()
                    } else {
                        Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"answer","reasoning_content":reasoning},"finish_reason":"stop"}]})).into_response()
                    }
                }
            }))).await;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sessions.db");
            let mut old = ChatMessage::assistant("previous answer");
            old.reasoning_content = Some("old-private".into());
            {
                let memory = SqliteMemory::open(&path).unwrap();
                memory
                    .push_message("s", ChatMessage::user("previous question"))
                    .await
                    .unwrap();
                memory.push_message("s", old).await.unwrap();
            }
            let registry = ProviderRegistry::new();
            // Every iteration recreates memory and the agent, exercising real disk reloads.
            for (round, enabled) in [true, false, true].into_iter().enumerate() {
                let mut entry = ProviderEntry::new("fixture", protocol, &url);
                entry.replay_reasoning = enabled;
                // The persisted provider setting must survive serialization and hot replacement.
                let entry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
                registry.replace(vec![entry]).unwrap();
                let provider = registry
                    .resolve(&ModelRef::parse("fixture/model"))
                    .unwrap()
                    .provider;
                let memory = Arc::new(SqliteMemory::open(&path).unwrap());
                let before = memory.get_messages("s").await.unwrap();
                let agent = BuiltinAgent::builder("fixture", provider)
                    .memory(memory.clone())
                    .compaction(None)
                    .build();
                if streaming {
                    let mut stream = agent
                        .run_standalone_stream("s", "next question")
                        .await
                        .unwrap();
                    while let Some(chunk) = stream.next().await {
                        chunk.unwrap();
                    }
                } else {
                    agent.run_standalone("s", "next question").await.unwrap();
                }
                let after = memory.get_messages("s").await.unwrap();
                assert_eq!(&after[..before.len()], before.as_slice());
                assert_eq!(
                    after.last().unwrap().reasoning_content.as_deref(),
                    Some(format!("new-private-{round}").as_str())
                );
                let requests = received.lock().unwrap();
                let assistants: Vec<_> = requests[round]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["role"] == "assistant")
                    .collect();
                assert_eq!(assistants.len(), round + 1);
                for (index, message) in assistants.iter().enumerate() {
                    if enabled && protocol == "openai_reasoning" {
                        let expected = if index == 0 {
                            "old-private".into()
                        } else {
                            format!("new-private-{}", index - 1)
                        };
                        assert_eq!(message["reasoning_content"], expected);
                    } else {
                        assert!(message.get("reasoning_content").is_none());
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn request_mapping_never_mutates_caller_messages() {
    let url = server(Router::new().route("/v1/chat/completions", post(|| async {
        Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}]}))
    }))).await;
    let mut assistant = ChatMessage::assistant("answer");
    assistant.reasoning_content = Some("private".into());
    let req = request(vec![
        ChatMessage::user("first"),
        assistant,
        ChatMessage::user("second"),
    ]);
    let before = req.messages.clone();
    for enabled in [false, true] {
        OpenAiChatProvider::new(&url, None, "fixture")
            .with_reasoning_content(true)
            .with_reasoning_replay(enabled)
            .chat(&req)
            .await
            .unwrap();
        assert_eq!(req.messages, before);
    }
}

#[tokio::test]
async fn protocol_reasoning_keeps_literal_answer_tags_through_streams_and_restart() {
    for (native, null_field) in [
        (None, false),
        (Some(""), false),
        (Some(""), true),
        (Some("  native-private\n"), false),
    ] {
        for streaming in [false, true] {
            let content = if native.is_some() {
                "<think>intentional example <think>nested</think>text</think>\nUse `</think>` literally"
            } else {
                "Examples: `<think>outer<think>inner</think>end</think>` and </think >"
            };
            let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| async move {
                use axum::response::IntoResponse;
                if body["stream"] == true {
                    // Content precedes the native reasoning signal, and tags cross chunk boundaries.
                    let mut sse = String::new();
                    for ch in content.chars() {
                        let delta = json!({"choices":[{"index":0,"delta":{"content":ch.to_string()},"finish_reason":null}]});
                        sse.push_str(&format!("data: {delta}\n\n"));
                    }
                    if let Some(reasoning) = native {
                        let delta = json!({"choices":[{"index":0,"delta":{"reasoning_content":if null_field { Value::Null } else { json!(reasoning) }},"finish_reason":null}]});
                        sse.push_str(&format!("data: {delta}\n\n"));
                    }
                    sse.push_str("data: [DONE]\n\n");
                    ([("content-type", "text/event-stream")], sse).into_response()
                } else {
                    let mut message = json!({"role":"assistant","content":content});
                    if let Some(reasoning) = native { message["reasoning_content"] = if null_field { Value::Null } else { json!(reasoning) }; }
                    Json(json!({"choices":[{"index":0,"message":message,"finish_reason":"stop"}]})).into_response()
                }
            }))).await;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sessions.db");
            {
                let memory = Arc::new(SqliteMemory::open(&path).unwrap());
                let agent = BuiltinAgent::builder(
                    "fixture",
                    Arc::new(OpenAiChatProvider::new(url, None, "fixture")),
                )
                .memory(memory.clone())
                .compaction(None)
                .build();
                if streaming {
                    let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();
                    let mut answer = String::new();
                    let mut reasoning = String::new();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.unwrap();
                        answer.push_str(&chunk.delta_text);
                        reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
                    }
                    assert_eq!(answer, content);
                    assert_eq!(reasoning, native.unwrap_or_default());
                } else {
                    assert_eq!(
                        agent.run_standalone("s", "question").await.unwrap().content,
                        content
                    );
                }
                let messages = memory.get_messages("s").await.unwrap();
                assert_eq!(messages[1].content.as_deref(), Some(content));
                assert_eq!(messages[1].reasoning_content.as_deref(), native);
            }
            let memory = SqliteMemory::open(&path).unwrap();
            let messages = memory.get_messages("s").await.unwrap();
            assert_eq!(messages[1].content.as_deref(), Some(content));
            assert_eq!(messages[1].reasoning_content.as_deref(), native);
        }
    }
}
