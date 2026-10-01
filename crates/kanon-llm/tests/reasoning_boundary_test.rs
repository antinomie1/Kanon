//! Protocol, persistence and stream regressions using only synthetic local fixtures.

use async_trait::async_trait;
use axum::{Json, Router, routing::post};
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
async fn tool_rounds_and_later_turns_replay_separate_reasoning_after_restart() {
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = received.clone();
    let url = server(Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            let mut requests = captured.lock().unwrap();
            let n = requests.len();
            requests.push(body);
            let message = if n < 2 {
                json!({"role":"assistant", "content":null, "reasoning_content":format!("private-{n}"),
                    "tool_calls":[{"id":format!("call-{n}"), "type":"function", "function":{"name":"fixture_tool","arguments":"{}"}}]})
            } else {
                json!({"role":"assistant", "content":"public answer", "reasoning_content":format!("private-{n}")})
            };
            Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if n < 2 {"tool_calls"} else {"stop"}}]}))
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
    let provider = Arc::new(OpenAiChatProvider::new(url, None, "fixture"));
    {
        let memory = Arc::new(SqliteMemory::open(&path).unwrap());
        let agent = Agent::builder("fixture", provider.clone())
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
    let agent = Agent::builder("fixture", provider)
        .memory(memory)
        .tool(tool())
        .compaction(None)
        .build();
    // The console's non-streaming tool path must expose reasoning only in its dedicated channel.
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
    // Adding a later turn must not reshape the already transmitted prefix.
    let earlier = requests[2]["messages"].as_array().unwrap();
    assert_eq!(
        &requests[3]["messages"].as_array().unwrap()[..earlier.len()],
        earlier
    );
}

#[tokio::test]
async fn upgrades_legacy_database_without_rewriting_or_removing_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let legacy = "<think>private-a</think><think>private-b</think>answer";
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

#[tokio::test]
async fn empty_streams_complete_without_appending_blank_assistant_history() {
    for chunks in [
        vec![],
        vec![ChatChunk::done(Some("stop".into()))],
        vec![ChatChunk::delta("")],
        vec![ChatChunk::reasoning(""), ChatChunk::done(None)],
        vec![ChatChunk::delta("<thi"), ChatChunk::done(None)],
    ] {
        let memory = Arc::new(InMemory::new());
        let previous = ChatMessage::assistant("previous synthetic answer");
        Memory::push_message(memory.as_ref(), "s", previous.clone())
            .await
            .unwrap();
        let sessions = Arc::new(kanon_llm::SessionManager::new(memory.clone()));
        let agent = Agent::builder(
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
            kanon_llm::token::estimate_text_tokens("question")
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
        (
            "<think>private-a</think><THINK>private-b</THINK>answer",
            "answer",
            true,
        ),
        (
            "<think>private-a<think>private-b</think>private-c</think>answer",
            "answer",
            true,
        ),
        ("<think>private-a</think><think>unfinished", "", true),
        ("<thi", "", false),
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
        let agent = Agent::builder(
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
async fn final_chunk_data_is_delivered_and_broken_legacy_streams_never_leak() {
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
        let agent = Agent::builder("fixture", Arc::new(StreamFixture { chunks, fail })).build();
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
                ChatMessage::assistant("<think>private-a</think><think>private-b</think>public"),
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
            assert_eq!(
                body["messages"][1]["reasoning_content"],
                "private-a\n\nprivate-b"
            );
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
    use axum::response::IntoResponse;
    use kanon_llm::OpenAiResponsesProvider;
    for protocol in ["openai", "responses"] {
        let (route, body) = if protocol == "openai" {
            let mut body = String::new();
            for text in [
                "<thi",
                "nk>private-a</think><think>",
                "private-b</think>",
                "answer",
            ] {
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
                json!({"type":"response.function_call_arguments.delta","delta":"not answer"}),
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
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(!chunk.delta_text.contains("private"));
            answer.push_str(&chunk.delta_text);
            reasoning.push_str(chunk.reasoning_text.as_deref().unwrap_or_default());
        }
        assert_eq!(answer, "answer");
        assert!(reasoning.contains("private-a"));
    }
}

#[test]
fn native_reasoning_round_trips_verbatim_even_when_content_echoes_a_legacy_envelope() {
    let mut response = ChatResponse {
        content: Some("<think>echoed-private</think><think>more-echo</think>answer".into()),
        reasoning_content: Some("  native-private\n".into()),
        ..ChatResponse::default()
    };
    response.separate_reasoning();
    assert_eq!(response.content.as_deref(), Some("answer"));
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some("  native-private\n")
    );
    let mut message = response.assistant_message();
    message.separate_reasoning();
    assert_eq!(message.reasoning_content, response.reasoning_content);
}
