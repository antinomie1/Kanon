//! Turn preparation, handing a command's message on to the model, and reading conversation
//! history, through a real gRPC plugin host.
//!
//! The contract: a preparer's text reaches the model inside the current user message (and so
//! the history), never the system prompt; a handler that passes its message on gets its replies
//! delivered and the model reads the (possibly rewritten) message; a handler that also captures
//! the conversation keeps the capture instead; and `GetConversationHistory` returns the user and
//! assistant turns of exactly the session the model answered in.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::ipc::CoreApiService;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_core::{AdapterError, Capability, PlatformAdapter};
use kanon_llm::BuiltinAgent;
use kanon_llm::gateway::types::{ChatRequest, ChatResponse, Role};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{GatewayError, LlmProvider};
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::message_pipeline_service_server::MessagePipelineServiceServer;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, CommandMeta, ConversationHistoryRequest,
    DecorateReplyRequest, DecorateReplyResult, DeliverMessageRequest, DeliverMessageResponse,
    EventAck, EventNotification, LlmRole, MessageSegment, PipelineEventRequest, PluginMeta,
    PreFilterResult, PrepareTurnRequest, PrepareTurnResult, TextSegment, ToolCallRequest,
    ToolCallResponse,
};
use kanon_transport::connect_ipc;
use tempfile::tempdir;

fn text(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.to_string(),
        })),
    }
}

fn texts(segments: &[MessageSegment]) -> Vec<String> {
    segments
        .iter()
        .filter_map(|segment| match &segment.segment {
            Some(Segment::Text(t)) => Some(t.content.clone()),
            _ => None,
        })
        .collect()
}

/// Plugin with a preparer and two commands: `/note` passes its message on with new text and a
/// reply of its own, `/ask` passes it on but also captures the conversation.
struct TurnHost {
    sessions: Arc<Mutex<Vec<String>>>,
}

#[tonic::async_trait]
impl kanon_proto::v1::message_pipeline_service_server::MessagePipelineService for TurnHost {
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
        let response = match req.command.as_str() {
            "note" => CommandExecuteResponse {
                success: true,
                replies: vec![text("noted")],
                pass_to_model: true,
                model_text: Some(format!("remember: {}", req.raw_args)),
                ..Default::default()
            },
            _ => CommandExecuteResponse {
                success: true,
                replies: vec![text("which one?")],
                capture_seconds: 30,
                pass_to_model: true,
                ..Default::default()
            },
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
        _request: tonic::Request<DecorateReplyRequest>,
    ) -> Result<tonic::Response<DecorateReplyResult>, tonic::Status> {
        Ok(tonic::Response::new(DecorateReplyResult::default()))
    }

    async fn on_prepare_turn(
        &self,
        request: tonic::Request<PrepareTurnRequest>,
    ) -> Result<tonic::Response<PrepareTurnResult>, tonic::Status> {
        let req = request.into_inner();
        self.sessions.lock().unwrap().push(req.session_id);
        let asked = req.context.map(|c| c.raw_text).unwrap_or_default();
        Ok(tonic::Response::new(PrepareTurnResult {
            text: format!("[memory for '{asked}'] likes tea"),
        }))
    }

    async fn on_deliver_message(
        &self,
        _request: tonic::Request<DeliverMessageRequest>,
    ) -> Result<tonic::Response<DeliverMessageResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("not an adapter"))
    }
}

/// Built-in adapter recording every delivery.
struct RecordingAdapter {
    delivered: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl PlatformAdapter for RecordingAdapter {
    fn platform(&self) -> &str {
        "qq"
    }

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        self.delivered
            .lock()
            .unwrap()
            .push(texts(&request.segments));
        Ok(DeliverMessageResponse {
            success: true,
            message_id: "m-1".to_string(),
            error_message: String::new(),
        })
    }
}

/// Model recording the last user message it was asked to answer, answering with reasoning.
struct Recorder {
    last_user: Arc<Mutex<String>>,
}

#[async_trait]
impl LlmProvider for Recorder {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .and_then(|message| message.content.clone())
            .unwrap_or_default();
        *self.last_user.lock().unwrap() = user;
        Ok(ChatResponse {
            content: Some("<think>hmm</think>好的".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

struct Fixture {
    engine: Arc<PipelineEngine>,
    last_user: Arc<Mutex<String>>,
    delivered: Arc<Mutex<Vec<Vec<String>>>>,
    sessions: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

async fn register_turn_host(
    supervisor: &Supervisor,
    socket_path: PathBuf,
) -> Arc<Mutex<Vec<String>>> {
    let listener = kanon_transport::IpcListener::bind(&socket_path).expect("host socket binds");
    let incoming = listener.incoming();
    let sessions = Arc::new(Mutex::new(Vec::new()));
    let service = TurnHost {
        sessions: sessions.clone(),
    };
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(MessagePipelineServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await;
    });

    let mut channel = None;
    for _ in 0..50 {
        match connect_ipc(&socket_path).await {
            Ok(candidate) => {
                channel = Some(candidate);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }

    supervisor
        .register_managed_host(Arc::new(ManagedHost::new(
            "host_turns".to_string(),
            socket_path,
            channel.expect("fixture host reachable"),
            vec![PluginMeta {
                id: "org.kanon.plugin.turns".to_string(),
                commands: vec![
                    CommandMeta {
                        name: "note".to_string(),
                        ..Default::default()
                    },
                    CommandMeta {
                        name: "ask".to_string(),
                        ..Default::default()
                    },
                ],
                prepares_turns: true,
                ..Default::default()
            }],
            100,
        )))
        .await;
    sessions
}

async fn fixture() -> Fixture {
    let dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    let delivered = Arc::new(Mutex::new(Vec::new()));
    supervisor
        .adapters()
        .register(Arc::new(RecordingAdapter {
            delivered: delivered.clone(),
        }))
        .await
        .expect("register adapter");
    let sessions = register_turn_host(&supervisor, dir.path().join("host_turns.sock")).await;
    let last_user = Arc::new(Mutex::new(String::new()));
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        BuiltinAgent::builder(
            "turns-test",
            Arc::new(Recorder {
                last_user: last_user.clone(),
            }),
        )
        .memory(memory)
        .model("test-model")
        .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(supervisor).with_tool_router(Arc::new(ToolRouter::from_arc(agent))),
    );
    engine.clone().start_outbound_dispatcher();
    Fixture {
        engine,
        last_user,
        delivered,
        sessions,
        _dir: dir,
    }
}

fn event(id: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "private:1".to_string(),
        sender_id: "u1".to_string(),
        raw_text: text.to_string(),
        ..Default::default()
    }
}

/// Waits until `count` deliveries were recorded; delivery runs on the dispatcher task.
async fn deliveries(delivered: &Mutex<Vec<Vec<String>>>, count: usize) -> Vec<Vec<String>> {
    for _ in 0..100 {
        if delivered.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    delivered.lock().unwrap().clone()
}

#[tokio::test]
async fn a_preparer_adds_context_to_the_current_user_message() {
    let fx = fixture().await;
    let PipelineResult::LlmReplied { content, .. } = fx
        .engine
        .process_event(event("e1", "what do I like?"))
        .await
    else {
        panic!("the model answers");
    };
    assert_eq!(content, "好的");

    let user = fx.last_user.lock().unwrap().clone();
    let context = user
        .find("[memory for 'what do I like?'] likes tea")
        .expect("the preparer's text reaches the model");
    // The preparer's text quotes the question too, so the message itself is the last mention.
    let message = user.rfind("what do I like?");
    assert!(
        message.is_some_and(|message| context < message),
        "the context comes before the message: {user:?}"
    );
    assert_eq!(
        *fx.sessions.lock().unwrap(),
        ["private:1:u1"],
        "the preparer is told the session the model answers in"
    );
}

#[tokio::test]
async fn a_command_can_hand_its_message_on_to_the_model() {
    let fx = fixture().await;
    let PipelineResult::LlmReplied { .. } = fx
        .engine
        .process_event(event("e1", "/note tea at five"))
        .await
    else {
        panic!("the message continues to the model");
    };
    let user = fx.last_user.lock().unwrap().clone();
    assert!(user.contains("remember: tea at five"), "{user:?}");
    assert!(
        !user.contains("/note"),
        "the model reads the rewritten text: {user:?}"
    );

    let delivered = deliveries(&fx.delivered, 1).await;
    assert_eq!(
        delivered[0],
        ["noted"],
        "the handler's own reply is delivered before the model's"
    );
}

#[tokio::test]
async fn a_capture_wins_over_passing_the_message_on() {
    let fx = fixture().await;
    let PipelineResult::CommandExecuted { replies, .. } =
        fx.engine.process_event(event("e1", "/ask")).await
    else {
        panic!("a capturing handler keeps the message");
    };
    assert_eq!(texts(&replies), ["which one?"]);
    assert!(
        fx.last_user.lock().unwrap().is_empty(),
        "the model was not asked"
    );
}

#[tokio::test]
async fn history_returns_the_turns_of_the_answered_session() {
    let fx = fixture().await;
    fx.engine.process_event(event("e1", "hello")).await;

    let service =
        CoreApiService::new(tokio::sync::mpsc::channel(1).0).with_engine(fx.engine.clone());
    let history = service
        .get_conversation_history(tonic::Request::new(ConversationHistoryRequest {
            context: Some(event("e2", "anything")),
            limit: 0,
        }))
        .await
        .expect("history is readable")
        .into_inner();
    assert_eq!(history.session_id, "private:1:u1");
    let roles: Vec<i32> = history.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [LlmRole::User as i32, LlmRole::Assistant as i32]);
    assert!(history.messages[0].text.contains("hello"));
    assert_eq!(
        history.messages[1].text, "好的",
        "the model's reasoning is not part of the history plugins read"
    );

    let last = service
        .get_conversation_history(tonic::Request::new(ConversationHistoryRequest {
            context: Some(event("e3", "anything")),
            limit: 1,
        }))
        .await
        .expect("history is readable")
        .into_inner();
    assert_eq!(last.messages.len(), 1);
    assert_eq!(last.messages[0].role, LlmRole::Assistant as i32);

    let missing = service
        .get_conversation_history(tonic::Request::new(ConversationHistoryRequest::default()))
        .await
        .expect_err("a request needs the message");
    assert_eq!(missing.code(), tonic::Code::InvalidArgument);
}
