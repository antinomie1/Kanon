//! Plugins inside the agent: system prompt rewrites, turn and tool events, `RunAgent`,
//! `RefreshPluginMeta`, and images given as bytes — through a real gRPC plugin host.
//!
//! The node is assembled as in production where it matters: an agent factory carrying
//! [`PluginAgentHook`], an instance catalog, and conversations in a SQLite file.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::{BotInstance, InstanceDraft, InstanceRegistry};
use kanon_core::ipc::CoreApiService;
use kanon_core::pipeline::{PipelineEngine, PipelineResult};
use kanon_core::supervisor::{ManagedHost, Supervisor};
use kanon_core::{PluginAgentHook, SkillCatalogHook, SkillStore, ToggleStore};
use kanon_llm::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, ToolCall,
};
use kanon_llm::{
    AgentConfig, AgentFactory, AgentHook, AgentSlot, GatewayError, LlmProvider, Persona,
    PersonaStore, SessionManager, SqliteMemory, SqliteSessionStore,
};
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::message_pipeline_service_server::{
    MessagePipelineService, MessagePipelineServiceServer,
};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DecorateReplyRequest, DecorateReplyResult,
    DeliverMessageRequest, DeliverMessageResponse, EventAck, EventKind, EventNotification,
    GetPluginMetaRequest, GetPluginMetaResponse, HttpRequest, HttpResponse, ImageSegment,
    LlmMessage, LlmRequest, LlmRequestHookRequest, LlmRequestHookResult, LlmRole, PingRequest,
    PingResponse, PipelineEventRequest, PluginActionRequest, PluginActionResponse, PluginMeta,
    PreFilterResult, PrepareTurnRequest, PrepareTurnResult, RefreshPluginMetaRequest,
    ReloadPluginConfigRequest, ReloadPluginConfigResponse, RunAgentRequest, ToolCallRequest,
    ToolCallResponse, ToolMeta, event_notification::Detail, image_segment, tool_call_response,
};
use kanon_transport::connect_ipc;
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use tonic::{Code, Request, Response, Status};

const PLUGIN: &str = "org.test.hooks";
const HOST: &str = "host_hooks";
const RULE: &str = "[plugin rule] answer briefly";

/// What the model was asked, one entry per request.
#[derive(Debug, Clone)]
struct Seen {
    system: String,
    tools: Vec<String>,
    messages: Vec<ChatMessage>,
}

/// Model that calls `lookup` when asked to "use tool", never answers "work", and otherwise
/// answers `reply <n>`.
#[derive(Default)]
struct Model {
    seen: Mutex<Vec<Seen>>,
    working: Notify,
}

#[async_trait]
impl LlmProvider for Model {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let last = request.messages.last().cloned().expect("a message");
        if last
            .content
            .as_deref()
            .is_some_and(|text| text.contains("work"))
        {
            self.working.notify_one();
            return std::future::pending().await;
        }
        let mut seen = self.seen.lock().unwrap();
        seen.push(Seen {
            system: request
                .messages
                .first()
                .filter(|message| message.role == Role::System)
                .and_then(|message| message.content.clone())
                .unwrap_or_default(),
            tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            messages: request.messages.clone(),
        });
        let asks_for_tool = last.role == Role::User
            && last
                .content
                .as_deref()
                .is_some_and(|text| text.contains("use tool"));
        if asks_for_tool && request.tools.iter().any(|tool| tool.name == "lookup") {
            return Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "lookup".to_string(),
                    arguments: serde_json::json!({ "q": "x" }),
                }],
                finish_reason: Some("tool_calls".to_string()),
                ..ChatResponse::default()
            });
        }
        let content = if last.role == Role::Tool {
            "done after tool".to_string()
        } else {
            format!("reply {}", seen.len())
        };
        Ok(ChatResponse {
            content: Some(content),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        })
    }
}

/// The plugin: rewrites the system prompt (and fails to for messages saying "broken"), serves
/// `lookup`, records the events it subscribed to, and reports whatever metadata it holds now.
#[derive(Clone)]
struct Plugin {
    metas: Arc<Mutex<Vec<PluginMeta>>>,
    rewrites: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<Detail>>>,
    tool_pause: Arc<Mutex<Option<Arc<ToolPause>>>>,
}

/// Holds one tool call between the first and second model requests.
#[derive(Default)]
struct ToolPause {
    entered: Notify,
    release: Notify,
}

fn plugin_meta(tools: &[&str]) -> PluginMeta {
    PluginMeta {
        id: PLUGIN.to_string(),
        tools: tools
            .iter()
            .map(|name| ToolMeta {
                name: name.to_string(),
                description: format!("{name} things"),
                parameters: None,
            })
            .collect(),
        events: [
            EventKind::AgentBegin,
            EventKind::AgentDone,
            EventKind::ToolCall,
            EventKind::ToolResult,
        ]
        .into_iter()
        .map(|kind| kind as i32)
        .collect(),
        rewrites_system_prompt: true,
        ..Default::default()
    }
}

#[tonic::async_trait]
impl PluginHostService for Plugin {
    async fn ping(&self, _: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse::default()))
    }

    async fn reload_plugin_config(
        &self,
        _: Request<ReloadPluginConfigRequest>,
    ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
        Err(Status::unimplemented("reload"))
    }

    async fn get_plugin_meta(
        &self,
        _: Request<GetPluginMetaRequest>,
    ) -> Result<Response<GetPluginMetaResponse>, Status> {
        Ok(Response::new(GetPluginMetaResponse {
            plugins: self.metas.lock().unwrap().clone(),
        }))
    }

    async fn invoke_action(
        &self,
        _: Request<PluginActionRequest>,
    ) -> Result<Response<PluginActionResponse>, Status> {
        Err(Status::unimplemented("actions"))
    }
}

#[tonic::async_trait]
impl MessagePipelineService for Plugin {
    async fn on_pre_filter(
        &self,
        _: Request<PipelineEventRequest>,
    ) -> Result<Response<PreFilterResult>, Status> {
        Ok(Response::new(PreFilterResult::default()))
    }

    async fn on_execute_command(
        &self,
        _: Request<CommandExecuteRequest>,
    ) -> Result<Response<CommandExecuteResponse>, Status> {
        Err(Status::unimplemented("commands"))
    }

    async fn on_call_tool(
        &self,
        request: Request<ToolCallRequest>,
    ) -> Result<Response<ToolCallResponse>, Status> {
        let req = request.into_inner();
        let pause = self.tool_pause.lock().unwrap().clone();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        Ok(Response::new(ToolCallResponse {
            call_id: req.call_id,
            success: true,
            payload: kanon_llm::tool_router::json_to_prost_struct(
                &serde_json::json!({ "answer": 42 }),
            )
            .map(tool_call_response::Payload::StructuredResult),
            ..Default::default()
        }))
    }

    async fn on_event(
        &self,
        request: Request<EventNotification>,
    ) -> Result<Response<EventAck>, Status> {
        if let Some(detail) = request.into_inner().detail {
            self.events.lock().unwrap().push(detail);
        }
        Ok(Response::new(EventAck { received: true }))
    }

    async fn on_deliver_message(
        &self,
        _: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        Err(Status::unimplemented("not an adapter"))
    }

    async fn on_decorate_reply(
        &self,
        _: Request<DecorateReplyRequest>,
    ) -> Result<Response<DecorateReplyResult>, Status> {
        Ok(Response::new(DecorateReplyResult::default()))
    }

    async fn on_prepare_turn(
        &self,
        _: Request<PrepareTurnRequest>,
    ) -> Result<Response<PrepareTurnResult>, Status> {
        Ok(Response::new(PrepareTurnResult::default()))
    }

    async fn on_llm_request(
        &self,
        request: Request<LlmRequestHookRequest>,
    ) -> Result<Response<LlmRequestHookResult>, Status> {
        let req = request.into_inner();
        self.rewrites.lock().unwrap().push(req.session_id);
        if req
            .context
            .is_some_and(|context| context.raw_text.contains("broken"))
        {
            return Err(Status::internal("the plugin broke"));
        }
        Ok(Response::new(LlmRequestHookResult {
            system_prompt: Some(format!("{}\n\n{RULE}", req.system_prompt)),
        }))
    }

    async fn on_http_request(
        &self,
        _: Request<HttpRequest>,
    ) -> Result<Response<HttpResponse>, Status> {
        Err(Status::unimplemented("http"))
    }
}

struct Node {
    engine: Arc<PipelineEngine>,
    api: CoreApiService,
    factory: Arc<AgentFactory>,
    model: Arc<Model>,
    sessions: Arc<SessionManager>,
    plugin: Plugin,
    rewriter: Arc<PluginAgentHook>,
    supervisor: Arc<Supervisor>,
    /// The session the test chat's messages go to.
    session: String,
}

async fn start(dir: &Path) -> Node {
    let db = dir.join("sessions.db");
    let sessions = Arc::new(
        SessionManager::new(Arc::new(SqliteMemory::open(&db).expect("memory")))
            .with_store(Arc::new(SqliteSessionStore::open(&db).expect("store")))
            .expect("stored sessions load"),
    );
    let registry = Arc::new(
        InstanceRegistry::open(dir.join("instances.json"))
            .await
            .expect("instance catalog"),
    );
    let instance = registry
        .create(
            InstanceDraft {
                name: "Test Bot".to_string(),
                enabled: true,
                adapters: vec!["qq".to_string()],
                ..InstanceDraft::default()
            },
            None,
        )
        .await
        .expect("instance");
    let personas = Arc::new(
        PersonaStore::new(dir.join("personas.json"))
            .load_registry()
            .expect("personas"),
    );

    let model = Arc::new(Model::default());
    let rewriter = Arc::new(PluginAgentHook::new());
    let skills = Arc::new(SkillStore::new(dir.join("skills")));
    let toggles = Arc::new(ToggleStore::in_memory());
    let factory = Arc::new(AgentFactory::new(
        "test",
        Arc::new(AgentSlot::new()),
        sessions.memory().clone(),
        sessions.clone(),
        personas,
        vec![
            Arc::new(SkillCatalogHook::new(
                skills.clone(),
                toggles.clone(),
                registry.clone(),
            )),
            rewriter.clone(),
        ],
        vec![Arc::new(kanon_core::ReadSkillTool::new(
            skills,
            toggles,
            registry.clone(),
        ))],
    ));
    factory.install(
        "test",
        model.clone(),
        AgentConfig {
            default_model: "model".to_string(),
            provider: Some("test".to_string()),
            ..AgentConfig::default()
        },
    );

    let supervisor = Arc::new(Supervisor::new(Some(dir.join("run")), None));
    let plugin = Plugin {
        metas: Arc::new(Mutex::new(vec![plugin_meta(&["lookup"])])),
        rewrites: Arc::default(),
        events: Arc::default(),
        tool_pause: Arc::default(),
    };
    serve_plugin(&supervisor, &dir.join("host.sock"), plugin.clone()).await;

    let engine = Arc::new(
        PipelineEngine::new(supervisor.clone())
            .with_agent_factory(factory.clone())
            .with_instances(registry),
    );
    let api = CoreApiService::new(tokio::sync::mpsc::channel(1).0)
        .with_supervisor(supervisor.clone())
        .with_engine(engine.clone())
        .with_agent_slot(factory.slot().clone());
    Node {
        engine,
        api,
        factory,
        model,
        sessions,
        plugin,
        rewriter,
        supervisor,
        session: instance.conversation_session_id("group:1:user:1"),
    }
}

async fn serve_plugin(supervisor: &Supervisor, socket: &Path, plugin: Plugin) {
    let listener = kanon_transport::IpcListener::bind(socket).expect("host socket binds");
    let incoming = listener.incoming();
    let metas = plugin.metas.lock().unwrap().clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(plugin.clone()))
            .add_service(MessagePipelineServiceServer::new(plugin))
            .serve_with_incoming(incoming)
            .await;
    });
    let mut channel = None;
    for _ in 0..50 {
        match connect_ipc(socket).await {
            Ok(candidate) => {
                channel = Some(candidate);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    supervisor
        .register_managed_host(Arc::new(ManagedHost::new(
            HOST.to_string(),
            socket.to_path_buf(),
            channel.expect("plugin host reachable"),
            metas,
            100,
        )))
        .await;
}

fn event(id: &str, text: &str) -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: id.to_string(),
        platform: "qq".to_string(),
        channel_id: "group:1".to_string(),
        sender_id: "user:1".to_string(),
        raw_text: text.to_string(),
        segments: Vec::new(),
        metadata: None,
    }
}

async fn say(node: &Node, id: &str, text: &str) -> String {
    match node.engine.process_event(event(id, text)).await {
        PipelineResult::LlmReplied { content, .. } => content,
        other => panic!("expected a reply to '{text}', got {other:?}"),
    }
}

fn seen(node: &Node) -> Vec<Seen> {
    node.model.seen.lock().unwrap().clone()
}

fn rewrites(node: &Node) -> usize {
    node.plugin.rewrites.lock().unwrap().len()
}

/// Waits until the plugin has received `count` events; events are sent fire-and-forget.
async fn events(node: &Node, count: usize) -> Vec<Detail> {
    for _ in 0..200 {
        if node.plugin.events.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    node.plugin.events.lock().unwrap().clone()
}

fn run(prompt: &str) -> RunAgentRequest {
    RunAgentRequest {
        plugin_id: PLUGIN.to_string(),
        prompt: prompt.to_string(),
        context: Some(event("run", "a message the plugin holds")),
        ..Default::default()
    }
}

#[tokio::test]
async fn plugins_rewrite_the_system_prompt_once_per_turn_and_its_compaction_reuses_it() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;

    // One turn, two model requests (the tool call and the answer): the plugin is asked once and
    // both requests open with the same rewritten prompt.
    assert_eq!(say(&node, "e1", "please use tool").await, "done after tool");
    let first = seen(&node);
    assert_eq!(first.len(), 2);
    assert_eq!(rewrites(&node), 1, "asked once per turn, not per request");
    assert!(first[0].system.ends_with(RULE), "{}", first[0].system);
    assert_eq!(first[0].system, first[1].system);
    assert_eq!(node.plugin.rewrites.lock().unwrap()[0], node.session);

    // The next turn asks again and gets the same prompt: the cached prefix stays intact.
    say(&node, "e2", "hello again").await;
    assert_eq!(rewrites(&node), 2);
    assert_eq!(seen(&node)[2].system, first[0].system);

    // Compaction runs outside any turn, so no plugin is asked, yet it sends the turn's prompt.
    let agent = node.factory.node_agent().expect("agent");
    assert!(
        agent
            .compact_session(&node.session, &[])
            .await
            .expect("compacted")
    );
    let summary_request = seen(&node).pop().unwrap();
    assert_eq!(summary_request.system, first[0].system);
    assert_eq!(rewrites(&node), 2);

    // A failing plugin costs its own effect, never the turn.
    say(&node, "e3", "the plugin is broken now").await;
    let after = seen(&node).pop().unwrap();
    assert!(!after.system.contains(RULE), "{}", after.system);
}

#[tokio::test]
async fn skill_edits_and_rewrite_cache_eviction_do_not_change_an_active_turn() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("skills/alpha");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill,
        "---\nname: alpha\ndescription: Original skill\n---\n\nSkill body",
    )
    .unwrap();
    let node = start(dir.path()).await;
    let pause = Arc::new(ToolPause::default());
    *node.plugin.tool_pause.lock().unwrap() = Some(pause.clone());
    let engine = node.engine.clone();
    let turn = tokio::spawn(async move {
        engine
            .process_event(event("held-turn", "please use tool"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), pause.entered.notified())
        .await
        .unwrap();

    std::fs::write(
        &skill,
        "---\nname: alpha\ndescription: Updated skill\n---\n\nSkill body",
    )
    .unwrap();
    let hosts = node.supervisor.get_all_hosts().await;
    // Force the bounded manual-compaction cache to forget this still-running turn. These other
    // sessions only prepare their prompts; no model calls or durable session records are needed.
    for index in 0..257 {
        let mut prompt = kanon_llm::BASE_PERSONA_PROMPT.to_string();
        kanon_core::pipeline::with_turn(
            event(&format!("other-{index}"), "another chat"),
            hosts.clone(),
            node.rewriter
                .on_system_prompt(&format!("other:{index}"), &mut prompt, &[]),
        )
        .await
        .unwrap();
    }
    *node.plugin.tool_pause.lock().unwrap() = None;
    pause.release.notify_one();
    assert!(matches!(
        turn.await.unwrap(),
        PipelineResult::LlmReplied { .. }
    ));

    let requests = seen(&node);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].system.contains("Original skill"));
    assert_eq!(requests[0].system, requests[1].system);
    assert_eq!(
        node.plugin
            .rewrites
            .lock()
            .unwrap()
            .iter()
            .filter(|session| *session == &node.session)
            .count(),
        1
    );

    say(&node, "next-turn", "hello again").await;
    assert!(seen(&node)[2].system.contains("Updated skill"));
    assert_eq!(
        node.plugin
            .rewrites
            .lock()
            .unwrap()
            .iter()
            .filter(|session| *session == &node.session)
            .count(),
        2
    );
}

#[tokio::test]
async fn subscribers_hear_the_turn_and_every_tool_call() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;

    say(&node, "e1", "please use tool").await;
    let received = events(&node, 4).await;
    assert_eq!(received.len(), 4, "{received:?}");

    let begin = received.iter().find_map(|detail| match detail {
        Detail::AgentBegin(begin) => Some(begin.clone()),
        _ => None,
    });
    assert_eq!(begin.expect("AGENT_BEGIN").session_id, node.session);
    let call = received.iter().find_map(|detail| match detail {
        Detail::ToolCall(call) => Some(call.clone()),
        _ => None,
    });
    let call = call.expect("TOOL_CALL");
    assert_eq!(call.tool_name, "lookup");
    assert_eq!(call.context.expect("context").event_id, "e1");
    let result = received.iter().find_map(|detail| match detail {
        Detail::ToolResult(result) => Some(result.clone()),
        _ => None,
    });
    let result = result.expect("TOOL_RESULT");
    assert!(result.success);
    assert!(result.result.contains("42"), "{}", result.result);
    let done = received.iter().find_map(|detail| match detail {
        Detail::AgentDone(done) => Some(done.clone()),
        _ => None,
    });
    let done = done.expect("AGENT_DONE");
    assert!(done.success);
    assert_eq!(done.content, "done after tool");
    assert_eq!(done.tools, ["lookup"]);
}

#[tokio::test]
async fn invalid_personas_keep_their_notice_and_event_category_without_calling_the_model() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let instances = node.engine.instances().unwrap();
    let instance_id = BotInstance::instance_id_from_session(&node.session).unwrap();
    let draft = InstanceDraft {
        name: "Test Bot".to_string(),
        enabled: true,
        adapters: vec!["qq".to_string()],
        ..Default::default()
    };
    let personas = node.factory.personas();
    personas
        .register(Persona::custom("missing", "Missing", "", "Temporary persona").unwrap())
        .unwrap();
    instances
        .update(
            instance_id,
            InstanceDraft {
                persona_id: Some("missing".to_string()),
                ..draft.clone()
            },
            Some(kanon_core::InstanceRuntime {
                personas: personas.as_ref(),
                sessions: node.sessions.as_ref(),
                providers: node.factory.providers(),
                default_agent: "builtin",
            }),
        )
        .await
        .unwrap();
    // Simulate an inconsistent catalog; request resolution must not blame the provider.
    personas.remove("missing").unwrap();

    for (index, (id, category, notice)) in [
        ("instance", "request_error", "请求配置无效"),
        ("session", "request_error", "请求配置无效"),
    ]
    .into_iter()
    .enumerate()
    {
        if id == "session" {
            instances
                .update(
                    instance_id,
                    draft.clone(),
                    Some(kanon_core::InstanceRuntime {
                        personas: personas.as_ref(),
                        sessions: node.sessions.as_ref(),
                        providers: node.factory.providers(),
                        default_agent: "builtin",
                    }),
                )
                .await
                .unwrap();
            node.sessions.set_persona(&node.session, "missing").unwrap();
        }

        let outcome = node.engine.process_event(event(id, "hello")).await;
        let PipelineResult::LlmFailed { replies, error } = outcome else {
            panic!("expected a failed turn, got {outcome:?}");
        };
        assert!(!error.contains("response from model provider"), "{error}");
        let Some(Segment::Text(text)) = &replies[0].segment else {
            panic!("expected a text failure notice");
        };
        assert!(text.content.contains(notice), "{}", text.content);
        assert!(!text.content.contains("模型的结果无法处理"));
        assert!(
            seen(&node).is_empty(),
            "local failures never call the model"
        );

        let received = events(&node, (index + 1) * 2).await;
        assert_eq!(received.len(), (index + 1) * 2, "{received:?}");
        assert!(received.iter().any(|detail| matches!(
            detail,
            Detail::AgentBegin(begin) if begin.context.as_ref().is_some_and(|context| context.event_id == id)
        )));
        let done = received
            .iter()
            .find_map(|detail| match detail {
                Detail::AgentDone(done)
                    if done
                        .context
                        .as_ref()
                        .is_some_and(|context| context.event_id == id) =>
                {
                    Some(done)
                }
                _ => None,
            })
            .expect("every begun turn finishes, including instance lookup failures");
        assert!(!done.success);
        assert_eq!(done.error, category);
    }
}

#[tokio::test]
async fn a_private_run_is_the_plugins_own_and_leaves_no_trace() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;

    let answer = node
        .api
        .run_agent(Request::new(RunAgentRequest {
            system_prompt: "You are a calculator.".to_string(),
            ..run("what is 6 x 7")
        }))
        .await
        .expect("run")
        .into_inner();
    assert_eq!(answer.content, "reply 1");
    assert!(
        answer.session_id.starts_with(&format!("plugin:{PLUGIN}:")),
        "{}",
        answer.session_id
    );
    let request = seen(&node).pop().unwrap();
    assert_eq!(request.system, "You are a calculator.");
    assert!(request.tools.is_empty(), "tools are opt-in");
    assert_eq!(rewrites(&node), 0, "no plugin hook runs for a private run");
    assert!(
        node.sessions
            .sessions_with_prefix("plugin:")
            .await
            .expect("sessions")
            .is_empty(),
        "nothing reaches the conversation store"
    );

    // With tools, the instance's tools are offered and called.
    let answer = node
        .api
        .run_agent(Request::new(RunAgentRequest {
            use_tools: true,
            ..run("use tool to look it up")
        }))
        .await
        .expect("run")
        .into_inner();
    assert_eq!(answer.content, "done after tool");
    assert_eq!(answer.tools, ["lookup"]);
}

#[tokio::test]
async fn a_run_in_the_conversation_is_one_of_its_turns_and_never_cuts_into_another() {
    let dir = tempfile::tempdir().expect("dir");
    let node = Arc::new(start(dir.path()).await);

    let answer = node
        .api
        .run_agent(Request::new(RunAgentRequest {
            in_conversation: true,
            ..run("remember the code 7")
        }))
        .await
        .expect("run")
        .into_inner();
    assert_eq!(answer.session_id, node.session);
    assert_eq!(rewrites(&node), 1, "the conversation's plugins take part");

    say(&node, "e1", "what was the code?").await;
    let history: Vec<String> = seen(&node)
        .pop()
        .unwrap()
        .messages
        .iter()
        .filter(|message| message.role != Role::System)
        .filter_map(|message| message.content.clone())
        .collect();
    assert_eq!(history[0], "remember the code 7");
    assert_eq!(history[1], "reply 1");

    // While the chat's own turn runs, a run in its conversation is refused, not queued.
    let running = {
        let node = node.clone();
        tokio::spawn(async move { node.engine.process_event(event("e2", "work hard")).await })
    };
    node.model.working.notified().await;
    let err = node
        .api
        .run_agent(Request::new(RunAgentRequest {
            in_conversation: true,
            ..run("are you there")
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    running.abort();
}

#[tokio::test]
async fn malformed_runs_are_refused_up_front() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let refuse = |request: RunAgentRequest| {
        let api = node.api.clone();
        async move {
            api.run_agent(Request::new(request))
                .await
                .unwrap_err()
                .code()
        }
    };

    let no_chat = RunAgentRequest {
        in_conversation: true,
        context: None,
        ..run("hi")
    };
    assert_eq!(refuse(no_chat).await, Code::InvalidArgument);
    let elsewhere = RunAgentRequest {
        context: Some(PipelineEventRequest {
            platform: "telegram".to_string(),
            ..event("x", "hi")
        }),
        ..run("hi")
    };
    assert_eq!(refuse(elsewhere).await, Code::NotFound);
    // The model is not set up for images, so an image is refused rather than dropped.
    let image = RunAgentRequest {
        images: vec![ImageSegment {
            source: Some(image_segment::Source::Url(
                "https://example.com/a.png".to_string(),
            )),
            mime_type: Some("image/png".to_string()),
            ..Default::default()
        }],
        ..run("look")
    };
    assert_eq!(refuse(image).await, Code::InvalidArgument);
    assert!(seen(&node).is_empty(), "nothing reached the model");
}

#[tokio::test]
async fn a_plugin_adds_and_removes_tools_at_runtime() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let tools_offered = |node: &Node| seen(node).pop().unwrap().tools;

    *node.plugin.metas.lock().unwrap() = vec![plugin_meta(&["lookup", "extra"])];
    say(&node, "e1", "hi").await;
    assert_eq!(
        tools_offered(&node),
        ["lookup", "read_skill"],
        "not before the refresh"
    );

    let refreshed = node
        .api
        .refresh_plugin_meta(Request::new(RefreshPluginMetaRequest {
            host_id: HOST.to_string(),
        }))
        .await
        .expect("refreshed")
        .into_inner();
    assert_eq!(refreshed.plugin_ids, [PLUGIN]);
    say(&node, "e2", "hi").await;
    assert_eq!(tools_offered(&node), ["extra", "lookup", "read_skill"]);

    // A host that stops declaring its plugin keeps what it had.
    *node.plugin.metas.lock().unwrap() = Vec::new();
    let err = node
        .api
        .refresh_plugin_meta(Request::new(RefreshPluginMetaRequest {
            host_id: HOST.to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    say(&node, "e3", "hi").await;
    assert_eq!(tools_offered(&node), ["extra", "lookup", "read_skill"]);

    let err = node
        .api
        .refresh_plugin_meta(Request::new(RefreshPluginMetaRequest {
            host_id: "nobody".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
}

#[tokio::test]
async fn request_llm_takes_images_as_bytes() {
    let dir = tempfile::tempdir().expect("dir");
    let node = start(dir.path()).await;
    let request = |mime_type: Option<&str>| LlmRequest {
        messages: vec![LlmMessage {
            role: LlmRole::User as i32,
            text: "what is this".to_string(),
            images: vec![ImageSegment {
                source: Some(image_segment::Source::RawBytes(vec![
                    0x89, b'P', b'N', b'G',
                ])),
                mime_type: mime_type.map(str::to_string),
                ..Default::default()
            }],
        }],
        ..Default::default()
    };

    let mut stream = node
        .api
        .request_llm(Request::new(request(Some("image/png"))))
        .await
        .expect("request")
        .into_inner();
    while stream.next().await.is_some() {}
    let user = seen(&node).pop().unwrap().messages.pop().unwrap();
    let parts = user.parts.expect("the image reached the model");
    assert!(
        matches!(&parts[0], ContentPart::Image { url: Some(url), .. } if url.starts_with("data:image/png;base64,")),
        "{parts:?}"
    );

    let err = node
        .api
        .request_llm(Request::new(request(None)))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        Code::InvalidArgument,
        "bytes need their MIME type"
    );
}
