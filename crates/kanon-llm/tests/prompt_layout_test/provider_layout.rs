//! Existing protocol regressions, sharing the parent fixtures.

use super::*;

/// Starts a stub that records request bodies and answers with `response`.
async fn spawn_stub(path: &'static str, response: Value) -> (SocketAddr, Arc<Mutex<Vec<Value>>>) {
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let app = Router::new().route(
        path,
        post(move |axum::Json(body): axum::Json<Value>| {
            let sink = sink.clone();
            let response = response.clone();
            async move {
                sink.lock().unwrap().push(body);
                axum::Json(response)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, captured)
}

fn sample_request() -> ChatRequest {
    ChatRequest {
        model: "claude-test".to_string(),
        messages: vec![
            ChatMessage::system("persona\n\ncatalog"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ],
        tools: canonical_tools(vec![
            ToolDefinition {
                name: "beta".to_string(),
                description: "b".to_string(),
                parameters: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "alpha".to_string(),
                description: "a".to_string(),
                parameters: json!({"type": "object"}),
            },
        ]),
        temperature: None,
        max_tokens: Some(64),
    }
}

#[tokio::test]
async fn anthropic_requests_mark_the_tools_system_and_conversation_as_cacheable() {
    let (addr, captured) = spawn_stub(
        "/v1/messages",
        json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 5,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 30
            }
        }),
    )
    .await;
    let provider = AnthropicMessagesProvider::new(format!("http://{addr}/v1"), None, "claude-test");

    let response = provider.chat(&sample_request()).await.expect("chat");

    let body = captured.lock().unwrap()[0].clone();
    let ephemeral = json!({"type": "ephemeral"});

    // Breakpoint 1: only the last tool, so the whole tool list is one cached prefix.
    assert_eq!(body["tools"][0]["name"], "alpha");
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["tools"][1]["name"], "beta");
    assert_eq!(body["tools"][1]["cache_control"], ephemeral);

    // Breakpoint 2: the system block, in the array form that can carry the marker.
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "persona\n\ncatalog");
    assert_eq!(body["system"][0]["cache_control"], ephemeral);

    // Breakpoint 3: the last block of the conversation, and only that one.
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    let last_blocks = messages[2]["content"].as_array().unwrap();
    assert_eq!(last_blocks.last().unwrap()["cache_control"], ephemeral);
    for earlier in &messages[..2] {
        assert!(
            earlier["content"][0].get("cache_control").is_none(),
            "earlier turns carry no marker: {earlier}"
        );
    }

    // `input_tokens` excludes cached tokens; the prompt size is the sum, and the cached share is
    // reported separately so the hit rate can be computed.
    let usage = response.usage.expect("usage");
    assert_eq!(usage.prompt_tokens, 1050);
    assert_eq!(usage.cached_tokens, 1000);
    assert_eq!(usage.completion_tokens, 5);
    assert_eq!(usage.total_tokens, 1055);
}

#[tokio::test]
async fn anthropic_streaming_requests_use_the_same_layout() {
    // The blocking and streaming calls share one request builder; a streamed turn must not miss
    // the cache its blocking twin wrote.
    let (addr, captured) = spawn_stub("/v1/messages", json!({})).await;
    let provider = AnthropicMessagesProvider::new(format!("http://{addr}/v1"), None, "claude-test");

    // The stub answers with plain JSON, so only the request is of interest.
    let _ = provider.chat_stream(&sample_request()).await;

    let body = captured.lock().unwrap()[0].clone();
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        body["tools"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[tokio::test]
async fn openai_style_providers_report_cached_prompt_tokens() {
    // OpenAI's own field.
    let (addr, _) = spawn_stub(
        "/v1/chat/completions",
        json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 2006,
                "completion_tokens": 10,
                "total_tokens": 2016,
                "prompt_tokens_details": {"cached_tokens": 1920}
            }
        }),
    )
    .await;
    let provider = OpenAiChatProvider::new(format!("http://{addr}/v1"), None, "m");
    let usage = provider
        .chat(&sample_request())
        .await
        .expect("chat")
        .usage
        .expect("usage");
    assert_eq!(usage.prompt_tokens, 2006);
    assert_eq!(usage.cached_tokens, 1920);

    // DeepSeek's flat field.
    let (addr, _) = spawn_stub(
        "/v1/chat/completions",
        json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 500,
                "completion_tokens": 10,
                "total_tokens": 510,
                "prompt_cache_hit_tokens": 448,
                "prompt_cache_miss_tokens": 52
            }
        }),
    )
    .await;
    let provider = OpenAiChatProvider::new(format!("http://{addr}/v1"), None, "m");
    let usage = provider
        .chat(&sample_request())
        .await
        .expect("chat")
        .usage
        .expect("usage");
    assert_eq!(usage.cached_tokens, 448);

    // An endpoint that reports nothing is "no cache information", i.e. zero.
    let (addr, _) = spawn_stub(
        "/v1/chat/completions",
        json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 1, "total_tokens": 8}
        }),
    )
    .await;
    let provider = OpenAiChatProvider::new(format!("http://{addr}/v1"), None, "m");
    let usage = provider
        .chat(&sample_request())
        .await
        .expect("chat")
        .usage
        .expect("usage");
    assert_eq!(usage.cached_tokens, 0);
}

#[tokio::test]
async fn the_responses_api_reports_cached_input_tokens() {
    let (addr, _) = spawn_stub(
        "/v1/responses",
        json!({
            "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}],
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 4,
                "total_tokens": 1204,
                "input_tokens_details": {"cached_tokens": 1024}
            }
        }),
    )
    .await;
    let provider =
        OpenAiResponsesProvider::new("sk-test").with_base_url(format!("http://{addr}/v1"));
    let usage = provider
        .chat(&sample_request())
        .await
        .expect("chat")
        .usage
        .expect("usage");
    assert_eq!(usage.prompt_tokens, 1200);
    assert_eq!(usage.cached_tokens, 1024);
}

/// Simulates a hook whose tool discovery order varies between calls.
struct ReorderTools(AtomicBool);

#[async_trait]
impl AgentHook for ReorderTools {
    async fn on_llm_request(
        &self,
        _: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        if self.0.fetch_xor(true, Ordering::SeqCst) {
            request.tools.reverse();
        }
        Ok(())
    }
}

#[tokio::test]
async fn hooks_cannot_make_tool_order_change_the_cached_prefix() {
    let recorder = Arc::new(Recorder::default());
    let agent = BuiltinAgent::builder("hook-order", recorder.clone())
        .tool(tool("alpha"))
        .tool(tool("beta"))
        .hook(ReorderTools(AtomicBool::new(true)))
        .compaction(None)
        .build();
    agent.run("s", "first", &[]).await.unwrap();
    agent.run("s", "second", &[]).await.unwrap();
    let requests = recorder.requests.lock().unwrap();
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[0].tools[0].name, "alpha");
}

struct LateSystem;

#[async_trait]
impl AgentHook for LateSystem {
    async fn on_llm_request(
        &self,
        _: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        request
            .messages
            .push(ChatMessage::system("dynamic context"));
        Ok(())
    }
}

#[tokio::test]
async fn a_hook_cannot_promote_turn_context_into_system_instructions() {
    let recorder = Arc::new(Recorder::default());
    let agent = BuiltinAgent::builder("late-system", recorder.clone())
        .hook(LateSystem)
        .compaction(None)
        .build();
    let error = agent.run("s", "hello", &[]).await.unwrap_err();
    assert!(matches!(error, kanon_llm::AgentError::InvalidRequest(_)));
    assert!(recorder.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn complete_and_streamed_requests_share_the_entire_wire_prefix() {
    let mut request = sample_request();
    request.messages.extend([
        ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "lookup-1".into(),
                name: "alpha".into(),
                arguments: json!({"city":"Tokyo"}),
            }],
            Some("<think>private</think>checking".into()),
        ),
        ChatMessage::tool_response("lookup-1", "sunny"),
    ]);
    let mut image_message = ChatMessage::user("current photo");
    image_message.parts = Some(vec![ContentPart::image_url(
        "https://example.invalid/photo.png",
        Some("image/png".into()),
    )]);
    request.messages.push(image_message);
    for responses in [false, true] {
        let (path, reply) = if responses {
            ("/v1/responses", json!({"status":"completed", "output":[]}))
        } else {
            (
                "/v1/chat/completions",
                json!({"choices":[{"index":0, "message":{"role":"assistant", "content":"ok"}, "finish_reason":"stop"}]}),
            )
        };
        let (addr, captured) = spawn_stub(path, reply).await;
        let base_url = format!("http://{addr}/v1");
        let provider: Box<dyn LlmProvider> = if responses {
            Box::new(OpenAiResponsesProvider::new("").with_base_url(base_url))
        } else {
            Box::new(OpenAiChatProvider::new(base_url, None, "test"))
        };
        provider.chat(&request).await.unwrap();
        // The fixture need not emit SSE: this assertion compares the submitted requests.
        let _ = provider.chat_stream(&request).await.unwrap();
        let mut bodies = captured.lock().unwrap().clone();
        assert_eq!(bodies.len(), 2);
        assert_eq!(
            bodies[1].as_object_mut().unwrap().remove("stream"),
            Some(json!(true))
        );
        bodies[0].as_object_mut().unwrap().remove("stream");
        assert_eq!(bodies[0], bodies[1]);
    }
}

/// Request middleware must not override model capabilities while constructing the prefix.
struct AddToolDefinition;

#[async_trait]
impl AgentHook for AddToolDefinition {
    async fn on_system_prompt(
        &self,
        _: &str,
        prompt: &mut String,
        tools: &[ToolDefinition],
    ) -> Result<(), kanon_llm::AgentError> {
        if !tools.is_empty() {
            prompt.push_str("\n\nUse the available tools.");
        }
        Ok(())
    }

    async fn on_llm_request(
        &self,
        _: &str,
        request: &mut ChatRequest,
    ) -> Result<(), kanon_llm::AgentError> {
        let definition = ToolDefinition {
            name: "hook_tool".into(),
            description: "Added by middleware".into(),
            parameters: json!({"type":"object"}),
        };
        request.tools.extend([definition.clone(), definition]);
        Ok(())
    }
}

#[tokio::test]
async fn disabled_tools_keep_an_append_only_prefix_through_manual_compaction() {
    for model_supports_tools in [false, true] {
        let recorder = Arc::new(Recorder::default());
        let agent = BuiltinAgent::builder("no-tools", recorder.clone())
            .system_prompt("Stable instructions.")
            .tool(tool("native_tool"))
            .tool_calling(model_supports_tools)
            .hook(AddToolDefinition)
            .compaction(None)
            .build();
        let options = TurnOptions {
            without_tools: model_supports_tools,
            ..Default::default()
        };
        for message in ["first", "second"] {
            agent
                .run_message_with("s", ChatMessage::user(message), &[], options.clone())
                .await
                .unwrap();
        }
        assert!(agent.compact_session_with("s", &[], options).await.unwrap());
        let requests = recorder.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request.tools.is_empty()));
        assert_eq!(
            requests[0].messages[0].content.as_deref(),
            Some("Stable instructions.")
        );
        assert!(requests[1].messages.starts_with(&requests[0].messages));
        assert!(requests[2].messages.starts_with(&requests[1].messages));
    }
}
