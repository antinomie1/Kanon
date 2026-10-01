//! Conversation captures: a command can ask for the sender's next message.
//!
//! The pipeline handles one event at a time, so a plugin can never block waiting for an answer.
//! Instead its command response names a capture window and the core routes the sender's next
//! message back to the same plugin as a continuation. These tests drive that through a real gRPC
//! host over a UDS, the same boundary production plugins cross.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_proto::v1::message_pipeline_service_server::MessagePipelineServiceServer;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, CommandMeta, DeliverMessageRequest,
    DeliverMessageResponse, EventAck, EventNotification, MessageSegment, PipelineEventRequest,
    PluginMeta, PreFilterResult, TextSegment, ToolCallRequest, ToolCallResponse,
};
use kanon_transport::connect_ipc;
use tempfile::tempdir;

/// A number-guessing plugin: `/guess` asks a question and captures the answer once.
struct GuessHost {
    /// Every command request the plugin received, in order.
    seen: Arc<Mutex<Vec<CommandExecuteRequest>>>,
}

fn text(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }
}

#[tonic::async_trait]
impl kanon_proto::v1::message_pipeline_service_server::MessagePipelineService for GuessHost {
    async fn on_llm_request(
        &self,
        _request: tonic::Request<kanon_proto::v1::LlmRequestHookRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::LlmRequestHookResult>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_llm_request"))
    }

    async fn on_http_request(
        &self,
        _request: tonic::Request<kanon_proto::v1::HttpRequest>,
    ) -> Result<tonic::Response<kanon_proto::v1::HttpResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("on_http_request"))
    }
    async fn on_pre_filter(
        &self,
        _request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        Ok(tonic::Response::new(PreFilterResult::default()))
    }

    async fn on_execute_command(
        &self,
        request: tonic::Request<CommandExecuteRequest>,
    ) -> Result<tonic::Response<CommandExecuteResponse>, tonic::Status> {
        let req = request.into_inner();
        let continuation = req.continuation;
        self.seen.lock().unwrap().push(req);
        let response = if continuation {
            CommandExecuteResponse {
                success: true,
                replies: vec![text("猜中了")],
                ..Default::default()
            }
        } else {
            CommandExecuteResponse {
                success: true,
                replies: vec![text("猜一个数字")],
                capture_seconds: 60,
                ..Default::default()
            }
        };
        Ok(tonic::Response::new(response))
    }

    async fn on_call_tool(
        &self,
        _request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("no tools"))
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
        Err(tonic::Status::unimplemented("not an adapter"))
    }
}

/// Starts the guessing host on a temporary socket and registers it with the supervisor.
async fn register_guess_host(
    supervisor: &Supervisor,
    socket_path: PathBuf,
) -> Arc<Mutex<Vec<CommandExecuteRequest>>> {
    let listener = kanon_transport::IpcListener::bind(&socket_path).expect("host socket binds");
    let incoming = listener.incoming();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let service = GuessHost { seen: seen.clone() };
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(MessagePipelineServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await;
    });

    // The socket file appears before the server accepts connections; retry briefly.
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

    supervisor
        .register_managed_host(Arc::new(ManagedHost::new(
            "host_guess".to_string(),
            socket_path,
            channel.expect("fixture host reachable"),
            vec![PluginMeta {
                id: "org.kanon.plugin.guess".to_string(),
                name: "Guess".to_string(),
                commands: vec![CommandMeta {
                    name: "guess".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            100,
        )))
        .await;
    seen
}

fn event(id: &str, sender: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: sender.to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: None,
    }
}

/// The captured sender's next message — even one that looks like a command — reaches the plugin
/// as a continuation; other senders are unaffected, and the capture is used up after one message.
#[tokio::test]
async fn a_capture_routes_the_next_message_back_to_the_plugin_once() {
    let dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let seen = register_guess_host(&supervisor, dir.path().join("host_guess.sock")).await;
    let engine = PipelineEngine::new(supervisor);

    let asked = engine.process_event(event("e1", "u1", "/guess")).await;
    assert!(
        matches!(&asked, PipelineResult::CommandExecuted { command, .. } if command == "guess"),
        "{asked:?}"
    );

    // Another sender in the same channel is not the one being asked.
    let other = engine.process_event(event("e2", "u2", "/nope")).await;
    assert!(
        matches!(other, PipelineResult::CommandNotFound { .. }),
        "{other:?}"
    );

    let answered = engine.process_event(event("e3", "u1", "/42")).await;
    let PipelineResult::CommandExecuted {
        command, replies, ..
    } = answered
    else {
        panic!("the answer goes to the plugin, got {answered:?}");
    };
    assert_eq!(command, "guess");
    assert!(matches!(&replies[0].segment, Some(Segment::Text(t)) if t.content == "猜中了"));

    // The continuation did not ask again, so the sender is free.
    let after = engine.process_event(event("e4", "u1", "/nope")).await;
    assert!(
        matches!(after, PipelineResult::CommandNotFound { .. }),
        "{after:?}"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(!seen[0].continuation);
    assert!(seen[1].continuation);
    assert_eq!(seen[1].command, "guess");
    assert_eq!(seen[1].raw_args, "/42");
    assert_eq!(
        seen[1].context.as_ref().map(|c| c.event_id.as_str()),
        Some("e3")
    );
}
