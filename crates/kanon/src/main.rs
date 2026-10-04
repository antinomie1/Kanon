//! Kanon node entrypoint: the project's single node binary.
//!
//! This crate owns *assembly only*, and it is the only place in the workspace that turns the
//! libraries into a running node. The microkernel ([`kanon_core`]), the management gateway
//! ([`kanon_api`]) and the platform adapters (Milky, OneBot v11, QQ Official) are libraries with no
//! entrypoints of their own; `kanon-dev` is the separate developer CLI and never runs a node.
//!
//! As the composition root it starts the core IPC server (`core.sock`), the pipeline worker, the
//! process supervisor and the Axum management gateway in one process, wiring the observability hub
//! into both the `tracing` pipeline and the lifecycle trace bus.
//!
//! # Configuration
//! The node reads no environment variables: all of its configuration lives in
//! `data/system.json`. The `startup` section holds what is needed before anything is served and
//! is edited by hand (the console never writes it); every field is optional:
//!
//! ```json
//! {
//!   "startup": {
//!     "api_addr": "127.0.0.1:8080",
//!     "log": "info",
//!     "run_dir": "/run/kanon",
//!     "typescript_runtime": "/usr/bin/node",
//!     "install_dependencies": true
//!   }
//! }
//! ```
//!
//! Model routing lives in the same document: a named provider directory plus a per-model settings
//! catalog, both editable through the console. A model is addressed as `<provider>/<model-id>`,
//! and exactly one of them is the node's global default model. The document also carries the
//! node-wide reply and context policies and the OneBot, Milky and QQ Official adapter sections.
//!
//! Conversations are durable: history, compaction summaries and session records live in
//! `data/sessions.db`, and the operator's personas in `data/personas.json`. Plugins keep small
//! state in the central key-value store `data/kv.db`. All are opened before anything is served,
//! and a file that cannot be read stops startup instead of being replaced by an empty one.

use std::sync::Arc;

use kanon_adapter_milky::MilkyAdapter;
use kanon_adapter_onebot::OneBotAdapter;
use kanon_adapter_qqofficial::QqOfficialAdapter;
use kanon_api::{
    ApiServer, ApiState, DEFAULT_SESSION_DB, NodeSettings, Observability, StartupConfig,
    SystemConfigStore, open_session_manager,
};
use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    BashPolicyStore, BashTool, CommandPolicyStore, DEFAULT_INSTANCE_CATALOG, DEFAULT_MCP_CONFIG,
    DEFAULT_SKILLS_DIR, DEFAULT_TOGGLE_STATE, EventIngress, HOST_WATCHDOG_INTERVAL,
    InstanceRegistry, MCP_WATCHDOG_INTERVAL, McpConfigStore, McpPool, PLUGIN_SECTION,
    PluginAgentHook, ReadSkillTool, SkillCatalogHook, SkillStore, ToggleStore,
    restore_instance_personas,
};
use kanon_llm::PersonaStore;
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Fallible startup result type shared by the binary entrypoint helpers.
type StartupResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::main]
async fn main() -> StartupResult<()> {
    // Read before anything else: the log filter and the socket directory come from here.
    let startup = load_startup()?;
    let observability = Arc::new(Observability::new());
    init_tracing(observability.clone(), &startup.log)?;

    // --- Core microkernel & supervisor ------------------------------------------------
    let (event_tx, event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let ingress = EventIngress::new(event_tx);
    let ipc_token = kanon_transport::generate_ipc_token()?;
    // The supervisor owns the socket layout: `core.sock` lives in the run directory it resolves.
    let supervisor = Arc::new(
        Supervisor::new(startup.run_dir.clone(), None)
            .with_ipc_token(ipc_token.clone())
            .with_typescript_runtime(startup.typescript_runtime.clone())
            .with_dependency_installer(
                startup
                    .install_dependencies
                    .then(kanon_core::DependencyInstaller::new),
            ),
    );
    let socket_path = supervisor.core_sock_path().to_path_buf();

    // --- Bot instances ----------------------------------------------------------------
    // Instances decide whether inbound platform traffic is answered at all: with no enabled
    // instance claiming a platform the pipeline drops the event instead of feeding it to a model.
    let instances = Arc::new(
        InstanceRegistry::open(DEFAULT_INSTANCE_CATALOG)
            .await
            .map_err(|err| format!("Failed to load the bot instance catalog: {err}"))?,
    );
    let instance_count = instances.len().await;
    if instance_count == 0 {
        tracing::warn!(
            "No bot instance is configured: platform messages will be dropped until an instance \
             is created and enabled in the console"
        );
    } else {
        tracing::info!(count = instance_count, "Bot instance catalog loaded");
    }

    // --- Plugin enable/disable state --------------------------------------------------
    // Toggling a plugin must not rewrite files inside the user's plugin directory, so the state
    // lives beside the node's other settings and is applied at startup and on every toggle.
    let plugin_state = Arc::new(
        ToggleStore::open(DEFAULT_TOGGLE_STATE)
            .await
            .map_err(|err| format!("Failed to load the plugin state store: {err}"))?,
    );
    let disabled_plugins = plugin_state.disabled_ids(PLUGIN_SECTION).await;
    if !disabled_plugins.is_empty() {
        tracing::info!(plugins = ?disabled_plugins, "Plugins disabled by the operator");
    }

    // --- MCP servers ------------------------------------------------------------------
    // MCP tools are offered through exactly the same router as plugin tools, so the pool is
    // created here and shared with both the console and the pipeline worker.
    let mcp_config = Arc::new(
        McpConfigStore::open(DEFAULT_MCP_CONFIG)
            .await
            .map_err(|err| format!("Failed to load the MCP configuration: {err}"))?,
    );
    // Attachments written by earlier runs are swept here: they only need to outlive the delivery
    // attempt that follows their tool call, and leaving them would grow the data directory forever.
    match kanon_core::prune_attachments(
        std::path::Path::new(kanon_core::DEFAULT_ATTACHMENT_DIR),
        kanon_core::ATTACHMENT_RETENTION,
    ) {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "Swept stale tool attachments"),
        Err(err) => tracing::warn!(error = %err, "Failed to sweep stale tool attachments"),
    }

    let mcp_pool = Arc::new(McpPool::new());
    mcp_pool.sync_from_config(&mcp_config).await;
    let mcp_server_count = mcp_pool.describe().await.len();
    if mcp_server_count > 0 {
        tracing::info!(count = mcp_server_count, "MCP servers configured");
    }

    // --- Skills -----------------------------------------------------------------------
    // Skills are plain directories on disk; the store only reads them, and the catalog hook plus
    // the `read_skill` tool enforce the node-wide and per-instance switches at call time.
    let skills = Arc::new(SkillStore::new(DEFAULT_SKILLS_DIR));
    match skills.list() {
        Ok(installed) if !installed.is_empty() => {
            tracing::info!(count = installed.len(), "Skills installed");
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "Failed to enumerate the skills directory"),
    }

    // --- Built-in platform adapters --------------------------------------------------
    // Built before the gateway state because the console manages this very instance: it holds the
    // concrete adapter so configuration changes reach the object the registry routes to. The
    // adapter is registered here and *started* later, when `start_all` hands every adapter the
    // core's ingest queue; until then a configured adapter reports `connecting` and opens no
    // connection it could not feed.
    let milky_adapter = register_milky_adapter(&supervisor).await?;
    let onebot_adapter = register_onebot_adapter(&supervisor).await?;
    let qqofficial_adapter = register_qqofficial_adapter(&supervisor).await?;

    // --- Management gateway state & agent engine --------------------------------------
    // The state owns one agent factory (and the named provider directory inside it), shared with
    // the pipeline worker and the IPC service, so a provider configured later through the console
    // is observed by all three without a restart.
    //
    // The builder validates and applies persisted settings through the same path used by console
    // updates, keeping startup and runtime model routing consistent.
    let node_settings = bootstrap_node_settings()?;
    // Bash enforces the command policy's administrator list — the serving instance's own, or the
    // node's; the API state publishes console edits into these same stores, and the pipeline
    // shares the command policy and the instance catalog through the state.
    let bash_tool = Arc::new(BashTool::new(
        kanon_core::DEFAULT_BASH_WORKSPACE,
        Arc::new(BashPolicyStore::new(node_settings.bash_policy.clone())),
        Arc::new(CommandPolicyStore::new(
            node_settings.command_policy.clone(),
        )),
        instances.clone(),
    )?);

    // The persona library is the built-in base assistant plus the operator's saved personas. A
    // malformed `data/personas.json` is a hard startup error rather than a silent fallback, for the
    // same reason as the system document: starting without the operator's personas would change how
    // the bot answers without anyone noticing.
    let persona_store = Arc::new(PersonaStore::default());
    let personas = Arc::new(persona_store.load_registry().map_err(|err| {
        format!(
            "Failed to load personas from {}: {err}",
            persona_store.path().display()
        )
    })?);
    tracing::info!(
        count = personas.len().saturating_sub(1),
        "Operator-defined personas loaded"
    );

    // Conversations survive a restart: history and summaries, persona bindings and counters are
    // stored in `data/sessions.db`, and an unreadable database stops startup instead of silently
    // starting from an empty session list.
    let sessions = open_session_manager(DEFAULT_SESSION_DB)?;

    // The plugins' central key-value store. Like the session database, a file that cannot be
    // opened stops startup: plugins would otherwise run on, silently losing what they stored.
    let kv = Arc::new(
        kanon_storage::KvStore::open(kanon_storage::DEFAULT_KV_FILE).map_err(|err| {
            format!(
                "Failed to open the key-value store {}: {err}",
                kanon_storage::DEFAULT_KV_FILE
            )
        })?,
    );

    let state = ApiState::builder(supervisor.clone())
        .with_startup(startup.clone())
        .with_sessions(sessions)
        .with_personas(personas)
        .with_persona_store(persona_store.clone())
        .with_milky_adapter(milky_adapter)
        .with_onebot_adapter(onebot_adapter)
        .with_qqofficial_adapter(qqofficial_adapter)
        .with_observability(observability.clone())
        .with_ingress(ingress.clone())
        .with_instances(instances.clone())
        .with_plugin_state(plugin_state.clone())
        .with_mcp_pool(mcp_pool.clone())
        .with_mcp_config(mcp_config.clone())
        .with_skill_store(skills.clone())
        .with_node_settings(node_settings)
        .with_native_tools(vec![Arc::new(ReadSkillTool::new(
            skills.clone(),
            plugin_state.clone(),
            instances.clone(),
        ))])
        // The plugin hook comes after the skill catalog: plugins rewrite the system prompt as the
        // operator's persona and skills built it.
        .with_hooks(vec![
            Arc::new(SkillCatalogHook::new(
                skills.clone(),
                plugin_state.clone(),
                instances.clone(),
            )),
            Arc::new(PluginAgentHook::new()),
        ])
        .with_bash_tool(bash_tool)
        .build();

    // Publish instance prompts as personas before the first message can arrive.
    restore_instance_personas(&instances.list().await, state.personas(), state.sessions())
        .map_err(|err| {
            format!("Failed to restore instance personas and session bindings: {err}")
        })?;

    match state.agent() {
        Some(agent) => tracing::info!(
            model = %agent.config().model_ref(),
            provider = ?agent.config().provider,
            temperature = ?agent.config().temperature,
            max_tokens = ?agent.config().max_tokens,
            "LLM provider configured for conversational pipeline and sandbox chat"
        ),
        None => tracing::warn!(
            "No default model is configured in data/system.json; \
             chat completions and conversational LLM routing are disabled"
        ),
    }

    // Fill the model catalog from each endpoint that has none yet. Spawned rather than awaited so a
    // slow or unreachable endpoint can never delay the node coming up; the catalog only holds
    // metadata, so arriving a few seconds late is harmless.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let written = state.autofill_model_catalog().await;
            if written > 0 {
                tracing::info!(
                    count = written,
                    "Model catalog populated from the configured endpoints"
                );
            }
        });
    }

    // The pipeline never awaits platform I/O: replies are queued and an independent dispatcher
    // resolves the destination platform to a built-in adapter or a plugin host.
    let engine = Arc::new(
        PipelineEngine::new(supervisor.clone())
            .with_observer(observability.events.clone())
            .with_agent_factory(state.agent_factory().clone())
            .with_instances(instances.clone())
            .with_toggles(plugin_state.clone())
            .with_reply_policy(state.reply_policy().clone())
            .with_context_policy(state.context_policy().clone())
            .with_event_policy(state.event_policy().clone())
            .with_command_policy(state.command_policy().clone())
            .with_mcp_pool(mcp_pool.clone()),
    );
    let pipeline_worker = engine.clone().start_worker(event_rx);
    let outbound_dispatcher = engine.clone().start_outbound_dispatcher();

    let service = CoreApiService::new(ingress.clone())
        .with_supervisor(supervisor.clone())
        .with_outbound_sender(engine.outbound_sender())
        .with_agent_slot(state.llm_slot().clone())
        .with_engine(engine.clone())
        .with_personas(state.personas().clone(), persona_store)
        .with_kv(kv);
    let ipc_server = CoreIpcServer::new(socket_path, service).with_auth_token(ipc_token);
    // Binding must succeed before any plugin starts; an existing path is not evidence that our
    // server owns it, and spawning first would hide listener failures inside a background task.
    let ipc_listener = ipc_server.bind_listener()?;

    // Bind the management endpoint before launching plugins: address or access-configuration
    // errors must fail startup before any child process begins serving traffic.
    let api_server = ApiServer::bind(startup.api_addr, state.clone()).await?;
    let bound_addr = api_server.local_addr();

    // --- Graceful shutdown channels ---------------------------------------------------
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let (api_shutdown_tx, api_shutdown_rx) = oneshot::channel();

    // Start Core IPC server FIRST so that spawned plugin hosts can connect to core.sock immediately
    let core_task = tokio::spawn(async move {
        ipc_server
            .run_with_listener(ipc_listener, async move {
                let _ = core_shutdown_rx.await;
            })
            .await
    });

    // Ensure ./plugins and ./data/plugins directories exist
    if let Err(err) = std::fs::create_dir_all("./plugins") {
        tracing::warn!(error = %err, "Failed to ensure ./plugins directory exists");
    }
    if let Err(err) = std::fs::create_dir_all("./data/plugins") {
        tracing::warn!(error = %err, "Failed to ensure ./data/plugins directory exists");
    }

    // Scan ./plugins once and launch what it declares, skipping any the operator disabled. This is
    // the only automatic scan: afterwards the directory is read again only when the console asks
    // for a rescan, and the gateway's catalog serves this same snapshot until then.
    let plugins = match state.rescan_plugins() {
        Ok(plugins) => plugins,
        Err(err) => {
            tracing::error!(
                dir = %state.plugins_dir().display(),
                error = %err,
                "Failed to scan plugins directory"
            );
            Vec::new()
        }
    };
    // Dependency installation belongs to optional plugins, not node readiness. Keep the task
    // owned by main so shutdown can cancel an install or handshake before stopping live hosts.
    let plugin_startup = {
        let supervisor = supervisor.clone();
        let plugin_state = plugin_state.clone();
        tokio::spawn(async move {
            launch_plugins(&supervisor, plugins, &plugin_state).await;
        })
    };

    // A crashed host is otherwise invisible: the supervisor would keep advertising a dead process
    // as healthy and route events into a closed socket. The watchdog prunes and restarts it.
    let host_watchdog =
        supervisor.spawn_host_watchdog(plugin_state.clone(), HOST_WATCHDOG_INTERVAL);
    let mcp_watchdog = mcp_pool.spawn_watchdog(
        mcp_config.clone(),
        plugin_state.clone(),
        MCP_WATCHDOG_INTERVAL,
    );
    tracing::info!(
        interval_secs = HOST_WATCHDOG_INTERVAL.as_secs(),
        "Plugin host watchdog started"
    );

    // --- Platform adapters ------------------------------------------------------------
    for (platform, error) in supervisor.adapters().start_all(ingress.clone()).await {
        tracing::error!(platform = %platform, error = %error, "Adapter failed to start");
    }

    let api_task = tokio::spawn(async move {
        api_server
            .run(async move {
                let _ = api_shutdown_rx.await;
            })
            .await
    });

    tracing::info!(address = %bound_addr, "Kanon node started");

    // SIGINT or SIGTERM: both must run the shutdown path below, which stops every plugin host.
    // Exiting without it leaves hosts alive with their platform connections open, and the next
    // start would serve each message twice.
    kanon_core::shutdown_signal().await;

    // No launcher or watchdog may publish a new host after the shutdown sweep starts. Awaiting
    // cancellation drops any in-flight child handles, whose kill_on_drop stops unfinished work.
    plugin_startup.abort();
    if let Err(err) = plugin_startup.await
        && !err.is_cancelled()
    {
        tracing::error!(error = %err, "Plugin startup task failed");
    }
    host_watchdog.abort();
    mcp_watchdog.abort();
    for task in [host_watchdog, mcp_watchdog] {
        if let Err(err) = task.await
            && !err.is_cancelled()
        {
            tracing::error!(error = %err, "Watchdog task failed");
        }
    }

    // Order matters. The console stops first. The pipeline then drains: ingress closes, queued
    // events go to the dead-letter log, and the event in progress and the queued replies get a
    // bounded grace. Only after that do the IPC server, adapters and hosts stop, because the
    // final deliveries (and plugin callbacks made while finishing the last event) still use them.
    let _ = api_shutdown_tx.send(());
    engine.drain(pipeline_worker, outbound_dispatcher).await;
    let _ = core_shutdown_tx.send(());
    let (core_result, api_result) = tokio::join!(
        kanon_core::shutdown::finish_server("Core IPC server", core_task),
        kanon_core::shutdown::finish_server("Management gateway", api_task),
    );
    for error in [&core_result, &api_result]
        .into_iter()
        .filter_map(|result| result.as_ref().err())
    {
        tracing::error!(%error, "Server shutdown failed");
    }

    for (platform, error) in supervisor.adapters().stop_all().await {
        tracing::warn!(platform = %platform, error = %error, "Adapter failed to stop cleanly");
    }
    supervisor.stop_all().await?;

    // A server failure must not skip adapter/host cleanup, but still makes process failure visible.
    core_result?;
    api_result?;

    tracing::info!("Kanon node shut down gracefully");
    Ok(())
}

/// Loads the node's `startup` settings from `data/system.json`.
fn load_startup() -> StartupResult<StartupConfig> {
    let store = SystemConfigStore::default();
    store.load_startup().map_err(|err| {
        format!(
            "Failed to load the startup settings from {}: {err}",
            store.path().display()
        )
        .into()
    })
}

/// Loads the model-routing settings saved in `data/system.json`.
///
/// A malformed document is a hard startup error rather than a silent fallback, because running
/// without the operator's chosen provider is exactly the surprise that must not happen.
fn bootstrap_node_settings() -> StartupResult<NodeSettings> {
    let store = SystemConfigStore::default();
    let settings = store.load_node_settings().map_err(|err| {
        format!(
            "Failed to load node system configuration at {}: {err}",
            store.path().display()
        )
    })?;
    tracing::info!(
        providers = ?settings.providers.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
        default_model = ?settings.default_model,
        "Model provider directory loaded from data/system.json"
    );
    Ok(settings)
}

/// Builds and registers the Milky platform adapter from its `data/system.json` section.
///
/// Without a saved section the adapter is registered disabled, ready to be configured in the
/// console. A malformed section is a hard error: that file is the node's own state, and silently
/// ignoring it would start a node that does not do what its configuration says.
async fn register_milky_adapter(supervisor: &Arc<Supervisor>) -> StartupResult<Arc<MilkyAdapter>> {
    let store = SystemConfigStore::default();
    let persisted = store.load_milky().map_err(|err| {
        format!(
            "Failed to load the Milky adapter configuration from {}: {err}",
            store.path().display()
        )
    })?;

    let adapter = Arc::new(MilkyAdapter::new(persisted.unwrap_or_default())?);
    supervisor.adapters().register(adapter.clone()).await?;

    let status = adapter.status();
    if status.enabled {
        tracing::info!(
            platform = %status.platform,
            base_url = %status.base_url,
            transport = %status.transport,
            "Milky adapter registered"
        );
    } else {
        tracing::info!(
            platform = %status.platform,
            "Milky adapter registered but disabled; configure it in the console under Plugins & Adapters"
        );
    }

    Ok(adapter)
}

/// Registers OneBot v11 from its `data/system.json` section, disabled when none is saved.
async fn register_onebot_adapter(
    supervisor: &Arc<Supervisor>,
) -> StartupResult<Arc<OneBotAdapter>> {
    let store = SystemConfigStore::default();
    let config = store
        .load_onebot()
        .map_err(|err| {
            format!(
                "Failed to load OneBot configuration from {}: {err}",
                store.path().display()
            )
        })?
        .unwrap_or_default();
    let adapter = Arc::new(OneBotAdapter::new(config)?);
    supervisor.adapters().register(adapter.clone()).await?;
    tracing::info!(
        platform = %adapter.identity(),
        enabled = adapter.config().enabled,
        "OneBot v11 adapter registered"
    );
    Ok(adapter)
}

/// Registers QQ Official from its `data/system.json` section, disabled when none is saved.
async fn register_qqofficial_adapter(
    supervisor: &Arc<Supervisor>,
) -> StartupResult<Arc<QqOfficialAdapter>> {
    let store = SystemConfigStore::default();
    let config = store
        .load_qqofficial()
        .map_err(|err| {
            format!(
                "Failed to load QQ Official configuration from {}: {err}",
                store.path().display()
            )
        })?
        .unwrap_or_default();
    let adapter = Arc::new(QqOfficialAdapter::new(config)?);
    supervisor.adapters().register(adapter.clone()).await?;
    tracing::info!(
        enabled = adapter.config().enabled,
        "QQ Official adapter registered"
    );
    Ok(adapter)
}

/// Installs the `tracing` subscriber with both console output and the log broadcast layer.
///
/// The WebSocket layer is attached to the same registry as the formatter, so `/ws/v1/logs`
/// observes exactly what operators see on stdout — no second, divergent logging path.
///
/// `log` is the `startup.log` filter directive; an invalid one stops startup rather than silently
/// logging at some other level.
fn init_tracing(observability: Arc<Observability>, log: &str) -> StartupResult<()> {
    let filter = EnvFilter::try_new(log)
        .map_err(|err| format!("Invalid startup.log filter '{log}': {err}"))?;

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(observability.logs.layer())
        .init();
    Ok(())
}

/// Launches the host processes of the plugins found by the startup scan.
///
/// Missing runtime environments (e.g. Python / TypeScript) or individual manifest errors
/// degrade gracefully to ensure the core microkernel and API gateway remain operational.
async fn launch_plugins(
    supervisor: &Arc<Supervisor>,
    plugins: Vec<kanon_core::DiscoveredPlugin>,
    plugin_state: &Arc<ToggleStore>,
) {
    tracing::info!(
        count = plugins.len(),
        "Discovered plugins during startup scan"
    );

    for discovered in plugins {
        let plugin_id = discovered.manifest.plugin.id.clone();
        let runtime = discovered.manifest.plugin.runtime.clone();
        // Startup, operator edits and watchdog restarts agree on the same per-plugin boundary.
        // Keep it until the host has read its saved configuration and entered the registry.
        let _configuration = supervisor.lock_plugin_config(&plugin_id).await;

        // A disabled plugin is not spawned at all: no host process, no routing, no adapter.
        if !plugin_state.is_enabled(PLUGIN_SECTION, &plugin_id).await {
            tracing::info!(
                plugin_id = %plugin_id,
                "Plugin is disabled by the operator; skipping launch"
            );
            continue;
        }

        match supervisor
            .spawn_from_manifest(&discovered.manifest_path, None)
            .await
        {
            Ok(host) => {
                tracing::info!(
                    plugin_id = %plugin_id,
                    host_id = %host.host_id,
                    runtime = %runtime,
                    "Plugin host launched and ready"
                );
            }
            Err(kanon_core::SupervisorError::RuntimeUnavailable { runtime, reason }) => {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    runtime = %runtime,
                    reason = %reason,
                    "Plugin runtime is unavailable on this host; marked as RuntimeUnavailable"
                );
            }
            Err(err) => {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    error = %err,
                    "Failed to launch plugin host; skipping gracefully without blocking startup"
                );
            }
        }
    }
}
