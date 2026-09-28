//! Kanon node entrypoint: the project's single node binary.
//!
//! This crate owns *assembly only*, and it is the only place in the workspace that turns the
//! libraries into a running node. The microkernel ([`kanon_core`]), the management gateway
//! ([`kanon_api`]) and the platform adapter ([`kanon_adapter_milky`]) are libraries with no
//! entrypoints of their own; `kanon-dev` is the separate developer CLI and never runs a node.
//!
//! As the composition root it starts the core IPC server (`core.sock`), the pipeline worker, the
//! process supervisor and the Axum management gateway in one process, wiring the observability hub
//! into both the `tracing` pipeline and the lifecycle trace bus.
//!
//! # Environment
//! - `KANON_API_ADDR` — management gateway bind address (default `127.0.0.1:8080`).
//! - `KANON_MILKY_BASE_URL` — Milky protocol implementation base URL; when set, the Milky adapter
//!   is enabled at startup. A configuration saved through the console wins over this bootstrap.
//! - `KANON_MILKY_TOKEN` — shared `access_token` for the Milky implementation.
//! - `KANON_MILKY_PLATFORM` — platform identifier owned by the Milky adapter (default `milky`).
//! - `KANON_MILKY_TRANSPORT` — inbound event transport, `sse` (default) or `websocket`.
//! - `KANON_ONEBOT_WS_URL` — OneBot v11 forward endpoint or reverse listener URL.
//! - `KANON_ONEBOT_TRANSPORT` — `forward_websocket` (default) or `reverse_websocket`.
//! - `KANON_ONEBOT_TOKEN` — shared OneBot access token.
//! - `KANON_ONEBOT_PLATFORM` — platform identifier (default `onebot`).
//! - `KANON_LLM_BASE_URL` — model provider base URL; when unset, chat debugging is disabled and
//!   `/api/v1/chat/completions` answers `503` instead of inventing a fake provider.
//! - `KANON_LLM_API_KEY` — provider credential.
//! - `KANON_LLM_MODEL` — default model identifier.
//! - `KANON_LLM_PROTOCOL` — `openai` (default), `openai_responses` or `anthropic`.
//! - `RUST_LOG` — standard `tracing` filter directive.
//!
//! Model routing itself lives in `data/system.json`: a named provider directory plus a per-model
//! settings catalog, both editable through the console. A model is addressed as
//! `<provider>/<model-id>`; the `KANON_LLM_*` variables only seed that directory when it is empty,
//! registering the endpoint under the preset name its base URL matches. The same document carries
//! the node-wide reply policy inherited by every instance without an override.

use std::net::SocketAddr;
use std::sync::Arc;

use kanon_adapter_milky::{MilkyAdapter, MilkyConfig};
use kanon_adapter_onebot::{OneBotAdapter, OneBotConfig};
use kanon_api::{
    ApiServer, ApiState, LlmProviderConfig, NodeSettings, Observability, SystemConfigStore,
};
use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    DEFAULT_INSTANCE_CATALOG, DEFAULT_MCP_CONFIG, DEFAULT_SKILLS_DIR, DEFAULT_TOGGLE_STATE,
    EventIngress, HOST_WATCHDOG_INTERVAL, InstanceRegistry, MCP_WATCHDOG_INTERVAL, McpConfigStore,
    McpPool, PLUGIN_SECTION, ReadSkillTool, SkillCatalogHook, SkillStore, ToggleStore,
    sync_instance_personas,
};
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default management gateway bind address (loopback only, never exposed by accident).
const DEFAULT_API_ADDR: &str = "127.0.0.1:8080";

/// Fallible startup result type shared by the binary entrypoint helpers.
type StartupResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::main]
async fn main() -> StartupResult<()> {
    let observability = Arc::new(Observability::new());
    init_tracing(observability.clone());

    let api_addr = resolve_api_addr()?;

    // --- Core microkernel & supervisor ------------------------------------------------
    let (event_tx, event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let ingress = EventIngress::new(event_tx);
    let default_ipc = CoreIpcServer::with_default_path(CoreApiService::new(ingress.clone()));
    let socket_path = default_ipc.socket_path().to_path_buf();

    let supervisor = Arc::new(Supervisor::new(None, Some(socket_path.clone())));

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

    // --- Management gateway state & agent engine --------------------------------------
    // The state owns one agent factory (and the named provider directory inside it), shared with
    // the pipeline worker and the IPC service, so a provider configured later through the console
    // is observed by all three without a restart.
    //
    // Bootstrap order: providers saved by the console win over the environment, because the console
    // is how an operator changes the node after it started. The settings are validated and applied
    // by the builder itself, so a bootstrapped directory and a console-configured one are applied
    // through exactly the same path.
    let node_settings = bootstrap_node_settings()?;
    let state = ApiState::builder(supervisor.clone())
        .with_milky_adapter(milky_adapter)
        .with_onebot_adapter(onebot_adapter)
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
        .with_hooks(vec![Arc::new(SkillCatalogHook::new(
            skills.clone(),
            plugin_state.clone(),
            instances.clone(),
        ))])
        .build();

    // Publish instance prompts as personas before the first message can arrive.
    sync_instance_personas(&instances.list().await, state.personas());

    match state.agent() {
        Some(agent) => tracing::info!(
            model = %agent.config().model_ref(),
            provider = ?agent.config().provider,
            temperature = ?agent.config().temperature,
            max_tokens = ?agent.config().max_tokens,
            "LLM provider configured for conversational pipeline and sandbox chat"
        ),
        None => tracing::warn!(
            "No LLM provider is configured (data/system.json and KANON_LLM_BASE_URL are both empty); \
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
            .with_mcp_pool(mcp_pool.clone()),
    );
    let pipeline_worker = engine.clone().start_worker(event_rx);
    let outbound_dispatcher = engine.clone().start_outbound_dispatcher();

    let service = CoreApiService::new(ingress.clone())
        .with_supervisor(supervisor.clone())
        .with_outbound_sender(engine.outbound_sender())
        .with_agent_slot(state.llm_slot().clone());
    let ipc_server = CoreIpcServer::new(socket_path.clone(), service);

    // --- Graceful shutdown channels ---------------------------------------------------
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel();
    let (api_shutdown_tx, api_shutdown_rx) = oneshot::channel();

    // Start Core IPC server FIRST so that spawned plugin hosts can connect to core.sock immediately
    let core_task = tokio::spawn(async move {
        ipc_server
            .run(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
    });

    // Wait briefly for Core socket to become active before spawning plugin processes
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Ensure ./plugins and ./data/plugins directories exist
    if let Err(err) = std::fs::create_dir_all("./plugins") {
        tracing::warn!(error = %err, "Failed to ensure ./plugins directory exists");
    }
    if let Err(err) = std::fs::create_dir_all("./data/plugins") {
        tracing::warn!(error = %err, "Failed to ensure ./data/plugins directory exists");
    }

    // Auto-discover and launch declared plugins from ./plugins directory, skipping any the
    // operator disabled.
    load_plugins_from_directory(&supervisor, "./plugins", &plugin_state).await;

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

    let api_server = ApiServer::bind(api_addr, state).await?;
    let bound_addr = api_server.local_addr();

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

    let _ = core_shutdown_tx.send(());
    let _ = api_shutdown_tx.send(());

    if let Err(err) = core_task.await? {
        tracing::error!(error = %err, "Core IPC server terminated with an error");
    }
    if let Err(err) = api_task.await? {
        tracing::error!(error = %err, "Management gateway terminated with an error");
    }

    pipeline_worker.abort();
    if let Some(handle) = outbound_dispatcher {
        handle.abort();
    }
    for (platform, error) in supervisor.adapters().stop_all().await {
        tracing::warn!(platform = %platform, error = %error, "Adapter failed to stop cleanly");
    }
    host_watchdog.abort();
    mcp_watchdog.abort();
    supervisor.stop_all().await?;

    tracing::info!("Kanon node shut down gracefully");
    Ok(())
}

/// Resolves the model-routing settings a freshly started node should use.
///
/// A directory persisted through the management console takes precedence over the `KANON_LLM_*`
/// environment bootstrap; the environment is only synthesized when the document carries no
/// provider at all. A malformed persisted document is a hard startup error rather than a silent
/// fallback, because running without the operator's chosen provider is exactly the surprise this
/// precedence exists to prevent.
fn bootstrap_node_settings() -> StartupResult<NodeSettings> {
    let store = SystemConfigStore::default();
    let persisted = store.load_node_settings().map_err(|err| {
        format!(
            "Failed to load node system configuration at {}: {err}",
            store.path().display()
        )
    })?;

    if persisted.has_providers() {
        tracing::info!(
            providers = ?persisted.providers.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            default_model = ?persisted.default_model,
            "Model provider directory loaded from data/system.json"
        );
        return Ok(persisted);
    }

    match LlmProviderConfig::from_env() {
        Some(config) => {
            tracing::info!(
                base_url = %config.base_url,
                model = %config.model,
                "No provider in data/system.json; using the KANON_LLM_* environment bootstrap"
            );
            // The environment only seeds the *provider*; policies persisted in the document are
            // the operator's own settings and must survive an environment-driven start.
            let mut settings = config.into_node_settings();
            settings.reply_policy = persisted.reply_policy;
            settings.context_policy = persisted.context_policy;
            Ok(settings)
        }
        None => Ok(persisted),
    }
}

/// Builds and registers the Milky platform adapter.
///
/// # Precedence
/// A configuration saved through the console is the node's own and wins over the
/// `KANON_MILKY_*` environment bootstrap, mirroring how the model provider is resolved: the
/// environment seeds a fresh deployment, the console is how it is changed afterwards. Unlike the
/// provider, a malformed *environment* value does not prevent the node from starting — the Milky
/// adapter is one optional platform among many, and refusing to boot the whole node over it would
/// be a worse failure than reporting it and staying disabled. A malformed *stored* document is
/// still a hard error, because that file is the node's own state and silently ignoring it would
/// start a node that does not do what its configuration says.
async fn register_milky_adapter(supervisor: &Arc<Supervisor>) -> StartupResult<Arc<MilkyAdapter>> {
    let store = SystemConfigStore::default();
    let persisted = store.load_milky().map_err(|err| {
        format!(
            "Failed to load the Milky adapter configuration from {}: {err}",
            store.path().display()
        )
    })?;

    let (config, source) = match persisted {
        Some(config) => (config, "data/system.json"),
        None => match MilkyConfig::from_env() {
            Ok(Some(config)) => (config, "environment"),
            Ok(None) => (MilkyConfig::default(), "defaults"),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "Ignoring an invalid KANON_MILKY_* bootstrap; the Milky adapter stays disabled"
                );
                (MilkyConfig::default(), "defaults")
            }
        },
    };

    let adapter = Arc::new(MilkyAdapter::new(config)?);
    supervisor.adapters().register(adapter.clone()).await?;

    let status = adapter.status();
    if status.enabled {
        tracing::info!(
            source = %source,
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

/// Registers OneBot v11, preferring saved settings over the environment bootstrap.
async fn register_onebot_adapter(
    supervisor: &Arc<Supervisor>,
) -> StartupResult<Arc<OneBotAdapter>> {
    let store = SystemConfigStore::default();
    let config = match store.load_onebot().map_err(|err| {
        format!(
            "Failed to load OneBot configuration from {}: {err}",
            store.path().display()
        )
    })? {
        Some(config) => config,
        None => OneBotConfig::from_env()?.unwrap_or_default(),
    };
    let adapter = Arc::new(OneBotAdapter::new(config)?);
    supervisor.adapters().register(adapter.clone()).await?;
    tracing::info!(
        platform = %adapter.identity(),
        enabled = adapter.config().enabled,
        "OneBot v11 adapter registered"
    );
    Ok(adapter)
}

/// Installs the `tracing` subscriber with both console output and the log broadcast layer.
///
/// The WebSocket layer is attached to the same registry as the formatter, so `/ws/v1/logs`
/// observes exactly what operators see on stdout — no second, divergent logging path.
fn init_tracing(observability: Arc<Observability>) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(observability.logs.layer())
        .init();
}

/// Resolves the management gateway bind address from the environment.
fn resolve_api_addr() -> StartupResult<SocketAddr> {
    let raw = std::env::var("KANON_API_ADDR").unwrap_or_else(|_| DEFAULT_API_ADDR.to_string());
    raw.parse::<SocketAddr>()
        .map_err(|err| format!("Invalid KANON_API_ADDR '{raw}': {err}").into())
}

/// Discovers plugins in the specified directory and launches their host processes.
///
/// Missing runtime environments (e.g. Python / TypeScript) or individual manifest errors
/// degrade gracefully to ensure the core microkernel and API gateway remain operational.
async fn load_plugins_from_directory(
    supervisor: &Arc<Supervisor>,
    dir: impl AsRef<std::path::Path>,
    plugin_state: &Arc<ToggleStore>,
) {
    let plugins = match kanon_core::PluginScanner::scan(dir.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(dir = %dir.as_ref().display(), error = %e, "Failed to scan plugins directory");
            return;
        }
    };

    tracing::info!(
        count = plugins.len(),
        "Discovered plugins during startup scan"
    );

    for discovered in plugins {
        let plugin_id = discovered.manifest.plugin.id.clone();
        let runtime = discovered.manifest.plugin.runtime.clone();

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
