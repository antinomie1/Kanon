//! Tests for rich media produced by tool calls reaching the outbound reply.
//!
//! A tool that draws a picture is only useful on a chat platform if the picture is delivered, so
//! this test follows one attachment across every boundary it must cross: an out-of-process tool
//! host (a real gRPC server over a UDS), the reasoning loop, and the pipeline that turns the result
//! into the message segments an adapter sends.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse, ToolCall};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::{ToolRouter, json_to_prost_struct};
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::v1::message_pipeline_service_server::MessagePipelineServiceServer;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DeliverMessageRequest, DeliverMessageResponse,
    EventAck, EventNotification, PipelineEventRequest, PluginMeta, PreFilterResult, ToolAttachment,
    ToolCallRequest, ToolCallResponse, ToolMeta, tool_call_response,
};
use kanon_transport::connect_ipc;
use tempfile::tempdir;

/// Provider that asks for the drawing tool once and then reports the tool's text.
struct DrawingProvider;

#[async_trait]
impl LlmProvider for DrawingProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let has_tool_result = request.messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("card rendered"))
        });

        if has_tool_result {
            return Ok(ChatResponse {
                content: Some("这是你的 B50 图。".to_string()),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_string()),
                usage: None,
            });
        }

        Ok(ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call-draw".to_string(),
                name: "draw_card".to_string(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some("tool_calls".to_string()),
            usage: None,
        })
    }
}

/// In-process plugin host exposing one tool that returns a file attachment.
struct DrawingHost {
    /// Path the attachment points at.
    image_path: String,
    /// Event attached to the last tool call, so the test can check who invoked the tool.
    caller: Arc<std::sync::Mutex<Option<PipelineEventRequest>>>,
}

#[tonic::async_trait]
impl kanon_proto::v1::message_pipeline_service_server::MessagePipelineService for DrawingHost {
    async fn on_pre_filter(
        &self,
        _request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        Ok(tonic::Response::new(PreFilterResult {
            action: kanon_proto::v1::pre_filter_result::Action::Pass as i32,
            modified_text: String::new(),
            reply_messages: Vec::new(),
        }))
    }

    async fn on_execute_command(
        &self,
        _request: tonic::Request<CommandExecuteRequest>,
    ) -> Result<tonic::Response<CommandExecuteResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("no commands"))
    }

    async fn on_call_tool(
        &self,
        request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        let req = request.into_inner();
        *self.caller.lock().unwrap() = req.context.clone();
        let result = json_to_prost_struct(&serde_json::json!({ "content": "card rendered" }))
            .expect("struct");

        Ok(tonic::Response::new(ToolCallResponse {
            call_id: req.call_id,
            success: true,
            error_message: String::new(),
            payload: Some(tool_call_response::Payload::StructuredResult(result)),
            attachments: vec![ToolAttachment {
                mime_type: "image/png".to_string(),
                file_path: Some(self.image_path.clone()),
                url: None,
            }],
        }))
    }

    async fn on_event(
        &self,
        _request: tonic::Request<EventNotification>,
    ) -> Result<tonic::Response<EventAck>, tonic::Status> {
        Ok(tonic::Response::new(EventAck { received: true }))
    }

    async fn on_decorate_reply(
        &self,
        _request: tonic::Request<kanon_proto::v1::DecorateReplyRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::DecorateReplyResult>, tonic::Status> {
        Ok(tonic::Response::new(
            kanon_proto::v1::DecorateReplyResult::default(),
        ))
    }

    async fn on_prepare_turn(
        &self,
        _request: tonic::Request<kanon_proto::v1::PrepareTurnRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::PrepareTurnResult>, tonic::Status> {
        Ok(tonic::Response::new(
            kanon_proto::v1::PrepareTurnResult::default(),
        ))
    }

    async fn on_deliver_message(
        &self,
        _request: tonic::Request<DeliverMessageRequest>,
    ) -> Result<tonic::Response<DeliverMessageResponse>, tonic::Status> {
        Ok(tonic::Response::new(DeliverMessageResponse {
            success: true,
            message_id: "fixture".to_string(),
            error_message: String::new(),
        }))
    }
}

/// Starts the drawing host on a temporary socket and registers it with the supervisor.
async fn register_drawing_host(
    supervisor: &Supervisor,
    socket_path: PathBuf,
    image_path: String,
) -> Arc<std::sync::Mutex<Option<PipelineEventRequest>>> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = kanon_transport::IpcListener::bind(&socket_path).expect("host socket binds");
    let incoming = listener.incoming();
    let caller = Arc::new(std::sync::Mutex::new(None));
    let service = DrawingHost {
        image_path,
        caller: caller.clone(),
    };

    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(MessagePipelineServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await;
    });

    // The socket file appears before the server is accepting connections; retry briefly so the
    // test does not race its own fixture.
    let mut channel = None;
    for _ in 0..50 {
        match connect_ipc(&socket_path).await {
            Ok(candidate) => {
                channel = Some(candidate);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
    let channel = channel.expect("fixture host reachable");

    supervisor
        .register_managed_host(Arc::new(ManagedHost::new(
            "host_draw".to_string(),
            socket_path,
            channel,
            vec![PluginMeta {
                id: "org.kanon.plugin.drawer".to_string(),
                name: "Drawer".to_string(),
                tools: vec![ToolMeta {
                    name: "draw_card".to_string(),
                    description: "Draws a card".to_string(),
                    parameters: None,
                }],
                ..PluginMeta::default()
            }],
            100,
        )))
        .await;
    caller
}

/// Inbound event fixture.
fn event(text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: "evt-attach".to_string(),
        platform: "qqofficial".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "user:1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: None,
    }
}

#[tokio::test]
async fn a_tool_attachment_is_delivered_as_an_image_segment() {
    let dir = tempdir().expect("temp dir");
    let image = dir.path().join("card.png");
    std::fs::write(&image, b"png-bytes").expect("fixture image");

    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let caller = register_drawing_host(
        &supervisor,
        dir.path().join("host_draw.sock"),
        image.to_string_lossy().to_string(),
    )
    .await;

    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder("attachment-test", Arc::new(DrawingProvider))
            .memory(memory)
            .model("test-model")
            .build(),
    );
    let engine =
        PipelineEngine::new(supervisor).with_tool_router(Arc::new(ToolRouter::from_arc(agent)));

    let result = engine.process_event(event("画一张 B50")).await;

    // The tool learns which platform event invoked it, without the model passing it along.
    let invoked_by = caller
        .lock()
        .unwrap()
        .clone()
        .expect("tool call carries its event");
    assert_eq!(invoked_by.sender_id, "user:1");
    assert_eq!(invoked_by.channel_id, "group:1");

    let replies = match result {
        PipelineResult::LlmReplied { content, replies } => {
            assert_eq!(content, "这是你的 B50 图。");
            replies
        }
        other => panic!("expected an LLM reply, got {other:?}"),
    };

    assert_eq!(
        replies.len(),
        2,
        "the text and the picture are separate segments: {replies:?}"
    );
    match &replies[0].segment {
        Some(Segment::Text(text)) => assert_eq!(text.content, "这是你的 B50 图。"),
        other => panic!("expected the text first, got {other:?}"),
    }
    match &replies[1].segment {
        Some(Segment::Image(image_segment)) => {
            assert_eq!(image_segment.mime_type.as_deref(), Some("image/png"));
            match image_segment.source.as_ref() {
                Some(kanon_proto::v1::image_segment::Source::FilePath(path)) => {
                    assert_eq!(path, &image.to_string_lossy().to_string());
                }
                other => panic!("expected a file path, got {other:?}"),
            }
        }
        other => panic!("expected an image segment, got {other:?}"),
    }
}
