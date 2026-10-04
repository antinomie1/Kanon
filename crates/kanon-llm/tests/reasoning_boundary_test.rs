//! Protocol, persistence and stream regressions using only synthetic local fixtures.

use async_trait::async_trait;
use axum::response::IntoResponse;
use axum::{Json, Router, routing::post};
use kanon_llm::BuiltinAgent;
use kanon_llm::{
    Agent, ChatChunk, ChatChunkStream, ChatMessage, ChatRequest, ChatResponse, GatewayError,
    InMemory, LlmProvider, Memory, NativeTool, OpenAiChatProvider, Role, SqliteMemory,
    ToolDefinition,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;

fn request(messages: Vec<ChatMessage>) -> ChatRequest {
    ChatRequest {
        model: "fixture".into(),
        messages,
        tools: vec![],
        temperature: None,
        max_tokens: None,
    }
}

async fn server(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}/v1")
}

#[tokio::test]
async fn all_retained_tool_rounds_replay_reasoning_after_restart() {
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = received.clone();
    let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            let mut requests = captured.lock().unwrap();
            let n = requests.len();
            let streaming = body["stream"] == true;
            requests.push(body);
            let message = if n < 2 {
                json!({"role":"assistant", "content":null, "reasoning_content":format!("private-{n}"),
                    "tool_calls":[{"id":format!("call-{n}"), "type":"function", "function":{"name":"fixture_tool","arguments":"{}"}}]})
            } else {
                json!({"role":"assistant", "content":"public answer", "reasoning_content":format!("private-{n}")})
            };
            let finish_reason = if n < 2 { "tool_calls" } else { "stop" };
            if streaming {
                let mut delta = message;
                if let Some(calls) = delta["tool_calls"].as_array_mut() {
                    for (index, call) in calls.iter_mut().enumerate() {
                        call["index"] = json!(index);
                    }
                }
                let event = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish_reason}]});
                ([("content-type", "text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n")).into_response()
            } else {
                Json(json!({"choices":[{"index":0,"message":message,"finish_reason":finish_reason}]})).into_response()
            }
        }
    }))).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let tool = || {
        NativeTool::new(
            ToolDefinition {
                name: "fixture_tool".into(),
                description: "Synthetic tool".into(),
                parameters: json!({"type":"object"}),
            },
            |_, _| async { Ok("synthetic result".into()) },
        )
    };
    let provider = kanon_llm::build_provider("openai_reasoning", url, None, "fixture").unwrap();
    {
        let memory = Arc::new(SqliteMemory::open(&path).unwrap());
        let agent = BuiltinAgent::builder("fixture", provider.clone())
            .memory(memory.clone())
            .tool(tool())
            .compaction(None)
            .build();
        let output = agent
            .run_standalone("s", "synthetic question")
            .await
            .unwrap();
        assert_eq!(output.content, "public answer");
        assert_eq!(output.executed_tools.len(), 2);
        let history = memory.get_messages("s").await.unwrap();
        assert_eq!(history.len(), 6);
        for (index, message) in history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .enumerate()
        {
            assert_eq!(
                message.reasoning_content.as_deref(),
                Some(format!("private-{index}").as_str())
            );
            assert!(
                !message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .contains("private")
            );
        }
    }
    let memory = Arc::new(SqliteMemory::open(&path).unwrap());
    let agent = BuiltinAgent::builder("fixture", provider)
        .memory(memory)
        .tool(tool())
        .compaction(None)
        .build();
    // Resuming with a genuine SSE response must replay stored reasoning and keep new reasoning
    // separate from visible text even when tools remain available.
    let mut stream = agent
        .run_standalone_stream("s", "next question")
        .await
        .unwrap();
    let mut answer = String::new();
    let mut reasoning = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        answer.push_str(&chunk.delta_text);
        reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
    }
    assert_eq!(answer, "public answer");
    assert_eq!(reasoning, "private-3");
    let requests = received.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (turn, req) in requests.iter().enumerate() {
        let messages = req["messages"].as_array().unwrap();
        let assistants: Vec<_> = messages
            .iter()
            .filter(|m| m["role"] == "assistant")
            .collect();
        assert_eq!(assistants.len(), turn);
        for (index, msg) in assistants.iter().enumerate() {
            assert_eq!(msg["reasoning_content"], format!("private-{index}"));
            assert!(
                !msg["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("private")
            );
        }
        for index in 0..turn.min(2) {
            assert!(
                messages
                    .iter()
                    .any(|m| m["role"] == "tool" && m["tool_call_id"] == format!("call-{index}"))
            );
        }
    }
    // Tool sub-rounds within one turn must not reshape the already transmitted prefix.
    for round in 1..3 {
        let earlier = requests[round - 1]["messages"].as_array().unwrap();
        assert_eq!(
            &requests[round]["messages"].as_array().unwrap()[..earlier.len()],
            earlier
        );
    }
    // A later user turn preserves the entire earlier prefix, including reasoning.
    let earlier = requests[2]["messages"].as_array().unwrap().clone();
    assert_eq!(
        &requests[3]["messages"].as_array().unwrap()[..earlier.len()],
        earlier.as_slice()
    );
}

#[tokio::test]
async fn upgrades_legacy_database_without_rewriting_or_removing_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let legacy = "<think>private-a\n\nprivate-b</think>answer";
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_key TEXT NOT NULL, role TEXT NOT NULL, content TEXT, tool_calls TEXT, tool_call_id TEXT, name TEXT, created_at INTEGER NOT NULL);").unwrap();
        for role in ["user", "assistant", "tool"] {
            db.execute(
                "INSERT INTO messages (session_key,role,content,created_at) VALUES ('s',?1,?2,0)",
                [role, legacy],
            )
            .unwrap();
        }
    }
    let memory = SqliteMemory::open(&path).unwrap();
    let messages = memory.get_messages("s").await.unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content.as_deref(), Some(legacy));
    assert_eq!(messages[1].content.as_deref(), Some("answer"));
    assert_eq!(
        messages[1].reasoning_content.as_deref(),
        Some("private-a\n\nprivate-b")
    );
    assert_eq!(messages[2].content.as_deref(), Some(legacy));
    memory.push_message("s", messages[1].clone()).await.unwrap();
    drop(memory);
    let reopened = SqliteMemory::open(&path).unwrap();
    let loaded = reopened.get_messages("s").await.unwrap();
    assert_eq!(loaded.len(), 4);
    assert_eq!(loaded[3], messages[1]);
    let db = rusqlite::Connection::open(&path).unwrap();
    let original: String = db
        .query_row("SELECT content FROM messages WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(original, legacy);
}

/// A native stream with arbitrary boundaries, including a final chunk carrying data.
struct StreamFixture {
    chunks: Vec<ChatChunk>,
    fail: bool,
}
#[async_trait]
impl LlmProvider for StreamFixture {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        unreachable!()
    }
    async fn chat_stream(&self, _: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let mut chunks: Vec<_> = self.chunks.iter().cloned().map(Ok).collect();
        if self.fail {
            chunks.push(Err(GatewayError::InvalidResponse(
                "synthetic disconnect".into(),
            )));
        }
        Ok(Box::pin(tokio_stream::iter(chunks)))
    }
}

/// A non-streaming response also used by the tool-enabled streaming preflight.
struct FinalResponseFixture(ChatResponse);

#[async_trait]
impl LlmProvider for FinalResponseFixture {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn empty_final_responses_are_not_persisted_in_either_agent_entrypoint() {
    for streaming in [false, true] {
        for (content, reasoning, keep) in [
            (None, None, false),
            (Some(""), None, false),
            (None, Some(""), false),
            (Some(""), Some(""), false),
            (Some("<think></think>"), None, false),
            (Some("answer"), None, true),
            (None, Some("private"), true),
            (Some(""), Some("private"), true),
            (Some("<think>private</think>"), None, true),
        ] {
            let response = ChatResponse {
                content: content.map(str::to_owned),
                reasoning_content: reasoning.map(str::to_owned),
                finish_reason: Some("stop".into()),
                ..ChatResponse::default()
            };
            let memory = Arc::new(InMemory::new());
            let agent = BuiltinAgent::builder("fixture", Arc::new(FinalResponseFixture(response)))
                .memory(memory.clone())
                // A declared but unused tool must not change final-response persistence.
                .tool(NativeTool::new(
                    ToolDefinition {
                        name: "unused".into(),
                        description: "Synthetic fixture".into(),
                        parameters: json!({"type":"object"}),
                    },
                    |_, _| async { panic!("the final response must not invoke a tool") },
                ))
                .compaction(None)
                .build();
            if streaming {
                let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();
                let mut finished = 0;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.unwrap();
                    assert!(!chunk.delta_text.contains("private"));
                    finished += usize::from(chunk.is_finished);
                }
                assert_eq!(finished, 1);
            } else {
                let output = agent.run_standalone("s", "question").await.unwrap();
                assert!(!output.content.contains("private"));
            }
            let messages = Memory::get_messages(memory.as_ref(), "s").await.unwrap();
            assert_eq!(
                messages.len(),
                1 + usize::from(keep),
                "streaming={streaming}, content={content:?}, reasoning={reasoning:?}"
            );
            assert_eq!(messages[0], ChatMessage::user("question"));
            if keep {
                assert_eq!(
                    messages[1].content.as_deref().unwrap_or_default(),
                    if content == Some("answer") {
                        "answer"
                    } else {
                        ""
                    }
                );
                assert_eq!(
                    messages[1].reasoning_content.as_deref(),
                    if content == Some("answer") {
                        None
                    } else {
                        Some("private")
                    }
                );
            }
        }
    }
}

#[tokio::test]
async fn strict_openai_requests_omit_reasoning_without_mutating_shared_history() {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = captured.clone();
    let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            received.lock().unwrap().push(body.clone());
            if body["messages"].as_array().unwrap().iter().any(|m| m.get("reasoning_content").is_some()) {
                return (axum::http::StatusCode::BAD_REQUEST, "unexpected reasoning_content".to_string());
            }
            if body["stream"] == true {
                (axum::http::StatusCode::OK, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into())
            } else {
                (axum::http::StatusCode::OK, json!({"choices":[{"index":0,"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}]}).to_string())
            }
        }
    }))).await;
    let mut assistant = ChatMessage::assistant("public");
    assistant.reasoning_content = Some("native-private".into());
    assistant.tool_calls = Some(vec![kanon_llm::ToolCall {
        id: "call-1".into(),
        name: "fixture_tool".into(),
        arguments: json!({}),
    }]);
    let mut private_only = ChatMessage::assistant("");
    private_only.reasoning_content = Some("native-private-only".into());
    let mut request = request(vec![
        ChatMessage::user("question"),
        assistant,
        ChatMessage::tool_response("call-1", "synthetic result"),
        ChatMessage::assistant("<think>legacy-private</think>public-final"),
        private_only,
        ChatMessage::assistant("<think>legacy-private-only</think>"),
        ChatMessage::user("next"),
    ]);
    request.model = "deepseek-flash".into();
    let original = request.messages.clone();
    for protocol in ["openai", "openai_chat"] {
        // A model name must not opt an otherwise unknown endpoint into extensions.
        let provider = kanon_llm::build_provider(protocol, &url, None, "deepseek-flash").unwrap();
        assert_eq!(
            provider.chat(&request).await.unwrap().content.as_deref(),
            Some("answer")
        );
        let mut stream = provider.chat_stream(&request).await.unwrap();
        let mut answer = String::new();
        while let Some(chunk) = stream.next().await {
            answer.push_str(&chunk.unwrap().delta_text);
        }
        assert_eq!(answer, "answer");
    }
    assert_eq!(request.messages, original);
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 4, "no retry may hide rejected parameters");
    for body in requests.iter() {
        assert!(!body.to_string().contains("private"));
        assert_eq!(body["messages"][1]["content"], "public");
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
        assert_eq!(body["messages"][3]["content"], "public-final");
        assert_eq!(body["messages"].as_array().unwrap().len(), 5);
    }
}

#[tokio::test]
async fn explicitly_enabled_reasoning_replays_verbatim_in_stream_requests() {
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = received.clone();
    let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(body);
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"new-private\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
        }
    }))).await;
    let provider = kanon_llm::build_provider("openai_reasoning", url, None, "fixture").unwrap();
    let mut assistant = ChatMessage::assistant("");
    assistant.reasoning_content = Some("  native-private\n".into());
    assistant.tool_calls = Some(vec![kanon_llm::ToolCall {
        id: "call-1".into(),
        name: "fixture_tool".into(),
        arguments: json!({}),
    }]);
    let mut request = request(vec![
        ChatMessage::user("question"),
        assistant,
        ChatMessage::tool_response("call-1", "synthetic result"),
        ChatMessage::assistant("<think>legacy-private</think>public"),
    ]);
    request.tools = vec![ToolDefinition {
        name: "fixture_tool".into(),
        description: "Synthetic tool".into(),
        parameters: json!({"type":"object"}),
    }];
    let original = request.messages.clone();
    let mut stream = provider.chat_stream(&request).await.unwrap();
    let mut answer = String::new();
    let mut reasoning = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        answer.push_str(&chunk.delta_text);
        reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
    }
    assert_eq!(answer, "answer");
    assert_eq!(reasoning, "new-private");
    assert_eq!(request.messages, original);
    let requests = received.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body = &requests[0];
    assert_eq!(
        body["messages"][1]["reasoning_content"],
        "  native-private\n"
    );
    assert_eq!(body["messages"][1]["content"], "");
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
    assert_eq!(body["messages"][3]["reasoning_content"], "legacy-private");
    assert_eq!(body["messages"][3]["content"], "public");
    assert!(body["messages"][0].get("reasoning_content").is_none());
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn empty_streams_complete_without_appending_blank_assistant_history() {
    for chunks in [
        vec![ChatChunk::done(Some("stop".into()))],
        vec![ChatChunk::delta(""), ChatChunk::done(None)],
        vec![ChatChunk::reasoning(""), ChatChunk::done(None)],
    ] {
        let memory = Arc::new(InMemory::new());
        let previous = ChatMessage::assistant("previous synthetic answer");
        Memory::push_message(memory.as_ref(), "s", previous.clone())
            .await
            .unwrap();
        let sessions = Arc::new(kanon_llm::SessionManager::new(memory.clone()));
        let agent = BuiltinAgent::builder(
            "fixture",
            Arc::new(StreamFixture {
                chunks,
                fail: false,
            }),
        )
        .memory(memory.clone())
        .session_manager(sessions.clone())
        .compaction(None)
        .build();
        let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();
        let mut finished = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.delta_text.is_empty());
            assert!(
                chunk
                    .reasoning_text
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
            );
            finished += usize::from(chunk.is_finished);
        }
        assert_eq!(finished, 1);
        assert_eq!(
            sessions.get_metadata("s").unwrap().total_tokens_used,
            kanon_llm::token::estimate_conversation_tokens(&[
                previous.clone(),
                ChatMessage::user("question"),
            ])
        );
        assert_eq!(
            Memory::get_messages(memory.as_ref(), "s").await.unwrap(),
            vec![previous, ChatMessage::user("question")]
        );
    }
}

#[tokio::test]
async fn streaming_separates_fragmented_legacy_envelopes_and_persists_both_channels() {
    for (text, expected, private) in [
        ("<think>private-a</think>answer", "answer", true),
        ("<thi", "<thi", false),
        ("Use <think> in examples", "Use <think> in examples", false),
        (
            "```xml\n<think>literal</think>\n```",
            "```xml\n<think>literal</think>\n```",
            false,
        ),
        ("", "", false),
    ] {
        let mut chunks = if private {
            Vec::new()
        } else {
            vec![ChatChunk::reasoning("native-private")]
        };
        chunks.extend(text.chars().map(|ch| ChatChunk::delta(ch.to_string())));
        chunks.push(ChatChunk::done(Some("stop".into())));
        let memory = Arc::new(InMemory::new());
        let agent = BuiltinAgent::builder(
            "fixture",
            Arc::new(StreamFixture {
                chunks,
                fail: false,
            }),
        )
        .memory(memory.clone())
        .compaction(None)
        .build();
        let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();
        let mut answer = String::new();
        let mut reasoning = String::new();
        let mut finished = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(!chunk.delta_text.contains("private"));
            answer.push_str(&chunk.delta_text);
            reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
            finished += usize::from(chunk.is_finished);
        }
        assert_eq!(answer, expected);
        assert_eq!(finished, 1);
        if private {
            assert!(reasoning.contains("private-a"));
        } else {
            assert_eq!(reasoning, "native-private");
        }
        let messages = Memory::get_messages(memory.as_ref(), "s").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.as_deref(), Some(expected));
        assert_eq!(
            messages[1].reasoning_content.as_deref(),
            Some(reasoning.as_str())
        );
    }
}

#[tokio::test]
async fn final_chunk_data_is_delivered_and_transport_errors_are_propagated() {
    for fail in [false, true] {
        let chunks = if fail {
            vec![ChatChunk::delta("<think>private")]
        } else {
            vec![ChatChunk {
                delta_text: "answer".into(),
                reasoning_text: Some("private".into()),
                ..ChatChunk::done(Some("stop".into()))
            }]
        };
        let agent =
            BuiltinAgent::builder("fixture", Arc::new(StreamFixture { chunks, fail })).build();
        let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();
        let mut answer = String::new();
        let mut error = false;
        while let Some(result) = stream.next().await {
            match result {
                Ok(chunk) => answer.push_str(&chunk.delta_text),
                Err(_) => error = true,
            }
        }
        assert_eq!(answer, if fail { "" } else { "answer" });
        assert_eq!(error, fail);
    }
}

#[path = "reasoning_boundary_test/protocol_reasoning.rs"]
mod protocol_reasoning;
