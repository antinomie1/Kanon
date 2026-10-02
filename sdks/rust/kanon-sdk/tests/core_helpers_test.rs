//! `CoreHandle` helpers against a fake core: the requests they build, what they make of the
//! answers, how gRPC failures map to `CoreError`, and which mistakes are rejected before
//! anything is sent. Also covers tools added and removed at runtime, which must tell the core.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanon_proto::v1::bot_api_service_server::{BotApiService, BotApiServiceServer};
use kanon_sdk::prelude::*;
use kanon_transport::{IpcListener, connect_ipc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// A request the fake core received.
#[derive(Debug, Clone)]
enum Seen {
    SetStorage(SetStorageRequest),
    GetStorage(GetStorageRequest),
    DeleteStorage(DeleteStorageRequest),
    ListStorage(ListStorageRequest),
    Conversations(&'static str, ConversationsRequest),
    Select(&'static str, SelectConversationRequest),
    Append(AppendConversationRequest),
    ListPersonas,
    UpsertPersona,
    DeletePersona,
    RunAgent(RunAgentRequest),
    Refresh(RefreshPluginMetaRequest),
    Render(RenderImageRequest),
    Llm,
}

/// A core that records requests and answers from a little in-memory state.
#[derive(Default)]
struct FakeCore {
    seen: Mutex<Vec<Seen>>,
    storage: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    personas: Mutex<Vec<Persona>>,
    /// When set, `RefreshPluginMeta` fails with this code.
    refresh_fails: Mutex<Option<tonic::Code>>,
}

impl FakeCore {
    fn record(&self, seen: Seen) {
        self.seen.lock().unwrap().push(seen);
    }

    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn conversations() -> ConversationList {
    ConversationList {
        conversations: vec![ConversationInfo {
            session_id: "s1".into(),
            current: true,
            title: "hello".into(),
            message_count: 2,
            last_active_at: 1,
        }],
    }
}

/// Serves a shared [`FakeCore`] (tonic needs the service by value).
struct Served(Arc<FakeCore>);

#[tonic::async_trait]
impl BotApiService for Served {
    type RequestLLMStream = ReceiverStream<Result<LlmChunk, Status>>;

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Err(Status::unimplemented("no ping here"))
    }

    async fn register_host(
        &self,
        _request: Request<RegisterHostRequest>,
    ) -> Result<Response<RegisterHostResponse>, Status> {
        Err(Status::unimplemented("not part of this fixture"))
    }

    async fn ingest_event(
        &self,
        _request: Request<IngestEventRequest>,
    ) -> Result<Response<IngestEventResponse>, Status> {
        Err(Status::resource_exhausted("ingest queue full"))
    }

    async fn reply_message(
        &self,
        _request: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        Err(Status::deadline_exceeded("adapter did not answer"))
    }

    async fn send_message(
        &self,
        _request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        Err(Status::not_found("unknown platform"))
    }

    async fn call_platform_api(
        &self,
        _request: Request<PlatformApiRequest>,
    ) -> Result<Response<PlatformApiResponse>, Status> {
        Err(Status::permission_denied("not allowed"))
    }

    async fn request_llm(
        &self,
        request: Request<LlmRequest>,
    ) -> Result<Response<Self::RequestLLMStream>, Status> {
        let request = request.into_inner();
        let images: usize = request
            .messages
            .iter()
            .map(|message| message.images.len())
            .sum();
        self.0.record(Seen::Llm);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        for delta in [format!("saw {images} "), "image(s)".to_string()] {
            let chunk = LlmChunk {
                delta_text: delta,
                is_finished: false,
            };
            tx.send(Ok(chunk)).await.unwrap();
        }
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn set_storage(
        &self,
        request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::SetStorage(request.clone()));
        let success = request.key != "refused";
        if success {
            self.0
                .storage
                .lock()
                .unwrap()
                .insert((request.plugin_id, request.key), request.value);
        }
        Ok(Response::new(SetStorageResponse { success }))
    }

    async fn get_storage(
        &self,
        request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::GetStorage(request.clone()));
        let value = self
            .0
            .storage
            .lock()
            .unwrap()
            .get(&(request.plugin_id, request.key))
            .cloned();
        Ok(Response::new(GetStorageResponse {
            found: value.is_some(),
            value: value.unwrap_or_default(),
        }))
    }

    async fn delete_storage(
        &self,
        request: Request<DeleteStorageRequest>,
    ) -> Result<Response<DeleteStorageResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::DeleteStorage(request.clone()));
        let deleted = self
            .0
            .storage
            .lock()
            .unwrap()
            .remove(&(request.plugin_id, request.key))
            .is_some();
        Ok(Response::new(DeleteStorageResponse { deleted }))
    }

    async fn list_storage(
        &self,
        request: Request<ListStorageRequest>,
    ) -> Result<Response<ListStorageResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::ListStorage(request.clone()));
        let keys = self
            .0
            .storage
            .lock()
            .unwrap()
            .keys()
            .filter(|(plugin, key)| {
                *plugin == request.plugin_id && key.starts_with(&request.prefix)
            })
            .map(|(_, key)| key.clone())
            .collect();
        Ok(Response::new(ListStorageResponse { keys }))
    }

    async fn get_conversation_history(
        &self,
        _request: Request<ConversationHistoryRequest>,
    ) -> Result<Response<ConversationHistoryResponse>, Status> {
        Err(Status::unavailable("no model configured"))
    }

    async fn list_conversations(
        &self,
        request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        self.0
            .record(Seen::Conversations("list", request.into_inner()));
        Ok(Response::new(conversations()))
    }

    async fn new_conversation(
        &self,
        request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        self.0
            .record(Seen::Conversations("new", request.into_inner()));
        Ok(Response::new(conversations()))
    }

    async fn switch_conversation(
        &self,
        request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        let request = request.into_inner();
        let missing = request.session_id == "missing";
        self.0.record(Seen::Select("switch", request));
        if missing {
            return Err(Status::not_found("no such conversation"));
        }
        Ok(Response::new(conversations()))
    }

    async fn delete_conversation(
        &self,
        request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        self.0.record(Seen::Select("delete", request.into_inner()));
        Ok(Response::new(ConversationList::default()))
    }

    async fn append_conversation(
        &self,
        request: Request<AppendConversationRequest>,
    ) -> Result<Response<AppendConversationResponse>, Status> {
        self.0.record(Seen::Append(request.into_inner()));
        Ok(Response::new(AppendConversationResponse {
            session_id: "s1".into(),
        }))
    }

    async fn list_personas(
        &self,
        _request: Request<ListPersonasRequest>,
    ) -> Result<Response<ListPersonasResponse>, Status> {
        self.0.record(Seen::ListPersonas);
        Ok(Response::new(ListPersonasResponse {
            personas: self.0.personas.lock().unwrap().clone(),
        }))
    }

    async fn upsert_persona(
        &self,
        request: Request<Persona>,
    ) -> Result<Response<UpsertPersonaResponse>, Status> {
        let persona = request.into_inner();
        self.0.record(Seen::UpsertPersona);
        if persona.id == "default" {
            return Err(Status::failed_precondition("the built-in persona is fixed"));
        }
        let mut personas = self.0.personas.lock().unwrap();
        let replaced = personas.iter().any(|known| known.id == persona.id);
        personas.retain(|known| known.id != persona.id);
        personas.push(persona);
        Ok(Response::new(UpsertPersonaResponse { replaced }))
    }

    async fn delete_persona(
        &self,
        request: Request<DeletePersonaRequest>,
    ) -> Result<Response<DeletePersonaResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::DeletePersona);
        let mut personas = self.0.personas.lock().unwrap();
        let before = personas.len();
        personas.retain(|known| known.id != request.id);
        Ok(Response::new(DeletePersonaResponse {
            deleted: personas.len() < before,
        }))
    }

    async fn run_agent(
        &self,
        request: Request<RunAgentRequest>,
    ) -> Result<Response<RunAgentResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::RunAgent(request.clone()));
        Ok(Response::new(RunAgentResponse {
            content: format!("answer to {}", request.prompt),
            attachments: vec![
                ToolAttachment {
                    mime_type: "image/png".into(),
                    file_path: Some("/data/chart.png".into()),
                    url: None,
                },
                ToolAttachment {
                    mime_type: "application/pdf".into(),
                    file_path: None,
                    url: Some("https://example.com/files/report.pdf?v=2".into()),
                },
                // Neither a file nor a URL: nothing to send.
                ToolAttachment {
                    mime_type: "image/png".into(),
                    file_path: None,
                    url: None,
                },
            ],
            tools: vec!["forecast".into()],
            session_id: "private-1".into(),
        }))
    }

    async fn refresh_plugin_meta(
        &self,
        request: Request<RefreshPluginMetaRequest>,
    ) -> Result<Response<RefreshPluginMetaResponse>, Status> {
        self.0.record(Seen::Refresh(request.into_inner()));
        if let Some(code) = *self.0.refresh_fails.lock().unwrap() {
            return Err(Status::new(code, "metadata refresh failed"));
        }
        Ok(Response::new(RefreshPluginMetaResponse {
            plugin_ids: vec!["test.plugin".into()],
        }))
    }

    async fn render_image(
        &self,
        request: Request<RenderImageRequest>,
    ) -> Result<Response<RenderImageResponse>, Status> {
        let request = request.into_inner();
        self.0.record(Seen::Render(request.clone()));
        let empty = matches!(
            &request.source,
            Some(render_image_request::Source::Text(text)) if text == "no file"
        );
        Ok(Response::new(RenderImageResponse {
            file_path: if empty {
                String::new()
            } else {
                "/data/render/1.png".into()
            },
            width: 720,
            height: 100,
        }))
    }
}

/// Starts a fake core and returns it with a handle identified as plugin `test.plugin` in host
/// `host-1`, plus the socket to remove afterwards.
async fn start(name: &str) -> (Arc<FakeCore>, CoreHandle, PathBuf) {
    let dir = std::env::temp_dir().join("kanon-sdk-tests");
    std::fs::create_dir_all(&dir).expect("temp socket dir");
    let socket = dir.join(format!("{name}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);

    let core = Arc::new(FakeCore::default());
    let listener = IpcListener::bind(&socket).expect("fake core binds");
    let service = BotApiServiceServer::new(Served(core.clone()));
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(listener.incoming())
            .await;
    });
    for _ in 0..50 {
        if let Ok(channel) = connect_ipc(socket.clone()).await {
            let handle = CoreHandle::new(channel).with_identity("test.plugin", "host-1");
            return (core, handle, socket);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("fake core at {} never became reachable", socket.display());
}

fn chat() -> MessageEvent {
    MessageEvent::new(
        PipelineEventRequest {
            event_id: "onebot:1".into(),
            platform: "onebot".into(),
            channel_id: "group:g1".into(),
            sender_id: "u1".into(),
            raw_text: "hi".into(),
            ..Default::default()
        },
        None,
    )
}

fn invalid(result: Result<impl std::fmt::Debug, CoreError>) -> String {
    match result {
        Err(CoreError::InvalidArgument(message)) => message,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Counter {
    hits: u32,
    last: Option<String>,
}

#[tokio::test]
async fn kv_values_round_trip_as_json_in_the_plugins_namespace() {
    let (fake, core, socket) = start("core-kv").await;

    let counter = Counter {
        hits: 3,
        last: Some("u1".into()),
    };
    core.kv_set("counter:g1", &counter).await.unwrap();
    assert_eq!(
        core.kv_get::<Counter>("counter:g1").await.unwrap(),
        Some(counter)
    );
    assert_eq!(core.kv_get::<Counter>("counter:g2").await.unwrap(), None);
    core.kv_set_with_ttl("cooldown:u1", &true, Duration::from_secs(90))
        .await
        .unwrap();
    assert_eq!(
        core.kv_keys("counter:").await.unwrap(),
        ["counter:g1".to_string()]
    );
    assert!(core.kv_delete("counter:g1").await.unwrap());
    assert!(!core.kv_delete("counter:g1").await.unwrap());

    let seen = fake.take();
    // Every storage call names the plugin's namespace; the core never infers it.
    for request in &seen {
        let plugin_id = match request {
            Seen::SetStorage(request) => &request.plugin_id,
            Seen::GetStorage(request) => &request.plugin_id,
            Seen::DeleteStorage(request) => &request.plugin_id,
            Seen::ListStorage(request) => &request.plugin_id,
            other => panic!("unexpected request {other:?}"),
        };
        assert_eq!(plugin_id, "test.plugin");
    }
    let Seen::SetStorage(first) = &seen[0] else {
        panic!("{seen:?}")
    };
    assert_eq!(first.plugin_id, "test.plugin");
    assert_eq!(first.value, br#"{"hits":3,"last":"u1"}"#);
    assert_eq!(first.ttl_seconds, 0);
    let Seen::SetStorage(with_ttl) = &seen[3] else {
        panic!("{seen:?}")
    };
    assert_eq!(
        (with_ttl.value.as_slice(), with_ttl.ttl_seconds),
        (&b"true"[..], 90)
    );

    // A stored value of another shape is an error, never `None`.
    assert!(matches!(
        core.kv_get::<u32>("cooldown:u1").await,
        Err(CoreError::Json(_))
    ));
    // The core declining to store is not success.
    assert!(matches!(
        core.kv_set("refused", &1).await,
        Err(CoreError::Unexpected(_))
    ));
    fake.take();

    // Mistakes are caught before anything is sent.
    invalid(
        core.kv_set_with_ttl("k", &1, Duration::from_millis(500))
            .await,
    );
    invalid(core.kv_get::<u32>("").await);
    invalid(core.kv_delete("").await);
    let oversized = "x".repeat(MAX_KV_VALUE_BYTES);
    assert!(invalid(core.kv_set("big", &oversized).await).contains("limit"));
    assert!(fake.take().is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn plugin_scoped_calls_need_the_plugins_identity() {
    let (fake, core, socket) = start("core-identity").await;
    let channel = connect_ipc(socket.clone()).await.unwrap();
    let anonymous = CoreHandle::new(channel);

    assert!(invalid(anonymous.kv_get::<u32>("k").await).contains("with_identity"));
    invalid(anonymous.render_text("hi").await);
    invalid(anonymous.agent("hi").await);
    invalid(anonymous.refresh_plugin_meta().await);
    assert!(fake.take().is_empty());

    assert_eq!(core.refresh_plugin_meta().await.unwrap(), ["test.plugin"]);
    let seen = fake.take();
    assert!(matches!(&seen[..], [Seen::Refresh(request)] if request.host_id == "host-1"));
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn conversation_helpers_act_on_the_chat_of_an_event() {
    let (fake, core, socket) = start("core-conversations").await;
    let event = chat();

    assert_eq!(
        core.list_conversations(&event).await.unwrap(),
        conversations().conversations
    );
    core.new_conversation(&event).await.unwrap();
    core.switch_conversation(&event, "s1").await.unwrap();
    assert!(
        core.delete_conversation(&event, "s1")
            .await
            .unwrap()
            .is_empty()
    );
    let session = core
        .append_conversation(&event, [("/roll", "4"), ("/roll", "2")])
        .await
        .unwrap();
    assert_eq!(session, "s1");

    let seen = fake.take();
    assert_eq!(seen.len(), 5);
    for (request, call) in seen[..2].iter().zip(["list", "new"]) {
        let Seen::Conversations(name, request) = request else {
            panic!("{seen:?}")
        };
        assert_eq!(*name, call);
        assert_eq!(request.context.as_ref(), Some(event.raw()));
    }
    let Seen::Select("switch", switch) = &seen[2] else {
        panic!("{seen:?}")
    };
    assert_eq!(switch.session_id, "s1");
    assert_eq!(switch.context.as_ref(), Some(event.raw()));
    let Seen::Append(append) = &seen[4] else {
        panic!("{seen:?}")
    };
    // Pairs become alternating user/assistant messages.
    let turns: Vec<_> = append
        .messages
        .iter()
        .map(|message| {
            (
                LlmRole::try_from(message.role).unwrap(),
                message.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        turns,
        [
            (LlmRole::User, "/roll"),
            (LlmRole::Assistant, "4"),
            (LlmRole::User, "/roll"),
            (LlmRole::Assistant, "2"),
        ]
    );

    assert!(matches!(
        core.switch_conversation(&event, "missing").await,
        Err(CoreError::NotFound(_))
    ));
    fake.take();
    invalid(core.switch_conversation(&event, "").await);
    invalid(
        core.append_conversation(&event, Vec::<(String, String)>::new())
            .await,
    );
    assert!(fake.take().is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn persona_helpers_manage_the_catalog() {
    let (fake, core, socket) = start("core-personas").await;
    let pirate = Persona {
        id: "pirate".into(),
        name: "Pirate".into(),
        prompt: "Talk like a pirate.".into(),
        builtin: false,
    };

    assert!(!core.upsert_persona(pirate.clone()).await.unwrap());
    assert!(core.upsert_persona(pirate.clone()).await.unwrap());
    assert_eq!(core.list_personas().await.unwrap(), [pirate.clone()]);
    assert!(matches!(
        core.upsert_persona(Persona {
            id: "default".into(),
            ..pirate.clone()
        })
        .await,
        Err(CoreError::FailedPrecondition(_))
    ));
    assert!(core.delete_persona("pirate").await.unwrap());
    assert!(!core.delete_persona("pirate").await.unwrap());
    assert_eq!(fake.take().len(), 6);

    invalid(
        core.upsert_persona(Persona {
            prompt: " ".into(),
            ..pirate
        })
        .await,
    );
    invalid(core.delete_persona("").await);
    assert!(fake.take().is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn agent_runs_carry_every_option_and_reply_with_attachments() {
    let (fake, core, socket) = start("core-agent").await;
    let event = MessageEvent::new(chat().raw().clone(), Some(core.clone()));

    let reply = event
        .agent("weather?")
        .in_conversation()
        .use_tools()
        .model("openai/gpt-5")
        .max_steps(4)
        .image(segment::image_bytes(vec![1, 2, 3], "image/png"))
        .await
        .unwrap();
    assert_eq!(reply.text, "answer to weather?");
    assert_eq!(reply.tools, ["forecast"]);
    assert_eq!(reply.session_id, "private-1");

    let seen = fake.take();
    let [Seen::RunAgent(request)] = &seen[..] else {
        panic!("{seen:?}")
    };
    assert_eq!(request.plugin_id, "test.plugin");
    assert_eq!(request.context.as_ref(), Some(event.raw()));
    assert!(request.in_conversation && request.use_tools);
    assert_eq!(
        (request.model.as_str(), request.max_steps),
        ("openai/gpt-5", 4)
    );
    assert_eq!(request.images.len(), 1);
    assert!(request.system_prompt.is_empty());

    // Returned from a handler, the reply becomes the text and the attachments it can send.
    let segments = reply.into_segments();
    assert_eq!(segments.len(), 3, "{segments:?}");
    assert!(matches!(
        &segments[0].segment,
        Some(message_segment::Segment::Text(text)) if text.content == "answer to weather?"
    ));
    assert!(matches!(
        &segments[1].segment,
        Some(message_segment::Segment::Image(ImageSegment {
            source: Some(image_segment::Source::FilePath(path)),
            ..
        })) if path == "/data/chart.png"
    ));
    assert!(matches!(
        &segments[2].segment,
        Some(message_segment::Segment::File(FileSegment {
            source: Some(file_segment::Source::Url(_)),
            name,
        })) if name == "report.pdf"
    ));

    // A private run with its own instructions, from the handle.
    core.agent("summarize")
        .system_prompt("Be brief.")
        .await
        .unwrap();
    let seen = fake.take();
    let [Seen::RunAgent(private)] = &seen[..] else {
        panic!("{seen:?}")
    };
    assert_eq!(private.system_prompt, "Be brief.");
    assert!(private.context.is_none() && !private.in_conversation);
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn agent_option_mistakes_fail_before_sending() {
    let (fake, core, socket) = start("core-agent-mistakes").await;
    let event = chat();

    invalid(core.agent("  ").await);
    invalid(core.agent("hi").in_conversation().await);
    invalid(
        core.agent("hi")
            .event(&event)
            .in_conversation()
            .system_prompt("Be brief.")
            .await,
    );
    for model in ["gpt-5", "/gpt-5", "openai/"] {
        assert!(invalid(core.agent("hi").model(model).await).contains("<provider>/<model-id>"));
    }
    // Raw bytes say nothing about their format; the model needs the MIME type.
    let unlabeled = ImageSegment {
        source: Some(image_segment::Source::RawBytes(vec![1])),
        mime_type: None,
        filename: None,
    };
    invalid(core.agent("hi").image(unlabeled).await);
    invalid(core.agent("hi").image(segment::text("not an image")).await);
    assert!(fake.take().is_empty());

    // Without a core there is nothing to call.
    assert!(matches!(
        chat().agent("hi").await,
        Err(CoreError::Standalone)
    ));
    assert!(matches!(
        chat().send("hi").await,
        Err(CoreError::Standalone)
    ));
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn model_requests_accept_raw_image_bytes_with_their_type() {
    let (fake, core, socket) = start("core-llm").await;
    let mut message = llm_message(LlmRole::User, "what is this?");
    message.images.push(ImageSegment {
        source: Some(image_segment::Source::RawBytes(vec![
            0x89, b'P', b'N', b'G',
        ])),
        mime_type: Some("image/png".into()),
        filename: None,
    });
    let request = LlmRequest {
        messages: vec![message.clone()],
        ..Default::default()
    };
    let mut stream = core.stream_llm(request).await.unwrap();
    let mut answer = String::new();
    while let Some(chunk) = stream.message().await.unwrap() {
        answer.push_str(&chunk.delta_text);
    }
    assert_eq!(answer, "saw 1 image(s)");
    assert_eq!(core.request_llm("hi").await.unwrap(), "saw 0 image(s)");
    assert_eq!(fake.take().len(), 2);

    message.images[0].mime_type = None;
    let unlabeled = LlmRequest {
        messages: vec![message],
        ..Default::default()
    };
    invalid(core.stream_llm(unlabeled).await);
    assert!(fake.take().is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn rendering_returns_a_ready_to_send_image_segment() {
    let (fake, core, socket) = start("core-render").await;

    let card = core.render_text("# Title\nbody").await.unwrap();
    assert_eq!(
        card.segment,
        Some(message_segment::Segment::Image(ImageSegment {
            source: Some(image_segment::Source::FilePath("/data/render/1.png".into())),
            mime_type: Some("image/png".into()),
            filename: None,
        }))
    );
    core.render_text_width("wide", 1200).await.unwrap();
    core.render_svg("<svg xmlns='http://www.w3.org/2000/svg'/>")
        .await
        .unwrap();
    let seen = fake.take();
    let requests: Vec<_> = seen
        .iter()
        .map(|seen| match seen {
            Seen::Render(request) => (
                request.plugin_id.as_str(),
                request.width,
                request.source.clone(),
            ),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(requests[0].1, 0);
    assert_eq!(requests[1].1, 1200);
    assert!(matches!(
        &requests[2],
        ("test.plugin", 0, Some(render_image_request::Source::Svg(_)))
    ));

    // A render without a file breaks the contract; it is not an image.
    assert!(matches!(
        core.render_text("no file").await,
        Err(CoreError::Unexpected(_))
    ));
    fake.take();
    invalid(core.render_text("  ").await);
    invalid(core.render_text_width("narrow", 100).await);
    assert!(fake.take().is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn grpc_failures_map_to_error_variants() {
    let (_fake, core, socket) = start("core-errors").await;
    let event = chat();

    let unimplemented = core.ping().await.unwrap_err();
    assert!(matches!(unimplemented, CoreError::Unimplemented(ref m) if m == "no ping here"));
    assert!(matches!(
        core.conversation_history(&event, 0).await,
        Err(CoreError::Unavailable(_))
    ));
    assert!(matches!(
        core.ingest_text("onebot", "group:g1", "u1", "hi").await,
        Err(CoreError::ResourceExhausted(_))
    ));
    assert!(matches!(
        core.reply_to(&event, "hi").await,
        Err(CoreError::DeadlineExceeded(_))
    ));
    assert!(matches!(
        core.send_message("nowhere", "c", "hi").await,
        Err(CoreError::NotFound(_))
    ));
    let other = core
        .call_platform_api("onebot", "x", serde_json::Value::Null)
        .await
        .unwrap_err();
    assert_eq!(other.code(), Some(tonic::Code::PermissionDenied));
    assert!(matches!(
        other,
        CoreError::Rpc { code: tonic::Code::PermissionDenied, ref message } if message == "not allowed"
    ));
    assert_eq!(CoreError::Standalone.code(), None);
    let _ = std::fs::remove_file(&socket);
}

#[derive(Deserialize, JsonSchema)]
struct Dice {
    /// Number of sides.
    sides: u32,
}

/// A router loaded with `core`, as the host would load it.
async fn loaded_router(core: CoreHandle) -> Router {
    let mut router = Router::new("test.plugin", "Test", "0.1.0")
        .tool(ToolSpec::new("first"), |_args, _event| async move { Ok(1) })
        .tool(ToolSpec::new("last"), |_args, _event| async move { Ok(2) });
    let mut context = PluginContext::new(std::env::temp_dir(), None).with_core(core);
    router.on_load(&mut context).await.unwrap();
    router
}

fn tool_names(router: &Router) -> Vec<String> {
    router
        .meta()
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

#[tokio::test]
async fn tools_added_at_runtime_are_announced_to_the_core() {
    let (fake, core, socket) = start("core-runtime-tools").await;
    let router = loaded_router(core).await;
    let slot = router.context();

    slot.add_tool(
        ToolSpec::typed::<Dice>("roll").description("Roll a die"),
        |dice, _event| async move { Ok(dice.sides) },
    )
    .await
    .unwrap();
    assert_eq!(tool_names(&router), ["first", "last", "roll"]);
    let seen = fake.take();
    assert!(matches!(&seen[..], [Seen::Refresh(request)] if request.host_id == "host-1"));

    let call = ToolCallRequest {
        call_id: "c1".into(),
        tool_name: "roll".into(),
        payload: Some(tool_call_request::Payload::StructuredArgs(
            kanon_sdk::json::to_struct(serde_json::Map::from_iter([(
                "sides".to_string(),
                json!(6),
            )])),
        )),
        ..Default::default()
    };
    let rolled = router.on_call_tool(call.clone()).await.unwrap();
    assert!(rolled.success, "{}", rolled.error_message);

    // A taken name is refused without bothering the core.
    assert!(matches!(
        slot.add_tool(
            ToolSpec::new("roll"),
            |args, _event| async move { Ok(args) }
        )
        .await,
        Err(CoreError::InvalidArgument(_))
    ));
    assert!(fake.take().is_empty());

    assert!(slot.remove_tool("roll").await.unwrap());
    assert!(!slot.remove_tool("roll").await.unwrap());
    assert_eq!(tool_names(&router), ["first", "last"]);
    assert_eq!(fake.take().len(), 1, "only the real removal refreshes");
    let gone = router.on_call_tool(call).await.unwrap();
    assert_eq!(gone.error_message, "Unknown tool: roll");
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn a_failed_refresh_rolls_the_tool_change_back() {
    let (fake, core, socket) = start("core-runtime-rollback").await;
    let router = loaded_router(core).await;
    let slot = router.context();
    *fake.refresh_fails.lock().unwrap() = Some(tonic::Code::Unavailable);

    let added = slot
        .add_tool(
            ToolSpec::new("roll"),
            |args, _event| async move { Ok(args) },
        )
        .await;
    assert!(matches!(added, Err(CoreError::Unavailable(_))));
    assert_eq!(tool_names(&router), ["first", "last"]);

    // Restored where it was, so the declared order (and the model's request prefix) holds.
    let removed = slot.remove_tool("first").await;
    assert!(matches!(removed, Err(CoreError::Unavailable(_))));
    assert_eq!(tool_names(&router), ["first", "last"]);
    assert_eq!(fake.take().len(), 2);
    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn without_a_core_runtime_tools_are_only_registered() {
    let router = Router::new("test.plugin", "Test", "0.1.0");
    let slot = router.context();
    slot.add_tool(
        ToolSpec::new("roll"),
        |args, _event| async move { Ok(args) },
    )
    .await
    .unwrap();
    assert_eq!(tool_names(&router), ["roll"]);
    assert!(slot.remove_tool("roll").await.unwrap());
    assert!(tool_names(&router).is_empty());
}
