//! Out-of-process plugin host runner.
//!
//! Exposes [`KanonHost`], which hosts a [`Plugin`] implementation, serves
//! `PluginHostService` and `MessagePipelineService` over an IPC listener,
//! and optionally registers itself with the Core microkernel.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

use crate::context::{CoreHandle, PluginContext};
use crate::plugin::Plugin;
use kanon_proto::v1::message_pipeline_service_server::{
    MessagePipelineService, MessagePipelineServiceServer,
};
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, DecorateReplyRequest, DecorateReplyResult,
    DeliverMessageRequest, DeliverMessageResponse, EventAck, EventNotification,
    GetPluginMetaRequest, GetPluginMetaResponse, PipelineEventRequest, PreFilterResult,
    PrepareTurnRequest, PrepareTurnResult, RegisterHostRequest, ReloadPluginConfigRequest,
    ReloadPluginConfigResponse, ToolCallRequest, ToolCallResponse,
};
use kanon_transport::{IpcListener, connect_ipc};

/// Out-of-process gRPC host for a Kanon plugin.
pub struct KanonHost<P: Plugin> {
    /// Inner plugin instance wrapped for shared thread-safe access.
    plugin: Arc<RwLock<P>>,
    /// Path to the dedicated socket on which this host will listen.
    socket_path: PathBuf,
    /// Path to the Kanon Core socket (`core.sock`).
    core_sock: Option<PathBuf>,
}

impl<P: Plugin> KanonHost<P> {
    /// Creates a new `KanonHost` wrapping the given plugin.
    ///
    /// Reads `KANON_HOST_SOCK` from the environment if present, falling back to `./run/host_rust.sock`.
    /// Reads `KANON_CORE_SOCK` from the environment if present.
    pub fn new(plugin: P) -> Self {
        let socket_path = std::env::var("KANON_HOST_SOCK")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./run/host_rust.sock"));

        let core_sock = std::env::var("KANON_CORE_SOCK").ok().map(PathBuf::from);

        Self {
            plugin: Arc::new(RwLock::new(plugin)),
            socket_path,
            core_sock,
        }
    }

    /// Explicitly sets the IPC endpoint socket path for this host.
    pub fn with_socket_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_path = path.into();
        self
    }

    /// Explicitly sets the Core IPC socket path (`core.sock`).
    pub fn with_core_sock(mut self, path: impl Into<PathBuf>) -> Self {
        self.core_sock = Some(path.into());
        self
    }

    /// Runs the plugin host until the process is asked to stop, or the core disappears.
    ///
    /// `SIGINT` and (on Unix) `SIGTERM` both request a graceful shutdown, which unloads the
    /// plugin and closes its platform connections. A core that stops answering liveness probes
    /// triggers the same path through [`crate::watchdog::watch_core`].
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.run_with_shutdown(shutdown_signal()).await
    }

    /// Runs the plugin host until the provided `shutdown` future resolves.
    pub async fn run_with_shutdown<F>(
        self,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let plugin_id = self.plugin.read().await.meta().id;

        // 1. Connect to Core before initializing the plugin: adapter plugins capture the core
        //    handle from their context during `on_load`, so the connection must already exist.
        let core_handle = self.connect_core(&plugin_id).await;
        // The same channel backs the liveness watchdog, so a dead core is noticed without any
        // extra connection.
        let core_watchdog = core_handle
            .clone()
            .map(|handle| (handle, crate::watchdog::CoreWatchdogConfig::default()));

        // 2. Initialize plugin lifecycle
        let data_dir = PathBuf::from(format!("./data/plugins/{plugin_id}"));
        // The core pushes configuration only when the operator changes it, so the last saved
        // configuration must be read here or the plugin would start unconfigured after every
        // restart.
        let config = read_stored_config(&data_dir)?;
        let mut ctx = PluginContext::new(data_dir, config);
        match core_handle {
            Some(handle) => ctx = ctx.with_core(handle),
            None => {
                eprintln!(
                    "[kanon-host] core is not reachable; host '{plugin_id}' continues in standalone mode"
                );
                tracing::warn!(
                    plugin_id = %plugin_id,
                    "Core is not reachable; continuing in standalone mode with ctx.core = None"
                );
            }
        }

        self.plugin.write().await.on_load(&mut ctx).await?;
        tracing::info!(plugin_id = %plugin_id, socket = %self.socket_path.display(), "Plugin loaded successfully");

        let auth = kanon_transport::AuthInterceptor::new(
            std::env::var("KANON_IPC_TOKEN").unwrap_or_default(),
        );

        // 3. Bind IPC Listener
        let listener = IpcListener::bind(&self.socket_path)?;
        let incoming = listener.incoming();

        // 4. Start gRPC services
        let host_svc = HostServiceImpl {
            plugin: self.plugin.clone(),
            config_versions: Arc::new(RwLock::new(std::collections::HashMap::new())),
        };
        let pipeline_svc = PipelineServiceImpl {
            plugin: self.plugin.clone(),
        };

        // 5. Stop for whichever reason comes first: an explicit shutdown request or a core that
        //    stopped answering. The watchdog matters because a killed core leaves this process
        //    running; an orphaned host would keep serving its platform and, once a new core
        //    starts, double-handle every message. Exiting is safe: the new core spawns its own
        //    hosts from the plugin directory.
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (reason_tx, mut reason_rx) =
            tokio::sync::oneshot::channel::<crate::watchdog::StopReason>();

        let requested_tx = stop_tx.clone();
        let requested = async move {
            shutdown.await;
            // Tell the watchdog the host is stopping for its own reasons.
            let _ = requested_tx.send(()).await;
            crate::watchdog::StopReason::Requested
        };

        let server_shutdown = async move {
            let reason = match core_watchdog {
                Some((handle, config)) => {
                    tokio::select! {
                        reason = crate::watchdog::watch_core(handle, config, stop_rx) => reason,
                        reason = requested => reason,
                    }
                }
                // No core was reachable at startup (standalone mode): only an explicit shutdown
                // request can stop this host.
                None => requested.await,
            };
            let _ = reason_tx.send(reason);
        };

        // 6. Serve until the shutdown future above resolves.
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::with_interceptor(
                host_svc,
                auth.clone(),
            ))
            .add_service(MessagePipelineServiceServer::with_interceptor(
                pipeline_svc,
                auth,
            ))
            .serve_with_incoming_shutdown(incoming, server_shutdown)
            .await?;

        // Terminal lifecycle events go to stderr as well as to `tracing`: a host that stops is
        // exactly the moment an operator needs an explanation, and plugins are free not to
        // install a tracing subscriber. `tracing` output is kept for structured pipelines.
        match reason_rx.try_recv() {
            Ok(crate::watchdog::StopReason::CoreLost) => {
                let message = format!(
                    "[kanon-host] core unreachable; stopping host '{plugin_id}' so it cannot serve its platform without a core"
                );
                eprintln!("{message}");
                tracing::warn!(plugin_id = %plugin_id, "Stopping host: the core it registered with is gone");
            }
            Ok(crate::watchdog::StopReason::Requested) => {
                tracing::info!(plugin_id = %plugin_id, "Stopping host: shutdown requested");
            }
            // The shutdown signal future always reports a reason before the server returns; a
            // missing one would mean the server stopped for an unknown cause, which is worth a
            // loud line rather than silence.
            Err(_) => {
                eprintln!(
                    "[kanon-host] host '{plugin_id}' gRPC server stopped without a recorded reason"
                );
                tracing::warn!(plugin_id = %plugin_id, "Host gRPC server stopped without a recorded reason");
            }
        }

        // 7. Cleanup on shutdown
        self.plugin.write().await.on_unload().await?;

        Ok(())
    }

    /// Registers this host with the Core microkernel and returns a reusable core handle.
    ///
    /// Registration doubles as the reachability probe: the handle is only produced when the core
    /// actually answered, so a dead or foreign socket yields `None` (standalone mode) instead of a
    /// handle that would fail on the first ingest.
    async fn connect_core(&self, plugin_id: &str) -> Option<CoreHandle> {
        let core_path = self.core_sock.as_ref()?;

        let channel = match connect_ipc(core_path.clone()).await {
            Ok(channel) => channel,
            Err(e) => {
                tracing::warn!(error = %e, "Core socket not reachable at startup; running standalone");
                return None;
            }
        };

        // `KANON_HOST_ID` is part of the Supervisor's startup contract; a host started by hand
        // (tests, debugging) registers under its plugin id instead.
        let host_id = std::env::var("KANON_HOST_ID").unwrap_or_else(|_| plugin_id.to_string());
        let handle = CoreHandle::new(channel).with_identity(plugin_id, host_id.clone());
        let registration = RegisterHostRequest {
            host_id,
            runtime: "rust".to_string(),
            endpoint: self.socket_path.to_string_lossy().to_string(),
            loaded_plugin_ids: vec![plugin_id.to_string()],
        };

        match handle.register_host(registration).await {
            Ok(_) => {
                tracing::info!("Successfully registered with Core IPC server");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to register with Core; running standalone");
                None
            }
        }
    }
}

/// Reads the configuration the core saved for this plugin (`<data_dir>/config.json`).
///
/// A missing file means the plugin was never configured. An unreadable or malformed one fails
/// startup: running with an empty configuration would silently ignore what the operator saved.
fn read_stored_config(
    data_dir: &Path,
) -> Result<Option<prost_types::Struct>, Box<dyn std::error::Error + Send + Sync>> {
    let path = data_dir.join("config.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display()).into()),
    };
    match serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|err| format!("invalid {}: {err}", path.display()))?
    {
        serde_json::Value::Object(fields) => Ok(Some(crate::json::to_struct(fields))),
        _ => Err(format!("{} must contain a JSON object", path.display()).into()),
    }
}

/// Implementation of [`PluginHostService`] for managing host lifecycle and inspection.
struct HostServiceImpl<P: Plugin> {
    plugin: Arc<RwLock<P>>,
    config_versions: Arc<RwLock<std::collections::HashMap<String, u64>>>,
}

#[tonic::async_trait]
impl<P: Plugin> PluginHostService for HostServiceImpl<P> {
    async fn ping(
        &self,
        request: Request<kanon_proto::v1::PingRequest>,
    ) -> Result<Response<kanon_proto::v1::PingResponse>, Status> {
        let req = request.into_inner();
        Ok(Response::new(kanon_proto::v1::PingResponse {
            timestamp: req.timestamp,
        }))
    }

    async fn reload_plugin_config(
        &self,
        request: Request<ReloadPluginConfigRequest>,
    ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
        let req = request.into_inner();
        let mut versions = self.config_versions.write().await;
        let current_ver = *versions.get(&req.plugin_id).unwrap_or(&0);

        if req.version > 0 && req.version <= current_ver {
            tracing::warn!(
                plugin_id = %req.plugin_id,
                current_version = current_ver,
                requested_version = req.version,
                "Rejecting stale or out-of-order configuration reload"
            );
            return Ok(Response::new(ReloadPluginConfigResponse {
                success: false,
                error_message: format!(
                    "Stale config version {}: current is {}",
                    req.version, current_ver
                ),
                applied_version: current_ver,
            }));
        }

        // The version is recorded only after the plugin accepted the payload, so a rejected
        // reload can be retried with the same version.
        if let Some(config) = req.config {
            if let Err(err) = self.plugin.write().await.on_config_reload(config).await {
                return Ok(Response::new(ReloadPluginConfigResponse {
                    success: false,
                    error_message: format!("on_config_reload failed: {err}"),
                    applied_version: current_ver,
                }));
            }
        }

        let applied = if req.version > 0 {
            req.version
        } else {
            current_ver + 1
        };
        versions.insert(req.plugin_id.clone(), applied);
        tracing::info!(
            plugin_id = %req.plugin_id,
            version = applied,
            "Reloading plugin configuration"
        );
        Ok(Response::new(ReloadPluginConfigResponse {
            success: true,
            error_message: String::new(),
            applied_version: applied,
        }))
    }

    /// Serves a control-plane management action.
    ///
    /// Failures are answered as `success = false` with the reason, never as a transport error,
    /// so the console can show the plugin's own explanation.
    async fn invoke_action(
        &self,
        request: Request<kanon_proto::v1::PluginActionRequest>,
    ) -> Result<Response<kanon_proto::v1::PluginActionResponse>, Status> {
        let request = request.into_inner();
        let params = request
            .parameters
            .map(|params| serde_json::Value::Object(crate::json::from_struct(params)))
            .unwrap_or(serde_json::Value::Null);
        let outcome = self
            .plugin
            .read()
            .await
            .on_invoke_action(&request.action, params)
            .await;
        Ok(Response::new(match outcome {
            Ok(serde_json::Value::Null) => kanon_proto::v1::PluginActionResponse {
                success: true,
                ..Default::default()
            },
            Ok(serde_json::Value::Object(fields)) => kanon_proto::v1::PluginActionResponse {
                success: true,
                error_message: String::new(),
                result: Some(crate::json::to_struct(fields)),
            },
            Ok(_) => kanon_proto::v1::PluginActionResponse {
                success: false,
                error_message: format!("action '{}' returned a non-object result", request.action),
                result: None,
            },
            Err(err) => kanon_proto::v1::PluginActionResponse {
                success: false,
                error_message: format!("Action failed: {err}"),
                result: None,
            },
        }))
    }

    async fn get_plugin_meta(
        &self,
        _request: Request<GetPluginMetaRequest>,
    ) -> Result<Response<GetPluginMetaResponse>, Status> {
        let meta = self.plugin.read().await.meta();
        Ok(Response::new(GetPluginMetaResponse {
            plugins: vec![meta],
        }))
    }
}

/// Implementation of [`MessagePipelineService`] for dispatching events and commands to the plugin.
struct PipelineServiceImpl<P: Plugin> {
    plugin: Arc<RwLock<P>>,
}

#[tonic::async_trait]
impl<P: Plugin> MessagePipelineService for PipelineServiceImpl<P> {
    /// Lets the plugin rewrite the system prompt. A handler error, and an empty replacement
    /// (which the core would refuse anyway), are answered as errors so the core logs them and
    /// keeps the prompt unchanged.
    async fn on_llm_request(
        &self,
        request: Request<kanon_proto::v1::LlmRequestHookRequest>,
    ) -> Result<Response<kanon_proto::v1::LlmRequestHookResult>, Status> {
        let plugin = self.plugin.read().await;
        match plugin.on_llm_request(request.into_inner()).await {
            Ok(Some(prompt)) if prompt.trim().is_empty() => Err(Status::internal(
                "the system prompt rewrite returned an empty prompt",
            )),
            Ok(system_prompt) => Ok(Response::new(kanon_proto::v1::LlmRequestHookResult {
                system_prompt,
            })),
            Err(err) => Err(Status::internal(err.to_string())),
        }
    }

    /// Serves one HTTP request. Handler failures are the plugin's to answer (the `Router`
    /// turns them into 500 responses); an error from a hand-written plugin becomes a status.
    async fn on_http_request(
        &self,
        request: Request<kanon_proto::v1::HttpRequest>,
    ) -> Result<Response<kanon_proto::v1::HttpResponse>, Status> {
        let plugin = self.plugin.read().await;
        match plugin.on_http_request(request.into_inner()).await {
            Ok(response) => Ok(Response::new(response)),
            Err(err) => Err(Status::internal(err.to_string())),
        }
    }

    async fn on_pre_filter(
        &self,
        request: Request<PipelineEventRequest>,
    ) -> Result<Response<PreFilterResult>, Status> {
        let req = request.into_inner();
        let plugin = self.plugin.read().await;
        match plugin.on_pre_filter(req).await {
            Ok(Some(result)) => Ok(Response::new(result)),
            Ok(None) => Ok(Response::new(PreFilterResult {
                action: kanon_proto::v1::pre_filter_result::Action::Pass as i32,
                modified_text: String::new(),
                reply_messages: vec![],
            })),
            Err(e) => Err(Status::internal(format!("PreFilter error: {e}"))),
        }
    }

    async fn on_execute_command(
        &self,
        request: Request<CommandExecuteRequest>,
    ) -> Result<Response<CommandExecuteResponse>, Status> {
        let req = request.into_inner();
        let plugin = self.plugin.read().await;
        match plugin.on_execute_command(req).await {
            Ok(resp) => Ok(Response::new(resp)),
            Err(e) => Ok(Response::new(CommandExecuteResponse {
                success: false,
                replies: vec![],
                error_message: e.to_string(),
                capture_seconds: 0,
                pass_to_model: false,
                model_text: None,
            })),
        }
    }

    async fn on_call_tool(
        &self,
        request: Request<ToolCallRequest>,
    ) -> Result<Response<ToolCallResponse>, Status> {
        let req = request.into_inner();
        let plugin = self.plugin.read().await;
        match plugin.on_call_tool(req).await {
            Ok(resp) => Ok(Response::new(resp)),
            Err(e) => Ok(Response::new(ToolCallResponse {
                call_id: String::new(),
                success: false,
                error_message: e.to_string(),
                payload: None,
                attachments: Vec::new(),
            })),
        }
    }

    async fn on_event(
        &self,
        request: Request<EventNotification>,
    ) -> Result<Response<EventAck>, Status> {
        let plugin = self.plugin.read().await;
        // Events are fire-and-forget for the core, so a handler failure is only logged here.
        if let Err(err) = plugin.on_event(request.into_inner()).await {
            tracing::warn!(error = %err, "Plugin event handler failed");
        }
        Ok(Response::new(EventAck { received: true }))
    }

    async fn on_decorate_reply(
        &self,
        request: Request<DecorateReplyRequest>,
    ) -> Result<Response<DecorateReplyResult>, Status> {
        let plugin = self.plugin.read().await;
        match plugin.on_decorate_reply(request.into_inner()).await {
            Ok(Some(segments)) => Ok(Response::new(DecorateReplyResult {
                modified: true,
                segments,
            })),
            Ok(None) => Ok(Response::new(DecorateReplyResult::default())),
            // A failed decorator must not eat the reply: report an error and the core keeps the
            // reply as it was.
            Err(err) => Err(Status::internal(err.to_string())),
        }
    }

    async fn on_prepare_turn(
        &self,
        request: Request<PrepareTurnRequest>,
    ) -> Result<Response<PrepareTurnResult>, Status> {
        let plugin = self.plugin.read().await;
        match plugin.on_prepare_turn(request.into_inner()).await {
            Ok(text) => Ok(Response::new(PrepareTurnResult { text })),
            // Reported as an error so the core logs it; the turn goes ahead without this context.
            Err(err) => Err(Status::internal(err.to_string())),
        }
    }

    async fn on_deliver_message(
        &self,
        request: Request<DeliverMessageRequest>,
    ) -> Result<Response<DeliverMessageResponse>, Status> {
        let req = request.into_inner();
        let plugin = self.plugin.read().await;

        // Mirror `on_execute_command`: a plugin error becomes an explicit failure response rather
        // than a fabricated success, so the core can report the delivery as failed.
        match plugin.on_deliver_message(req).await {
            Ok(resp) => Ok(Response::new(resp)),
            Err(e) => Ok(Response::new(DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: e.to_string(),
            })),
        }
    }
}

/// Resolves when the process is asked to stop: `SIGINT`, or `SIGTERM` on Unix.
///
/// `SIGTERM` is what supervisors and container runtimes send, so ignoring it would leave a host
/// that only stops on `Ctrl-C` — exactly the behaviour that produces orphaned platforms.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!(error = %err, "Failed to install SIGTERM handler; falling back to SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
