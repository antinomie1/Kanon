//! Offline sandbox: a small Kanon node, in this process, serving one plugin.
//!
//! The plugin runs exactly as it would on a real node: in its own host process, with its
//! dependencies installed by its own tool, behind the real message pipeline, with the central KV
//! store, conversations (`/ls`, `/switch`, `/del`, `/new`), the agent and its plugin hooks. Only
//! the two things a developer machine lacks are replaced:
//!
//! - **the chat platform** — a built-in `sandbox` adapter whose messages come from the terminal
//!   (or `-m`) and whose deliveries are printed as `bot>` lines;
//! - **the model** — a deterministic mock that echoes what it received, calls a tool when a
//!   message says `!tool <name> [json]`, and reports the tool's result.
//!
//! Everything lives in a temporary directory, so each run starts from an empty store.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::{TempDir, tempdir};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::ipc::DEFAULT_INGEST_QUEUE_CAPACITY;
use kanon_core::pipeline::DeadLetterWriter;
use kanon_core::pipeline::engine::OutboundMessage;
use kanon_core::supervisor::{ManagedHost, Supervisor, SupervisorError};
use kanon_core::{
    AdapterError, Capability, CommandPolicy, CommandPolicyStore, CoreApiService, CoreIpcServer,
    PipelineEngine, PipelineResult, PlatformAdapter, PluginAgentHook,
};
use kanon_llm::{
    AgentConfig, AgentFactory, AgentSlot, ChatRequest, ChatResponse, GatewayError, LlmProvider,
    PersonaStore, Role, SessionManager, SqliteMemory, SqliteSessionStore, ToolCall,
};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, DeliverMessageResponse, MessageSegment, PipelineEventRequest,
    TextSegment, ToolCallRequest, ToolCallResponse, audio_segment, image_segment,
    tool_call_request, tool_call_response, video_segment,
};

use crate::build::{BuildError, build_plugin};
use crate::lint::{LintError, find_manifest_path};

/// Platform name of the sandbox's built-in adapter.
const PLATFORM: &str = "sandbox";

/// The one chat the sandbox simulates: a private conversation with the developer.
const CHANNEL: &str = "sandbox-chat";

/// Sender of every simulated message. Listed as a bot administrator, so commands restricted to
/// administrators can be tried too.
const SENDER: &str = "developer";

/// Model reference the mock answers under (`<provider>/<model-id>`).
const MOCK_PROVIDER: &str = "sandbox";
const MOCK_MODEL: &str = "mock";

/// Session id given to tools called directly (`-t`, `:call`): such a call belongs to no
/// conversation turn.
const DIRECT_CALL_SESSION: &str = "sandbox-direct";

/// Errors occurring during sandbox execution.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Manifest location or validation failure.
    #[error("Manifest lookup error: {0}")]
    Lint(#[from] LintError),
    /// Host supervisor failure.
    #[error("Supervisor lifecycle error: {0}")]
    Supervisor(#[from] SupervisorError),
    /// File I/O failure.
    #[error("I/O error in sandbox: {0}")]
    Io(#[from] std::io::Error),
    /// A part of the sandbox node could not be assembled.
    #[error("Sandbox setup failed: {0}")]
    Setup(String),
    /// gRPC status error.
    #[error("gRPC RPC error from host: {0}")]
    Rpc(Box<tonic::Status>),
    /// Specified command was not found on the plugin.
    #[error("Command '/{0}' is not declared by any loaded plugin")]
    CommandNotFound(String),
    /// The command ran and reported a failure, or was refused.
    #[error("Command '/{0}' failed")]
    CommandFailed(String),
    /// Specified tool was not found on the plugin.
    #[error("Tool '{0}' is not declared by the plugin")]
    ToolNotFound(String),
    /// The tool ran and reported a failure.
    #[error("Tool '{0}' failed")]
    ToolFailed(String),
    /// The plugin could not be built.
    #[error("Build failed: {0}")]
    Build(#[from] BuildError),
    /// JSON parsing error when processing tool arguments.
    #[error("Invalid JSON tool arguments: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<tonic::Status> for SandboxError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

/// Execution options configuring sandbox behavior.
#[derive(Debug, Clone, Default)]
pub struct SandboxOptions {
    /// Command to run through the pipeline (e.g. `pycalc` or `/pycalc`); fails the run when no
    /// plugin answers it or the plugin reports a failure.
    pub command: Option<String>,
    /// Tool to call directly, bypassing the model.
    pub tool: Option<String>,
    /// Arguments of the command, or the tool's JSON arguments.
    pub args: Vec<String>,
    /// Messages to send in order, as the developer would type them (`/note add milk`,
    /// `count to 3`, `!tool dice {}`), each answered before the next is sent.
    pub messages: Vec<String>,
    /// Whether to run without opening an interactive terminal REPL.
    pub non_interactive: bool,
}

/// Renders a list of MessageSegments into a readable string.
fn format_segments(segments: &[MessageSegment]) -> String {
    let mut out = Vec::new();
    for seg in segments {
        if let Some(ref inner) = seg.segment {
            match inner {
                Segment::Text(t) => out.push(t.content.clone()),
                Segment::Image(i) => match &i.source {
                    Some(image_segment::Source::Url(u)) => out.push(format!("[Image URL: {}]", u)),
                    Some(image_segment::Source::FilePath(p)) => {
                        out.push(format!("[Image File: {}]", p))
                    }
                    Some(image_segment::Source::RawBytes(b)) => {
                        out.push(format!("[Image Bytes: {}B]", b.len()))
                    }
                    None => out.push("[Image]".to_string()),
                },
                Segment::Audio(a) => match &a.source {
                    Some(audio_segment::Source::Url(u)) => out.push(format!("[Audio URL: {}]", u)),
                    Some(audio_segment::Source::FilePath(p)) => {
                        out.push(format!("[Audio File: {}]", p))
                    }
                    Some(audio_segment::Source::RawBytes(b)) => {
                        out.push(format!("[Audio Bytes: {}B]", b.len()))
                    }
                    None => out.push("[Audio]".to_string()),
                },
                Segment::Video(v) => match &v.source {
                    Some(video_segment::Source::Url(u)) => out.push(format!("[Video URL: {}]", u)),
                    Some(video_segment::Source::FilePath(p)) => {
                        out.push(format!("[Video File: {}]", p))
                    }
                    Some(video_segment::Source::RawBytes(b)) => {
                        out.push(format!("[Video Bytes: {}B]", b.len()))
                    }
                    None => out.push("[Video]".to_string()),
                },
                Segment::File(f) => out.push(format!("[File: {}]", f.name)),
                Segment::Face(f) => out.push(format!("[Face: {}]", f.id)),
                Segment::Mention(m) => out.push(format!("@{}", m.target_user_id)),
                Segment::Reply(r) => {
                    out.push(format!("[Reply to {}: {}]", r.target_message_id, r.snippet))
                }
                Segment::Custom(c) => out.push(format!("[Custom: {}]", c.type_name)),
            }
        }
    }
    out.join("")
}

/// Prints `text` after `label`, indenting continuation lines under the first.
fn print_labeled(label: &str, text: &str) {
    let indent = " ".repeat(label.len() + 1);
    let mut lines = text.lines();
    println!("{label} {}", lines.next().unwrap_or_default());
    for line in lines {
        println!("{indent}{line}");
    }
}

/// The sandbox's chat platform: every message the node delivers is printed to the terminal.
///
/// It declares the media capabilities so plugins' images, voice, video and files are shown as
/// what they are instead of being reduced to text fallbacks.
#[derive(Default)]
struct SandboxAdapter {
    delivered: AtomicU64,
}

#[async_trait]
impl PlatformAdapter for SandboxAdapter {
    fn platform(&self) -> &str {
        PLATFORM
    }

    fn display_name(&self) -> &str {
        "Sandbox terminal"
    }

    fn capabilities(&self) -> &[Capability] {
        &[
            Capability::SendImage,
            Capability::SendVoice,
            Capability::SendVideo,
            Capability::SendFile,
        ]
    }

    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        // An empty delivery is the sandbox's own flush marker (see `Sandbox::send`): it only
        // proves that everything queued before it has been printed.
        if request.segments.is_empty() {
            return Ok(DeliverMessageResponse {
                success: true,
                ..DeliverMessageResponse::default()
            });
        }
        let number = self.delivered.fetch_add(1, Ordering::Relaxed) + 1;
        print_labeled("bot>", &format_segments(&request.segments));
        Ok(DeliverMessageResponse {
            success: true,
            message_id: format!("sandbox-{number}"),
            error_message: String::new(),
        })
    }
}

/// A model that needs no network and answers predictably.
///
/// - A user message containing `!tool <name> [json]` makes it call that tool (by its plugin
///   name or the namespaced name the model sees) with the JSON arguments, `{}` when omitted.
/// - After a tool ran, it answers with the tool's result.
/// - Anything else is echoed back, exactly as the model received it.
///
/// It keeps the last request so the developer can inspect the system prompt and tools plugins
/// produced (`:prompt`).
#[derive(Default)]
struct MockModel {
    last_request: Mutex<Option<ChatRequest>>,
    calls: AtomicU64,
}

impl MockModel {
    /// The last request the agent sent, if the model was asked anything yet.
    fn last_request(&self) -> Option<ChatRequest> {
        self.last_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// A plain text answer that ends the turn.
    fn answer(text: String) -> ChatResponse {
        ChatResponse {
            content: Some(text),
            finish_reason: Some("stop".to_string()),
            ..ChatResponse::default()
        }
    }

    /// Answers a user message: a tool call when it asks for one, otherwise an echo.
    fn answer_user(&self, text: &str, request: &ChatRequest) -> ChatResponse {
        let Some(directive) = text
            .lines()
            .find_map(|line| line.split_once("!tool").map(|(_, rest)| rest.trim()))
        else {
            return Self::answer(format!("(mock model) received:\n{text}"));
        };
        let (name, arguments) = directive
            .split_once(char::is_whitespace)
            .map(|(name, rest)| (name, rest.trim()))
            .unwrap_or((directive, ""));
        let arguments = if arguments.is_empty() {
            serde_json::json!({})
        } else {
            match serde_json::from_str(arguments) {
                Ok(arguments) => arguments,
                Err(err) => {
                    return Self::answer(format!(
                        "(mock model) the arguments of `!tool {name}` are not JSON: {err}"
                    ));
                }
            }
        };
        // The model sees plugin tools as `<plugin>__<tool>`; the developer may type either.
        let suffix = format!("__{name}");
        let offered: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        let matches: Vec<&str> = match offered.iter().find(|tool| **tool == name) {
            Some(exact) => vec![*exact],
            None => offered
                .iter()
                .copied()
                .filter(|tool| tool.ends_with(&suffix))
                .collect(),
        };
        let [tool] = matches.as_slice() else {
            let problem = if matches.is_empty() {
                format!("no tool named '{name}' is offered")
            } else {
                format!("'{name}' is ambiguous")
            };
            return Self::answer(format!(
                "(mock model) {problem}; offered tools: {}",
                if offered.is_empty() {
                    "none".to_string()
                } else {
                    offered.join(", ")
                }
            ));
        };
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        ChatResponse {
            tool_calls: vec![ToolCall {
                id: format!("mock-call-{call}"),
                name: tool.to_string(),
                arguments,
            }],
            finish_reason: Some("tool_calls".to_string()),
            ..ChatResponse::default()
        }
    }
}

#[async_trait]
impl LlmProvider for MockModel {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        *self
            .last_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
        let Some(last) = request.messages.last() else {
            return Ok(Self::answer(
                "(mock model) the request had no messages".to_string(),
            ));
        };
        let text = last.content.as_deref().unwrap_or_default();
        Ok(match last.role {
            Role::Tool => Self::answer(format!("(mock model) the tool returned: {text}")),
            Role::User => self.answer_user(text, request),
            _ => Self::answer(format!("(mock model) received:\n{text}")),
        })
    }
}

/// The running sandbox node and the plugin it serves.
struct Sandbox {
    supervisor: Arc<Supervisor>,
    engine: Arc<PipelineEngine>,
    model: Arc<MockModel>,
    host: Arc<ManagedHost>,
    worker: JoinHandle<()>,
    dispatcher: Option<JoinHandle<()>>,
    server: JoinHandle<Result<(), String>>,
    server_shutdown: oneshot::Sender<()>,
    next_event: u64,
    /// Holds sockets, databases and plugin data; removed when the sandbox is dropped.
    _dir: TempDir,
}

impl Sandbox {
    /// Assembles the node as `kanon` does and starts the plugin from `manifest_path`.
    async fn start(manifest_path: &Path) -> Result<Self, SandboxError> {
        let dir = tempdir()?;
        let root = dir.path().to_path_buf();
        let core_sock = root.join("core.sock");
        let setup =
            |what: &str, err: &dyn std::fmt::Display| SandboxError::Setup(format!("{what}: {err}"));

        // Dependencies are installed with the plugin's own tool, exactly as the node does, so a
        // plugin that runs here runs there.
        let supervisor = Arc::new(
            Supervisor::new(Some(root.join("run")), Some(core_sock.clone()))
                .with_dependency_installer(Some(kanon_core::DependencyInstaller::new())),
        );
        let adapter: Arc<dyn PlatformAdapter> = Arc::new(SandboxAdapter::default());
        supervisor
            .adapters()
            .register(adapter)
            .await
            .map_err(|err| setup("sandbox adapter", &err))?;

        // Conversations are stored as on a node, so `/ls`, `/switch`, `/del` and the plugin
        // conversation APIs behave the same.
        let db = root.join("sessions.db");
        let memory = SqliteMemory::open(&db).map_err(|err| setup("session memory", &err))?;
        let store = SqliteSessionStore::open(&db).map_err(|err| setup("session store", &err))?;
        let sessions = Arc::new(
            SessionManager::new(Arc::new(memory))
                .with_store(Arc::new(store))
                .map_err(|err| setup("sessions", &err))?,
        );
        let persona_store = Arc::new(PersonaStore::new(root.join("personas.json")));
        let personas = Arc::new(
            persona_store
                .load_registry()
                .map_err(|err| setup("personas", &err))?,
        );

        // One instance answers every sandbox message, with the mock as its model.
        let instances = Arc::new(
            InstanceRegistry::open(root.join("instances.json"))
                .await
                .map_err(|err| setup("instance catalog", &err))?,
        );
        instances
            .create(
                InstanceDraft {
                    name: "Sandbox".to_string(),
                    enabled: true,
                    adapters: vec![PLATFORM.to_string()],
                    ..InstanceDraft::default()
                },
                Some((&personas, &sessions, &kanon_llm::ProviderRegistry::new())),
            )
            .await
            .map_err(|err| setup("sandbox instance", &err))?;
        let model = Arc::new(MockModel::default());
        let factory = Arc::new(AgentFactory::new(
            "sandbox",
            Arc::new(AgentSlot::new()),
            sessions.memory().clone(),
            sessions,
            personas.clone(),
            vec![Arc::new(PluginAgentHook::new())],
            Vec::new(),
        ));
        factory.install(
            "sandbox",
            model.clone(),
            AgentConfig {
                default_model: MOCK_MODEL.to_string(),
                provider: Some(MOCK_PROVIDER.to_string()),
                ..AgentConfig::default()
            },
        );

        let policy = CommandPolicy {
            admins: vec![format!("{PLATFORM}:{SENDER}")],
            ..CommandPolicy::default()
        };
        let engine = Arc::new(
            PipelineEngine::new(supervisor.clone())
                .with_agent_factory(factory.clone())
                .with_instances(instances)
                .with_command_policy(Arc::new(CommandPolicyStore::new(policy)))
                .with_dead_letter(Arc::new(DeadLetterWriter::new(root.join("dead_letter")))),
        );
        // The worker only serves events plugins ingest themselves (adapter plugins); the
        // developer's messages are processed directly so each is answered before the next.
        let (event_tx, event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
        let worker = engine.clone().start_worker(event_rx);
        let dispatcher = engine.clone().start_outbound_dispatcher();

        let kv = kanon_storage::KvStore::open_in_memory().map_err(|err| setup("KV store", &err))?;
        let service = CoreApiService::new(event_tx)
            .with_supervisor(supervisor.clone())
            .with_outbound_sender(engine.outbound_sender())
            .with_engine(engine.clone())
            .with_agent_slot(factory.slot().clone())
            .with_personas(personas, persona_store)
            .with_kv(Arc::new(kv))
            .with_plugin_data_dir(kanon_storage::PluginDataDir::new(root.join("plugins")));
        let core_server = CoreIpcServer::new(&core_sock, service);
        let (server_shutdown, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            core_server
                .run(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .map_err(|error| error.to_string())
        });
        // The host connects back to the core socket during its handshake.
        for _ in 0..50 {
            if core_sock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // What runs is always the code as it is now: a Rust plugin is rebuilt first.
        let manifest = kanon_core::PluginManifest::load_from_file(manifest_path)
            .map_err(|err| setup("plugin.toml", &err))?;
        let plugin_root = manifest_path.parent().unwrap_or(Path::new("."));
        if manifest.plugin.runtime == "rust" {
            println!("Building with cargo...");
        }
        let executable = build_plugin(plugin_root, &manifest).await?;

        println!("Spawning plugin host process and completing handshake...");
        let host = supervisor
            .spawn_from_manifest(manifest_path, executable.as_deref())
            .await?;
        Ok(Self {
            supervisor,
            engine,
            model,
            host,
            worker,
            dispatcher,
            server,
            server_shutdown,
            next_event: 0,
            _dir: dir,
        })
    }

    /// Sends one message as the developer and waits until every reply it caused is printed.
    async fn send(&mut self, text: &str) -> PipelineResult {
        self.next_event += 1;
        let event = PipelineEventRequest {
            event_id: format!("sandbox-event-{}", self.next_event),
            platform: PLATFORM.to_string(),
            channel_id: CHANNEL.to_string(),
            sender_id: SENDER.to_string(),
            raw_text: text.to_string(),
            segments: vec![MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: text.to_string(),
                })),
            }],
            metadata: None,
        };
        let event_id = event.event_id.clone();
        let result = self.engine.process_event(event).await;
        match &result {
            PipelineResult::CommandNotFound { command } => {
                println!("[sandbox] no plugin declares /{command}")
            }
            PipelineResult::CommandExecuted { success: false, .. } => {
                println!("[sandbox] the command reported a failure")
            }
            PipelineResult::LlmFailed { error, .. } => {
                println!("[sandbox] model turn failed: {error}")
            }
            PipelineResult::ReplySuppressed { reason, .. } => {
                println!("[sandbox] reply suppressed: {reason}")
            }
            PipelineResult::Passed(_) => println!("[sandbox] passed without a reply"),
            _ => {}
        }

        // Replies go through the outbound queue like the node's, behind anything the plugin
        // sent while handling the message. The platform's queue is delivered in order, so once
        // this delivery is acknowledged everything before it has been printed; with no replies
        // an empty delivery serves as that marker.
        let (receipt, delivered) = oneshot::channel();
        let message = OutboundMessage {
            request: DeliverMessageRequest {
                platform: PLATFORM.to_string(),
                channel_id: CHANNEL.to_string(),
                recipient_id: SENDER.to_string(),
                segments: result.replies().to_vec(),
                event_id,
            },
            split_lines: matches!(
                result,
                PipelineResult::LlmReplied {
                    split_lines: true,
                    ..
                }
            ),
            receipt: Some(receipt),
        };
        if self.engine.outbound_sender().send(message).await.is_err() {
            println!("[sandbox] the outbound queue is closed; replies were not delivered");
        } else if delivered.await.is_err() {
            println!("[sandbox] the reply was dropped before delivery");
        }
        result
    }

    /// Prints the system prompt and tools of the last model request.
    fn print_last_prompt(&self) {
        let Some(request) = self.model.last_request() else {
            println!("The model has not been asked anything yet; send a message first.");
            return;
        };
        let system = request
            .messages
            .iter()
            .find(|message| message.role == Role::System)
            .and_then(|message| message.content.as_deref())
            .unwrap_or("(none)");
        println!("--- system prompt ---\n{system}");
        let tools: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        println!(
            "--- tools ({}) ---\n{}",
            tools.len(),
            if tools.is_empty() {
                "(none)".to_string()
            } else {
                tools.join("\n")
            }
        );
        println!(
            "--- {} message(s) of history and the current turn ---",
            request
                .messages
                .iter()
                .filter(|message| message.role != Role::System)
                .count()
        );
    }

    /// Stops in the node's order: deliver what is queued, then stop the plugin, then the core.
    async fn shutdown(self) {
        self.engine.drain(self.worker, self.dispatcher).await;
        if let Err(err) = self.supervisor.stop_all().await {
            println!("[sandbox] stopping the plugin host failed: {err}");
        }
        let _ = self.server_shutdown.send(());
        if let Err(error) = kanon_core::shutdown::finish_server("Sandbox core", self.server).await {
            println!("[sandbox] {error}");
        }
    }
}

/// Prints what the loaded plugins declare.
fn print_plugin_summary(host: &ManagedHost) {
    println!("\n[Host Connected] ID: {}", host.host_id);
    for meta in host.metas() {
        println!(
            "Loaded Plugin: {} ({}) v{}",
            meta.name, meta.id, meta.version
        );
        if !meta.commands.is_empty() {
            println!("Commands:");
            for cmd in &meta.commands {
                let usage = if cmd.usage.is_empty() {
                    format!("/{}", cmd.name)
                } else {
                    cmd.usage.clone()
                };
                println!("  {usage:<24} {}", cmd.description);
                for sub in &cmd.subcommands {
                    let usage = if sub.usage.is_empty() {
                        format!("/{} {}", cmd.name, sub.name)
                    } else {
                        sub.usage.clone()
                    };
                    println!("    {usage:<22} {}", sub.description);
                }
                if !cmd.platforms.is_empty() && !cmd.platforms.iter().any(|p| p == PLATFORM) {
                    println!(
                        "    (only on {}; the sandbox will not route it)",
                        cmd.platforms.join(", ")
                    );
                }
            }
        }
        if !meta.triggers.is_empty() {
            println!("Triggers:");
            for trigger in &meta.triggers {
                println!("  {:<24} {}", trigger.name, trigger.pattern);
            }
        }
        if !meta.tools.is_empty() {
            println!("Tools:");
            for tool in &meta.tools {
                println!("  {:<24} {}", tool.name, tool.description);
            }
        }
        let mut hooks = Vec::new();
        if meta.rewrites_system_prompt {
            hooks.push("system prompt rewrite");
        }
        if meta.prepares_turns {
            hooks.push("turn preparation");
        }
        if meta.decorates_replies {
            hooks.push("reply decoration");
        }
        if meta.serves_http {
            hooks.push("HTTP routes (not served by the sandbox)");
        }
        if !hooks.is_empty() {
            println!("Hooks: {}", hooks.join(", "));
        }
    }
    println!("============================================================\n");
}

/// Prints the REPL's usage.
fn print_repl_help() {
    println!("Type messages as you would in a chat; the mock model answers anything that is not");
    println!("a command or trigger. Sandbox commands start with ':'.");
    println!(
        "  <text>                 Send a message (\"/cmd args\" runs a command, \"/help\" lists them)"
    );
    println!("  !tool <name> [json]    Inside a message: make the mock model call that tool");
    println!("  :call <tool> [json]    Call a tool directly, without the model");
    println!("  :prompt                Show the system prompt and tools of the last model request");
    println!("  :help                  Show this help");
    println!("  :quit                  Shut down the sandbox (also Ctrl-D)\n");
}

/// Runs the offline sandbox for the plugin at `path`.
pub async fn run_sandbox(path: &Path, opts: SandboxOptions) -> Result<(), SandboxError> {
    let (manifest_path, _root) = find_manifest_path(path)?;

    println!("============================================================");
    println!(" Kanon Offline Sandbox");
    println!(" Manifest: {}", manifest_path.display());
    println!(
        " Model:    {MOCK_PROVIDER}/{MOCK_MODEL} (no network; echoes, or calls `!tool <name> [json]`)"
    );
    println!("============================================================");

    let mut sandbox = Sandbox::start(&manifest_path).await?;
    print_plugin_summary(&sandbox.host);
    let outcome = drive(&mut sandbox, &opts).await;
    sandbox.shutdown().await;
    println!("Sandbox terminated.");
    outcome
}

/// Does what the options ask for: a command, a tool call, scripted messages, or the REPL.
async fn drive(sandbox: &mut Sandbox, opts: &SandboxOptions) -> Result<(), SandboxError> {
    if let Some(command) = &opts.command {
        return run_command(sandbox, command, &opts.args).await;
    }
    if let Some(tool) = &opts.tool {
        return call_tool(&sandbox.host, tool, &opts.args.join(" ")).await;
    }
    for message in &opts.messages {
        print_labeled("you>", message);
        sandbox.send(message).await;
    }
    if opts.non_interactive || !opts.messages.is_empty() {
        return Ok(());
    }
    repl(sandbox).await
}

/// Runs `/command args` through the pipeline, failing unless a plugin answered it successfully.
async fn run_command(
    sandbox: &mut Sandbox,
    command: &str,
    args: &[String],
) -> Result<(), SandboxError> {
    let name = command.trim_start_matches('/');
    let text = if args.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {}", args.join(" "))
    };
    print_labeled("you>", &text);
    match sandbox.send(&text).await {
        PipelineResult::CommandExecuted { success: true, .. }
        | PipelineResult::BuiltinReplied { .. }
        | PipelineResult::SessionRotated { .. }
        | PipelineResult::ModelListed { .. }
        | PipelineResult::ModelSelected { .. } => Ok(()),
        PipelineResult::CommandNotFound { .. } => {
            Err(SandboxError::CommandNotFound(name.to_string()))
        }
        _ => Err(SandboxError::CommandFailed(name.to_string())),
    }
}

/// The interactive terminal loop.
async fn repl(sandbox: &mut Sandbox) -> Result<(), SandboxError> {
    print_repl_help();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("you> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let Some(line) = lines.next_line().await? else {
            // EOF: Ctrl-D, or the end of piped input.
            println!();
            return Ok(());
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        let Some(sandbox_command) = input.strip_prefix(':') else {
            sandbox.send(input).await;
            continue;
        };
        let (name, rest) = sandbox_command
            .split_once(char::is_whitespace)
            .map(|(name, rest)| (name, rest.trim()))
            .unwrap_or((sandbox_command, ""));
        match name {
            "quit" | "q" | "exit" => return Ok(()),
            "help" => print_repl_help(),
            "prompt" => sandbox.print_last_prompt(),
            "call" => {
                let (tool, args) = rest
                    .split_once(char::is_whitespace)
                    .map(|(tool, args)| (tool, args.trim()))
                    .unwrap_or((rest, ""));
                if tool.is_empty() {
                    println!("Usage: :call <tool> [json]");
                } else if let Err(err) = call_tool(&sandbox.host, tool, args).await {
                    println!("Error: {err}");
                }
            }
            other => println!("Unknown sandbox command ':{other}'; type :help."),
        }
    }
}

/// The event a direct tool call is attributed to: the developer in the sandbox chat.
fn direct_call_context() -> PipelineEventRequest {
    PipelineEventRequest {
        event_id: "sandbox-direct-call".to_string(),
        platform: PLATFORM.to_string(),
        channel_id: CHANNEL.to_string(),
        sender_id: SENDER.to_string(),
        ..PipelineEventRequest::default()
    }
}

/// Calls a declared tool directly, without the model, and prints its result.
async fn call_tool(
    host: &ManagedHost,
    tool_name: &str,
    json_args: &str,
) -> Result<(), SandboxError> {
    let declared = host
        .metas()
        .iter()
        .any(|meta| meta.tools.iter().any(|tool| tool.name == tool_name));
    if !declared {
        return Err(SandboxError::ToolNotFound(tool_name.to_string()));
    }

    let parsed_json: serde_json::Value = if json_args.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(json_args)?
    };
    let req = ToolCallRequest {
        call_id: "sandbox-direct-call".to_string(),
        tool_name: tool_name.to_string(),
        session_id: DIRECT_CALL_SESSION.to_string(),
        payload: kanon_llm::tool_router::json_to_prost_struct(&parsed_json)
            .map(tool_call_request::Payload::StructuredArgs),
        context: Some(direct_call_context()),
    };

    println!("[Invoking Tool] '{tool_name}' with payload: {parsed_json}");
    let start = std::time::Instant::now();
    let resp: ToolCallResponse = host.on_call_tool(req).await?;
    let elapsed = start.elapsed();

    println!(
        "[Response] (Status: {}, RTT: {:.2}ms)",
        if resp.success { "SUCCESS" } else { "FAILED" },
        elapsed.as_secs_f64() * 1000.0
    );
    match &resp.payload {
        Some(tool_call_response::Payload::StructuredResult(s)) => {
            let json_val = kanon_llm::tool_router::prost_struct_to_json(s.clone())
                .map_err(|error| tonic::Status::data_loss(error.to_string()))?;
            println!(
                "Result: {}",
                serde_json::to_string_pretty(&json_val).unwrap_or_default()
            );
        }
        Some(tool_call_response::Payload::RawBytes(bytes)) => {
            println!(
                "Raw bytes (len {}): {}",
                bytes.len(),
                String::from_utf8_lossy(bytes)
            );
        }
        None => {}
    }
    if !resp.error_message.is_empty() {
        println!("Error: {}", resp.error_message);
    }
    println!();
    if resp.success {
        Ok(())
    } else {
        Err(SandboxError::ToolFailed(tool_name.to_string()))
    }
}
