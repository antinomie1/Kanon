//! Streaming and complete replies share one tool loop, commit point and failure recovery.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_llm::{
    Agent, AgentError, AgentHook, BuiltinAgent, ChatChunk, ChatChunkStream, ChatMessage,
    ChatRequest, ChatResponse, CompactionPolicy, GatewayError, InMemory, LlmProvider, Memory,
    NativeTool, SessionManager, StopSignal, ToolCall, ToolDefinition,
};
use tokio::sync::{Notify, mpsc};
use tokio_stream::StreamExt;

struct ToolStream {
    requests: Mutex<Vec<ChatRequest>>,
    rounds: AtomicUsize,
    release: Arc<Notify>,
}

#[async_trait]
impl LlmProvider for ToolStream {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        assert_eq!(
            request.messages.last().unwrap().content.as_deref(),
            Some(kanon_llm::COMPACTION_INSTRUCTION)
        );
        self.requests.lock().unwrap().push(request.clone());
        Ok(ChatResponse {
            content: Some("summary".into()),
            ..Default::default()
        })
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        if self.rounds.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Box::pin(tokio_stream::iter([Ok(ChatChunk {
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    arguments: serde_json::json!({"city":"Tokyo"}),
                }],
                ..ChatChunk::done(Some("tool_calls".into()))
            })])));
        }
        let (tx, rx) = mpsc::channel(4);
        let release = self.release.clone();
        tokio::spawn(async move {
            tx.send(Ok(ChatChunk::delta("answer "))).await.unwrap();
            release.notified().await;
            tx.send(Ok(ChatChunk::delta("complete"))).await.unwrap();
            tx.send(Ok(ChatChunk::done(Some("stop".into()))))
                .await
                .unwrap();
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

#[tokio::test]
async fn tools_stream_live_record_turns_and_compact_with_the_same_prefix() {
    let memory = Arc::new(InMemory::new());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let provider = Arc::new(ToolStream {
        requests: Mutex::new(Vec::new()),
        rounds: AtomicUsize::new(0),
        release: Arc::new(Notify::new()),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let agent = BuiltinAgent::builder("stream", provider.clone())
        .session_manager(sessions.clone())
        .system_prompt("stable instructions")
        .tool(NativeTool::new(
            ToolDefinition {
                name: "lookup".into(),
                description: "Lookup weather".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
            move |_, arguments| {
                assert_eq!(arguments, serde_json::json!({"city":"Tokyo"}));
                called.fetch_add(1, Ordering::SeqCst);
                async { Ok("sunny".into()) }
            },
        ))
        .compaction(Some(CompactionPolicy {
            trigger_ratio: 0.5,
            default_context_tokens: 2,
            min_messages: 2,
        }))
        .build();
    let mut stream = agent.run_stream("s", "weather?", &[]).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        first.delta_text, "answer ",
        "the first delta precedes completion"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(sessions.try_write("s").is_err());
    assert_eq!(memory.get_messages("s").len(), 3);
    provider.release.notify_one();
    let mut answer = first.delta_text;
    let mut completed = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        answer.push_str(&chunk.delta_text);
        completed += usize::from(chunk.is_finished);
    }
    assert_eq!(answer, "answer complete");
    assert_eq!(completed, 1);
    assert_eq!(sessions.get_metadata("s").unwrap().turn_count, 1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while memory.snapshot("s").await.unwrap().summary.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[1].tools, requests[2].tools);
    assert_eq!(requests[0].messages[0], requests[1].messages[0]);
    assert_eq!(requests[1].messages[0], requests[2].messages[0]);
    assert_eq!(
        &requests[1].messages[..requests[0].messages.len()],
        requests[0].messages.as_slice()
    );
    assert_eq!(
        &requests[2].messages[..requests[1].messages.len()],
        requests[1].messages.as_slice()
    );
}

struct WaitingStream(Mutex<Option<mpsc::Receiver<Result<ChatChunk, GatewayError>>>>);

#[async_trait]
impl LlmProvider for WaitingStream {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        panic!("stream only")
    }
    async fn chat_stream(&self, _: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            self.0.lock().unwrap().take().unwrap(),
        )))
    }
}

#[tokio::test]
async fn stopped_and_failed_streams_close_history_without_success_done() {
    for failure in ["transport", "stop", "eof"] {
        let memory = Arc::new(InMemory::new());
        let sessions = Arc::new(SessionManager::new(memory.clone()));
        let (tx, rx) = mpsc::channel(4);
        let agent = BuiltinAgent::builder("stream", Arc::new(WaitingStream(Mutex::new(Some(rx)))))
            .session_manager(sessions.clone())
            .compaction(None)
            .build();
        let stop = StopSignal::new();
        let mut stream =
            kanon_llm::with_stop_signal(stop.clone(), agent.run_stream("s", "question", &[]))
                .await
                .unwrap();
        tx.send(Ok(ChatChunk::delta("partial"))).await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap().delta_text, "partial");
        match failure {
            "stop" => stop.stop(),
            "eof" => drop(tx),
            _ => {
                tx.send(Err(GatewayError::InvalidResponse(
                    "broken transport".into(),
                )))
                .await
                .unwrap();
            }
        }
        let mut failed = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => assert!(!chunk.is_finished),
                Err(error) => {
                    if failure == "stop" {
                        assert!(matches!(error, AgentError::Stopped), "{error}");
                    } else {
                        assert!(
                            matches!(error, AgentError::Gateway(GatewayError::InvalidResponse(_))),
                            "{error}"
                        );
                    }
                    failed = true;
                }
            }
        }
        assert!(failed);
        assert!(sessions.try_write("s").is_ok());
        let messages = memory.get_messages("s");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], ChatMessage::user("question"));
        assert!(
            messages[1]
                .content
                .as_deref()
                .unwrap()
                .contains("This turn")
        );
        assert!(sessions.get_metadata("s").is_none());
    }
}

struct Rewrite;
#[async_trait]
impl AgentHook for Rewrite {
    async fn on_llm_response(
        &self,
        _: &str,
        response: &mut ChatResponse,
    ) -> Result<(), AgentError> {
        response.content = Some("rewritten after delivery".into());
        Ok(())
    }
}

#[tokio::test]
async fn response_hooks_cannot_silently_rewrite_already_delivered_text() {
    let memory = Arc::new(InMemory::new());
    let (tx, rx) = mpsc::channel(4);
    let agent = BuiltinAgent::builder("stream", Arc::new(WaitingStream(Mutex::new(Some(rx)))))
        .memory(memory.clone())
        .hook(Rewrite)
        .compaction(None)
        .build();
    let mut stream = agent.run_stream("s", "question", &[]).await.unwrap();
    tx.send(Ok(ChatChunk::delta("original"))).await.unwrap();
    tx.send(Ok(ChatChunk::done(Some("stop".into()))))
        .await
        .unwrap();
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                assert!(!chunk.is_finished);
                assert_eq!(chunk.delta_text, "original");
            }
            Err(error) => {
                assert!(matches!(error, AgentError::InvalidRequest(ref message)
                    if message.contains("cannot rewrite")));
                failed = true;
            }
        }
    }
    assert!(failed);
    assert!(
        !memory
            .get_messages("s")
            .iter()
            .any(|message| message.content.as_deref() == Some("rewritten after delivery"))
    );
}

struct StaticResponse(ChatResponse);
#[async_trait]
impl LlmProvider for StaticResponse {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(self.0.clone())
    }
}

fn lookup_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "lookup".into(),
        arguments: serde_json::json!({}),
    }
}

fn lookup_definition() -> ToolDefinition {
    ToolDefinition {
        name: "lookup".into(),
        description: "test tool".into(),
        parameters: serde_json::json!({"type":"object"}),
    }
}

#[tokio::test]
async fn iteration_limit_streams_the_same_fallback_that_history_records() {
    let memory = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder(
        "limit",
        Arc::new(StaticResponse(ChatResponse {
            tool_calls: vec![lookup_call("call-1")],
            ..Default::default()
        })),
    )
    .memory(memory.clone())
    .max_iterations(0)
    .compaction(None)
    .tool(NativeTool::new(lookup_definition(), |_, _| async {
        panic!("limit forbids execution")
    }))
    .build();
    let mut stream = agent.run_stream("s", "question", &[]).await.unwrap();
    let mut answer = String::new();
    let mut finish_reason = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        answer.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finish_reason = chunk.finish_reason;
        }
    }
    assert!(!answer.is_empty());
    assert_eq!(finish_reason.as_deref(), Some("max_iterations"));
    assert_eq!(
        memory.get_messages("s")[1].content.as_deref(),
        Some(answer.as_str())
    );
}

#[tokio::test]
async fn streamed_textual_tool_markup_is_rejected_without_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let agent = BuiltinAgent::builder(
        "markup",
        Arc::new(StaticResponse(ChatResponse {
            content: Some("<tool_call>{\"name\":\"lookup\",\"arguments\":{}}</tool_call>".into()),
            ..Default::default()
        })),
    )
    .compaction(None)
    .tool(NativeTool::new(lookup_definition(), move |_, _| {
        called.fetch_add(1, Ordering::SeqCst);
        async { Ok("must not happen".into()) }
    }))
    .build();
    let mut stream = agent.run_stream("s", "question", &[]).await.unwrap();
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => assert!(!chunk.is_finished),
            Err(error) => {
                assert!(error.to_string().contains("structured tool calls"));
                failed = true;
            }
        }
    }
    assert!(failed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopping_a_streamed_tool_round_answers_every_open_call() {
    let memory = Arc::new(InMemory::new());
    let started = Arc::new(Notify::new());
    let tool_started = started.clone();
    let agent = BuiltinAgent::builder(
        "cancel-tool",
        Arc::new(StaticResponse(ChatResponse {
            tool_calls: vec![lookup_call("call-1"), lookup_call("call-2")],
            ..Default::default()
        })),
    )
    .memory(memory.clone())
    .compaction(None)
    .tool(NativeTool::new(lookup_definition(), move |_, _| {
        tool_started.notify_one();
        async { std::future::pending::<Result<String, String>>().await }
    }))
    .build();
    let stop = StopSignal::new();
    let mut stream =
        kanon_llm::with_stop_signal(stop.clone(), agent.run_stream("s", "question", &[]))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    stop.stop();
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(AgentError::Stopped)
    ));
    assert!(stream.next().await.is_none());
    let messages = memory.get_messages("s");
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(messages[3].tool_call_id.as_deref(), Some("call-2"));
    assert_eq!(
        messages[2].content.as_deref(),
        Some(kanon_llm::STOPPED_TOOL_RESULT)
    );
    assert_eq!(
        messages[3].content.as_deref(),
        Some(kanon_llm::STOPPED_TOOL_RESULT)
    );
    assert!(
        messages[4]
            .content
            .as_deref()
            .unwrap()
            .contains("This turn")
    );
}

#[tokio::test]
async fn a_streamed_tool_failure_keeps_its_type_and_paired_result() {
    let memory = Arc::new(InMemory::new());
    let agent = BuiltinAgent::builder(
        "failed-tool",
        Arc::new(StaticResponse(ChatResponse {
            tool_calls: vec![lookup_call("call-1")],
            ..Default::default()
        })),
    )
    .memory(memory.clone())
    .stop_on_tool_failure(true)
    .compaction(None)
    .tool(NativeTool::new(lookup_definition(), |_, _| async {
        Err("lookup unavailable".into())
    }))
    .build();
    let mut stream = agent.run_standalone_stream("s", "question").await.unwrap();

    assert!(
        matches!(stream.next().await.unwrap(), Err(AgentError::ToolFailed(message))
        if message.contains("lookup unavailable"))
    );
    assert!(stream.next().await.is_none());
    let messages = memory.get_messages("s");
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(
        messages[2].content.as_deref(),
        Some("Error: lookup unavailable")
    );
    assert!(
        messages[3]
            .content
            .as_deref()
            .unwrap()
            .contains("a tool execution failed")
    );
}

#[tokio::test]
async fn a_streamed_storage_failure_keeps_its_type_and_releases_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    let memory = Arc::new(kanon_llm::SqliteMemory::open(&db).unwrap());
    let sessions = Arc::new(SessionManager::new(memory.clone()));
    let (tx, rx) = mpsc::channel(4);
    let agent = BuiltinAgent::builder("stream", Arc::new(WaitingStream(Mutex::new(Some(rx)))))
        .session_manager(sessions.clone())
        .compaction(None)
        .build();
    let mut stream = agent.run_stream("s", "question", &[]).await.unwrap();
    tx.send(Ok(ChatChunk::delta("partial"))).await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap().delta_text, "partial");

    // Fail the actual assistant commit after streaming has started, while keeping reads usable.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER deny_assistant BEFORE INSERT ON messages WHEN NEW.role = 'assistant'
         BEGIN SELECT RAISE(FAIL, 'assistant write blocked'); END;",
        )
        .unwrap();
    tx.send(Ok(ChatChunk::done(Some("stop".into()))))
        .await
        .unwrap();

    assert!(
        matches!(stream.next().await.unwrap(), Err(AgentError::Memory(message))
        if message.contains("assistant write blocked"))
    );
    assert!(stream.next().await.is_none());
    assert!(sessions.try_write("s").is_ok());
    assert_eq!(
        memory.get_messages("s").await.unwrap(),
        [ChatMessage::user("question")]
    );
}
