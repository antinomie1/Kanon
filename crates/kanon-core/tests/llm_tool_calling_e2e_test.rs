//! End-to-end integration test verifying the LLM Gateway and cross-process Tool Calling loop.
//!
//! Workflow under test:
//! 1. Starts an out-of-process Rust plugin host (`demo-rust-plugin`) via `Supervisor`.
//! 2. Spawns a lightweight Mock HTTP server mimicking an OpenAI-compatible endpoint (`/v1/chat/completions`).
//! 3. Configures `kanon-llm` with `OpenAiProvider`, `InMemory`, and `ToolRouter`.
//! 4. Attaches the `ToolRouter` to `PipelineEngine` in `kanon-core`.
//! 5. Dispatches an inbound conversational message requiring mathematical reasoning.
//! 6. Traces the full state machine loop:
//!    - User message passes through PreFilter and CommandRouter (no slash command match);
//!    - PipelineEngine invokes `ToolRouter`;
//!    - Turn 1: Mock LLM receives user query + dynamic tool schema, requests tool `fast_calc`;
//!    - ToolRouter routes call to `demo-rust-plugin` via UDS IPC (`ManagedHost.on_call_tool`);
//!    - `demo-rust-plugin` executes computation and returns Protobuf Struct `{ "result": 42 }`;
//!    - Turn 2: Tool output is fed back to the Mock LLM;
//!    - Mock LLM synthesizes final answer based on tool output;
//!    - Outbound queue receives delivery request with final answer.

mod common;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_llm::gateway::providers::OpenAiProvider;
use kanon_llm::memory::InMemory;
use kanon_llm::tool_router::ToolRouter;
use kanon_proto::v1::bot_api_service_client::BotApiServiceClient;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, IngestEventRequest, MessageSegment, PipelineEventRequest, TextSegment,
};
use kanon_transport::connect_ipc;

/// Locates the compiled `demo-rust-plugin` binary in the target directory.
fn find_demo_plugin_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root");

    let debug_bin = root_dir.join("target/debug/demo-rust-plugin");
    if debug_bin.exists() {
        return debug_bin;
    }

    panic!(
        "Demo plugin executable not found at '{}'. Ensure 'cargo build -p demo-rust-plugin' has run.",
        debug_bin.display()
    );
}

/// Shared state for the Mock LLM HTTP server.
#[derive(Clone)]
struct MockServerState {
    request_counter: Arc<AtomicUsize>,
}

/// Handler for `/v1/chat/completions` simulating multi-turn reasoning and tool invocation.
async fn mock_chat_completions(
    State(state): State<MockServerState>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let turn = state.request_counter.fetch_add(1, Ordering::SeqCst);

    if turn == 0 {
        // Turn 0: Model inspects user prompt and requests `fast_calc` tool execution
        let tools = payload.get("tools").and_then(|t| t.as_array());
        assert!(
            tools.is_some(),
            "Turn 0: Request to model must declare available tools"
        );
        let has_fast_calc = tools
            .unwrap()
            .iter()
            .any(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some("fast_calc"));
        assert!(
            has_fast_calc,
            "Turn 0: 'fast_calc' tool definition must be dynamically present"
        );

        let response = serde_json::json!({
            "id": "chatcmpl-mock-1",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_mock_calc_001",
                                "type": "function",
                                "function": {
                                    "name": "fast_calc",
                                    "arguments": "{\"expr\": \"10 + 32\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ],
            "usage": {
                "prompt_tokens": 40,
                "completion_tokens": 15,
                "total_tokens": 55
            }
        });
        Json(response)
    } else {
        // Turn 1: Model receives tool result and produces the final natural language answer
        let messages = payload
            .get("messages")
            .and_then(|m| m.as_array())
            .expect("Messages array");

        // Verify that the conversation history includes the tool response message
        let has_tool_response = messages.iter().any(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("tool")
                && m.get("tool_call_id").and_then(|id| id.as_str()) == Some("call_mock_calc_001")
        });
        assert!(
            has_tool_response,
            "Turn 1: Request must include the tool execution output message"
        );

        let response = serde_json::json!({
            "id": "chatcmpl-mock-2",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "The computed result from the Rust plugin tool for [10 + 32] is 42."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 60,
                "completion_tokens": 20,
                "total_tokens": 80
            }
        });
        Json(response)
    }
}

/// Spawns the lightweight Mock LLM HTTP server on a local ephemeral port.
async fn start_mock_llm_server(state: MockServerState) -> (SocketAddr, oneshot::Sender<()>) {
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat_completions))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind ephemeral TCP port for Mock LLM");
    let addr = listener.local_addr().expect("Local addr");

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("Mock LLM server failed");
    });

    (addr, shutdown_tx)
}

#[tokio::test]
async fn test_llm_tool_calling_e2e_lifecycle() {
    // 1. Initialize isolated runtime directory for UDS sockets
    let temp_dir = tempdir().expect("Failed to create temporary directory");
    let run_dir = temp_dir.path().to_path_buf();
    let core_sock = run_dir.join("core.sock");

    // 2. Start Core IPC Server hosting BotApiService on core.sock
    let (event_tx, event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx);
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        core_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
            .expect("Core IPC Server failed");
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(core_sock.exists(), "Core socket file must exist");

    // 3. Initialize Supervisor and spawn out-of-process demo-rust-plugin
    let supervisor = Arc::new(Supervisor::new(
        Some(run_dir.clone()),
        Some(core_sock.clone()),
    ));
    let plugin_bin = find_demo_plugin_bin();
    let managed_host = supervisor
        .spawn_plugin("demo_rust", &plugin_bin, &[])
        .await
        .expect("Failed to spawn demo_rust plugin host");

    // Verify metadata discovery registered both the /rustcalc command and the fast_calc tool
    assert_eq!(managed_host.meta.len(), 1);
    assert_eq!(managed_host.meta[0].commands[0].name, "rustcalc");
    assert_eq!(managed_host.meta[0].tools[0].name, "fast_calc");

    // 4. Start Mock LLM HTTP server
    let mock_state = MockServerState {
        request_counter: Arc::new(AtomicUsize::new(0)),
    };
    let (mock_addr, mock_shutdown_tx) = start_mock_llm_server(mock_state.clone()).await;
    let mock_base_url = format!("http://127.0.0.1:{}/v1", mock_addr.port());

    // 5. Initialize kanon-llm gateway, memory, and ToolRouter
    let provider = Arc::new(OpenAiProvider::new(&mock_base_url, None, "mock-model"));
    let memory = Arc::new(InMemory::new());
    let tool_router = Arc::new(ToolRouter::new(provider, memory.clone(), "mock-model"));

    // 6. Register a built-in adapter for the fixture platform and start the pipeline worker with
    //    the ToolRouter plus the outbound dispatcher that delivers replies.
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<DeliverMessageRequest>(100);
    supervisor
        .adapters()
        .register(common::ChannelAdapter::shared("test_im", outbound_tx))
        .await
        .expect("adapter registration");

    let engine = Arc::new(PipelineEngine::new(supervisor.clone()).with_tool_router(tool_router));
    let worker_handle = engine.clone().start_worker(event_rx);
    let dispatcher_handle = engine
        .start_outbound_dispatcher()
        .expect("outbound dispatcher starts");

    // 7. Connect gRPC client to Core IPC socket
    let core_channel = connect_ipc(&core_sock)
        .await
        .expect("Failed to connect to core.sock");
    let mut core_client = BotApiServiceClient::new(core_channel);

    // =========================================================================
    // Scenario: Conversational query triggers Tool Calling loop -> Outbound reply
    // =========================================================================
    let user_event = IngestEventRequest {
        platform: "test_im".to_string(),
        event: Some(PipelineEventRequest {
            event_id: "evt_llm_math_1".to_string(),
            platform: "test_im".to_string(),
            channel_id: "chan_chat".to_string(),
            sender_id: "alice".to_string(),
            raw_text: "Please calculate 10 + 32 for me".to_string(),
            segments: vec![MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "Please calculate 10 + 32 for me".to_string(),
                })),
            }],
            metadata: None,
        }),
    };

    // Verify Fast-ACK is instantaneous
    let start_ack = std::time::Instant::now();
    let ack = core_client
        .ingest_event(user_event)
        .await
        .expect("IngestEvent RPC failed")
        .into_inner();
    let ack_elapsed = start_ack.elapsed();

    assert!(ack.accepted, "Fast-ACK must immediately accept event");
    assert_eq!(ack.event_id, "evt_llm_math_1");
    assert!(
        ack_elapsed < Duration::from_millis(50),
        "Fast-ACK must resolve in < 50ms (took {:?})",
        ack_elapsed
    );

    // Await delivery of outbound answer produced by the LLM tool loop
    let outbound_reply = tokio::time::timeout(Duration::from_secs(5), outbound_rx.recv())
        .await
        .expect("Timed out waiting for outbound reply from LLM tool calling pipeline")
        .expect("Outbound channel unexpectedly closed");

    assert_eq!(outbound_reply.platform, "test_im");
    assert_eq!(outbound_reply.channel_id, "chan_chat");
    assert_eq!(outbound_reply.recipient_id, "alice");
    assert_eq!(outbound_reply.segments.len(), 1);

    if let Some(Segment::Text(text)) = &outbound_reply.segments[0].segment {
        assert_eq!(
            text.content,
            "The computed result from the Rust plugin tool for [10 + 32] is 42."
        );
    } else {
        panic!("Expected text segment in outbound reply");
    }

    // Verify Mock LLM received exactly 2 requests (Turn 0: tool invocation, Turn 1: final answer)
    assert_eq!(mock_state.request_counter.load(Ordering::SeqCst), 2);

    // Verify conversation memory contains the entire multi-turn trajectory
    let session_key = InMemory::make_session_key("chan_chat", "alice");
    let history = memory.get_messages(&session_key);
    assert_eq!(
        history.len(),
        4,
        "Expected User -> Assistant (tools) -> Tool -> Assistant"
    );

    // =========================================================================
    // Clean Shutdown and Resource Teardown
    // =========================================================================
    worker_handle.abort();
    dispatcher_handle.abort();
    let _ = mock_shutdown_tx.send(());
    supervisor
        .stop_host("demo_rust")
        .await
        .expect("Failed to terminate host process");

    let _ = core_shutdown_tx.send(());
    let _ = server_task.await;

    assert!(
        !core_sock.exists(),
        "Core socket must be removed on shutdown"
    );
}
