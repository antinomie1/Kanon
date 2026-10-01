//! Lifecycle events and reply decoration through a real gRPC plugin host.
//!
//! The contract: a plugin is told only about the event kinds it subscribed to; a decorating
//! plugin rewrites both model and command replies before delivery; and a failing decorator leaves
//! the reply exactly as it was.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_core::{AdapterError, Capability, META_NOTICE, PlatformAdapter};
use kanon_llm::gateway::types::{ChatRequest, ChatResponse};
use kanon_llm::memory::{InMemory, Memory};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, GatewayError, LlmProvider};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::event_notification::Detail;
use kanon_proto::v1::message_pipeline_service_server::MessagePipelineServiceServer;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, CommandMeta, DecorateReplyRequest,
    DecorateReplyResult, DeliverMessageRequest, DeliverMessageResponse, EventAck, EventKind,
    EventNotification, MessageSegment, PipelineEventRequest, PluginMeta, PreFilterResult,
    ReplySource, TextSegment, ToolCallRequest, ToolCallResponse,
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

/// Plugin recording its events and signing every reply with `~`, except replies to `boom`,
/// where it fails.
struct HookHost {
    events: Arc<Mutex<Vec<EventNotification>>>,
}

#[tonic::async_trait]
impl kanon_proto::v1::message_pipeline_service_server::MessagePipelineService for HookHost {
    async fn on_pre_filter(
        &self,
        _request: tonic::Request<PipelineEventRequest>,
    ) -> Result<tonic::Response<PreFilterResult>, tonic::Status> {
        Ok(tonic::Response::new(PreFilterResult::default()))
    }

    async fn on_execute_command(
        &self,
        _request: tonic::Request<CommandExecuteRequest>,
    ) -> Result<tonic::Response<CommandExecuteResponse>, tonic::Status> {
        Ok(tonic::Response::new(CommandExecuteResponse {
            success: true,
            replies: vec![text("hello")],
            ..Default::default()
        }))
    }

    async fn on_call_tool(
        &self,
        _request: tonic::Request<ToolCallRequest>,
    ) -> Result<tonic::Response<ToolCallResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("no tools"))
    }

    async fn on_event(
        &self,
        request: tonic::Request<EventNotification>,
    ) -> Result<tonic::Response<EventAck>, tonic::Status> {
        self.events.lock().unwrap().push(request.into_inner());
        Ok(tonic::Response::new(EventAck { received: true }))
    }

    async fn on_decorate_reply(
        &self,
        request: tonic::Request<DecorateReplyRequest>,
    ) -> Result<tonic::Response<DecorateReplyResult>, tonic::Status> {
        let req = request.into_inner();
        if req
            .context
            .as_ref()
            .is_some_and(|context| context.raw_text.contains("boom"))
        {
            return Err(tonic::Status::internal("decorator crashed"));
        }
        let mark = match req.source() {
            ReplySource::Llm => "~llm",
            ReplySource::Command => "~cmd",
            ReplySource::Unspecified => "~?",
        };
        let mut segments = req.segments;
        segments.push(text(mark));
        Ok(tonic::Response::new(DecorateReplyResult {
            modified: true,
            segments,
        }))
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

/// Built-in adapter that accepts every delivery.
struct OkAdapter;

#[async_trait]
impl PlatformAdapter for OkAdapter {
    fn platform(&self) -> &str {
        "qq"
    }

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        Ok(DeliverMessageResponse {
            success: true,
            message_id: "m-1".to_string(),
            error_message: String::new(),
        })
    }
}

struct Echo;

#[async_trait]
impl LlmProvider for Echo {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            content: Some("好的".to_string()),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

/// Starts the hook host and registers it, subscribed to `events` and decorating replies.
async fn register_hook_host(
    supervisor: &Supervisor,
    socket_path: PathBuf,
    events: Vec<EventKind>,
) -> Arc<Mutex<Vec<EventNotification>>> {
    let listener = kanon_transport::IpcListener::bind(&socket_path).expect("host socket binds");
    let incoming = listener.incoming();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let service = HookHost {
        events: seen.clone(),
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
            "host_hooks".to_string(),
            socket_path,
            channel.expect("fixture host reachable"),
            vec![PluginMeta {
                id: "org.kanon.plugin.hooks".to_string(),
                commands: vec![CommandMeta {
                    name: "hi".to_string(),
                    ..Default::default()
                }],
                events: events.into_iter().map(|kind| kind as i32).collect(),
                decorates_replies: true,
                ..Default::default()
            }],
            100,
        )))
        .await;
    seen
}

fn event(id: &str, text: &str, notice: Option<&str>) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "private:1".to_string(),
        sender_id: "u1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: notice.map(|kind| prost_types::Struct {
            fields: [(
                META_NOTICE.to_string(),
                prost_types::Value {
                    kind: Some(Kind::StringValue(kind.to_string())),
                },
            )]
            .into(),
        }),
    }
}

async fn engine_with_host(
    events: Vec<EventKind>,
) -> (
    PipelineEngine,
    Arc<Mutex<Vec<EventNotification>>>,
    tempfile::TempDir,
) {
    let dir = tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    supervisor
        .adapters()
        .register(Arc::new(OkAdapter))
        .await
        .expect("register adapter");
    let seen = register_hook_host(&supervisor, dir.path().join("host_hooks.sock"), events).await;
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let agent = Arc::new(
        Agent::builder("hooks-test", Arc::new(Echo))
            .memory(memory)
            .model("test-model")
            .build(),
    );
    let engine =
        PipelineEngine::new(supervisor).with_tool_router(Arc::new(ToolRouter::from_arc(agent)));
    (engine, seen, dir)
}

/// Waits until `count` events arrived; events are fire-and-forget, so they trail the result.
async fn wait_for(seen: &Mutex<Vec<EventNotification>>, count: usize) -> Vec<EventNotification> {
    for _ in 0..100 {
        if seen.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn decorators_rewrite_model_and_command_replies_and_failures_keep_the_reply() {
    let (engine, _, _dir) = engine_with_host(Vec::new()).await;

    let PipelineResult::CommandExecuted { replies, .. } =
        engine.process_event(event("e1", "/hi", None)).await
    else {
        panic!("the command runs");
    };
    assert_eq!(texts(&replies), ["hello", "~cmd"]);

    let PipelineResult::LlmReplied {
        content, replies, ..
    } = engine.process_event(event("e2", "你好", None)).await
    else {
        panic!("the model answers");
    };
    assert_eq!(
        content, "好的",
        "decoration never changes the model's words"
    );
    assert_eq!(texts(&replies), ["好的", "~llm"]);

    let PipelineResult::CommandExecuted { replies, .. } =
        engine.process_event(event("e3", "/hi boom", None)).await
    else {
        panic!("the command runs");
    };
    assert_eq!(
        texts(&replies),
        ["hello"],
        "a failed decorator keeps the reply"
    );
}

#[tokio::test]
async fn plugins_receive_only_the_events_they_subscribed_to() {
    let (engine, seen, _dir) =
        engine_with_host(vec![EventKind::Notice, EventKind::LlmResponse]).await;

    engine
        .process_event(event("n1", "[poke]", Some("poke")))
        .await;
    engine.process_event(event("e1", "你好", None)).await;
    engine
        .dispatch_outbound_request(DeliverMessageRequest {
            platform: "qq".to_string(),
            channel_id: "private:1".to_string(),
            segments: vec![text("hi")],
            ..Default::default()
        })
        .await;

    wait_for(&seen, 2).await;
    // Give an unwanted `MessageSent` the chance to arrive before asserting it did not.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = seen.lock().unwrap().clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events
            .iter()
            .all(|event| event.plugin_id == "org.kanon.plugin.hooks")
    );
    assert!(events.iter().any(
        |event| matches!(&event.detail, Some(Detail::Notice(notice)) if notice.event_id == "n1")
    ));
    assert!(events.iter().any(|event| matches!(
        &event.detail,
        Some(Detail::LlmResponse(response))
            if response.content == "好的"
                && response.context.as_ref().is_some_and(|c| c.event_id == "e1")
    )));
}

#[tokio::test]
async fn a_delivered_message_is_reported_to_subscribers() {
    let (engine, seen, _dir) = engine_with_host(vec![EventKind::MessageSent]).await;

    engine
        .dispatch_outbound_request(DeliverMessageRequest {
            platform: "qq".to_string(),
            channel_id: "private:1".to_string(),
            segments: vec![text("hi")],
            ..Default::default()
        })
        .await;

    let events = wait_for(&seen, 1).await;
    let Some(Detail::MessageSent(sent)) = &events[0].detail else {
        panic!("expected a message_sent event, got {events:?}");
    };
    assert_eq!(sent.message_id, "m-1");
    assert_eq!(texts(&sent.message.as_ref().unwrap().segments), ["hi"]);
}
