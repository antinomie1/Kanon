//! Plugin lifecycle and configuration control routes.
//!
//! # Design notes
//! - The **running process** is authoritative for plugin metadata: `GetPluginMeta` results are
//!   cached on [`kanon_core::ManagedHost`] at handshake time.
//! - The **static manifest** is authoritative for declaration-time facts the process does not
//!   report over gRPC, namely the `[config_schema]` JSON Schema used to render console forms.
//! - Configuration updates are committed in the order *validate → hot reload → persist*: the
//!   plugin host judges the payload first, and only an accepted configuration is written to
//!   disk. A rejected reload therefore leaves both the running plugin and the persisted file
//!   untouched.

use std::path::Path;

use axum::Json;
use axum::Router;
use axum::extract::{FromRequest, Path as AxumPath, State};
use axum::routing::{get, post};
use kanon_core::{ManagedHost, PluginManifest, PluginScanner, SupervisorError};
use kanon_llm::tool_router::{json_to_prost_struct, prost_struct_to_json};
use kanon_proto::v1::PluginMeta;
use kanon_storage::PluginId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::observability::TraceEvent;
use crate::plugin_config::PluginConfigStore;
use crate::state::ApiState;

/// Registers all plugin management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route("/api/v1/plugins/install", post(install_plugin))
        .route(
            "/api/v1/plugins/:id/config",
            get(get_config).put(put_config),
        )
        .route("/api/v1/plugins/:id/restart", post(restart_plugin))
        // Enabling starts the host process; disabling stops it, so a disabled plugin releases its
        // memory and disappears from routing entirely.
        .route(
            "/api/v1/plugins/:id/enabled",
            axum::routing::put(set_plugin_enabled),
        )
        .route(
            "/api/v1/plugins/:id/tools/:tool_name",
            post(call_plugin_tool),
        )
        // Management actions: the operator-facing counterpart of tools. They are never offered
        // to the model, which is what lets an adapter expose credential binding safely.
        .route(
            "/api/v1/plugins/:id/actions/:action_name",
            post(invoke_plugin_action),
        )
}

/// Request body for installing a plugin from a local directory path.
#[derive(Debug, Deserialize)]
pub struct InstallPathRequest {
    /// Filesystem path to the local plugin directory.
    pub path: String,
}

/// Standard response payload returned after a plugin installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPluginResponse {
    /// Unique plugin identifier.
    pub plugin_id: String,
    /// Human-readable plugin name.
    pub name: String,
    /// Semantic version of the installed plugin.
    pub version: String,
    /// Runtime platform declared by the plugin.
    pub runtime: String,
    /// List of statically declared commands.
    pub commands: Vec<CommandView>,
    /// List of statically declared tools.
    pub tools: Vec<ToolView>,
    /// Lifecycle status after installation (`running` or `RuntimeUnavailable`).
    pub status: String,
    /// Informational or status message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Catalog of supervised hosts and their plugins.
#[derive(Debug, Serialize)]
pub struct PluginCatalog {
    /// Total number of plugin instances across every host.
    pub total: usize,
    /// Supervised host processes.
    pub hosts: Vec<HostView>,
    /// Flat plugin listing with owning host information.
    pub plugins: Vec<PluginView>,
}

/// A supervised plugin host process.
#[derive(Debug, Serialize)]
pub struct HostView {
    /// Host process identifier.
    pub host_id: String,
    /// Lifecycle status of the host process.
    pub status: String,
    /// Runtime language declared by the manifest, when known.
    pub runtime: Option<String>,
    /// IPC endpoint the host listens on.
    pub socket_path: String,
    /// Pipeline scheduling priority.
    pub priority: i32,
    /// Identifiers of the plugins served by this host.
    pub plugin_ids: Vec<String>,
    /// Whether the control plane can restart this host (a launch recipe is recorded).
    pub restartable: bool,
    /// Operating system process ID (PID) of the managed host child process, if running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Plugin instances hosted by this process.
    pub plugins: Vec<PluginView>,
}

/// A plugin instance with its declared capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginView {
    /// Plugin identifier (e.g. `org.kanon.plugin.weather`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Plugin version string.
    pub version: String,
    /// Author attribution.
    pub author: String,
    /// Short description.
    pub description: String,
    /// Owning host process identifier.
    pub host_id: String,
    /// Runtime language of the owning host.
    pub runtime: Option<String>,
    /// Scheduling priority inherited from the manifest.
    pub priority: i32,
    /// Lifecycle status (`running`, `declared`, `disabled`, `crashed`, or `RuntimeUnavailable`).
    pub status: String,
    /// Whether the operator allows this plugin to run.
    ///
    /// A disabled plugin is not merely idle: its host process is stopped, so its pre-filters,
    /// commands, tools and adapter disappear from the node.
    pub enabled: bool,
    /// Runtime health reported by the host watchdog, when the plugin has a host process.
    pub health: Option<kanon_core::HostHealth>,
    /// Statically declared commands.
    pub commands: Vec<CommandView>,
    /// Statically declared tools and their parameter schemas.
    pub tools: Vec<ToolView>,
}

/// Request body for enabling or disabling a plugin.
#[derive(Debug, Deserialize)]
pub struct SetPluginEnabledRequest {
    /// Whether the plugin should run on this node.
    pub enabled: bool,
}

/// Confirmation returned after a plugin is enabled or disabled.
#[derive(Debug, Serialize)]
pub struct PluginStateResponse {
    /// Whether the catalog changed.
    pub applied: bool,
    /// Human-readable confirmation.
    pub message: String,
    /// Plugin identifier the request addressed.
    pub plugin_id: String,
    /// State after the call.
    pub enabled: bool,
    /// Owning host identifier, when the plugin is running.
    pub host_id: Option<String>,
}

/// A command declared by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandView {
    /// Command trigger word without the leading slash.
    pub name: String,
    /// Help text.
    pub description: String,
    /// Usage syntax.
    pub usage: String,
    /// Dispatch priority.
    pub priority: i32,
}

/// A tool declared by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolView {
    /// Tool name exposed to the model.
    pub name: String,
    /// Description presented to the model.
    pub description: String,
    /// JSON Schema of the accepted parameters.
    pub parameters: Value,
}

/// Current configuration values plus the declaration schema.
#[derive(Debug, Serialize)]
pub struct PluginConfigView {
    /// Plugin identifier.
    pub plugin_id: String,
    /// Effective configuration values (schema defaults merged with persisted values).
    pub values: Value,
    /// Declared JSON Schema, or `null` when the manifest declares none.
    pub schema: Value,
    /// Whether the values came from a persisted file rather than schema defaults alone.
    pub persisted: bool,
    /// Currently applied configuration version token.
    pub version: u64,
}

/// Request body for `PUT /api/v1/plugins/:id/config`.
#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    /// Complete replacement configuration object.
    pub values: Value,
    /// Optional expected version for optimistic concurrency control (CAS).
    /// If provided and mismatched against in-memory state, yields HTTP 409 Conflict.
    #[serde(default)]
    pub version: Option<u64>,
}

/// Confirmation payload returned after a successful configuration update.
#[derive(Debug, Serialize)]
pub struct UpdateConfigResponse {
    /// Plugin identifier.
    pub plugin_id: String,
    /// Host process that acknowledged the reload.
    pub host_id: String,
    /// Effective configuration values now in force.
    pub values: Value,
    /// Always `true`: the field exists so console clients can assert on the outcome explicitly.
    pub reloaded: bool,
    /// Monotonically incremented configuration version token.
    pub version: u64,
}

/// Confirmation payload returned after a host restart.
#[derive(Debug, Serialize)]
pub struct RestartResponse {
    /// Host process that was restarted.
    pub host_id: String,
    /// Plugins reported by the freshly restarted host.
    pub plugins: Vec<PluginView>,
}

/// Lists every supervised host and plugin with static metadata.
async fn list_plugins(State(state): State<ApiState>) -> Json<PluginCatalog> {
    let hosts = state.supervisor().get_all_hosts().await;

    let mut host_views = Vec::with_capacity(hosts.len());
    let mut plugin_views = Vec::new();

    for host in &hosts {
        let p_views = plugin_views_for(host);
        let pid = host.pid().await;
        host_views.push(host_view(host, pid, p_views.clone()));
        plugin_views.extend(p_views);
    }

    // Include any plugins recorded as unavailable due to missing runtime environments
    let unavailable = state.supervisor().get_unavailable_plugins().await;
    for unavail in unavailable {
        let plugin_view = plugin_view_from_manifest_with_status(&unavail.manifest, &unavail.status);
        let host_id = format!("host_{}", unavail.manifest.plugin.id.replace('.', "_"));
        host_views.push(HostView {
            host_id: host_id.clone(),
            status: unavail.status.clone(),
            runtime: Some(unavail.manifest.plugin.runtime.clone()),
            socket_path: String::new(),
            priority: unavail.manifest.plugin.priority.unwrap_or(500),
            plugin_ids: vec![unavail.manifest.plugin.id.clone()],
            restartable: true,
            pid: None,
            plugins: vec![plugin_view.clone()],
        });
        plugin_views.push(plugin_view);
    }

    // Disabled plugins have no host at all, so the catalog would otherwise hide them and the
    // console could never re-enable one. Scan the plugin directory and present the rest (with a
    // placeholder status: the overlay below decides the final one).
    for discovered in discovered_plugin_views(state.plugins_dir(), &plugin_views) {
        plugin_views.push(discovered);
    }

    // Overlay the operator's enable/disable state and the watchdog's health, after every entry
    // exists, so the console sees one authoritative status per plugin instead of inferring it from
    // a missing host. This must run last: a plugin with no host is not necessarily disabled (its
    // launch may have failed), and only the state store knows the operator's intent.
    for view in &mut plugin_views {
        let enabled = state
            .plugin_state()
            .is_enabled(kanon_core::PLUGIN_SECTION, &view.id)
            .await;
        view.enabled = enabled;
        view.health = match hosts.iter().find(|host| host.host_id == view.host_id) {
            Some(host) => Some(host.health().await),
            None => None,
        };
        if !enabled {
            view.status = "disabled".to_string();
        } else if let Some(health) = &view.health
            && health.state == "crashed"
        {
            view.status = "crashed".to_string();
        }
    }

    // Host views were assembled before the overlay, so propagate the resolved state into their
    // nested plugin entries: otherwise the console would show one plugin as enabled in the host
    // card and disabled in the catalog.
    for host_view in &mut host_views {
        for plugin in &mut host_view.plugins {
            if let Some(resolved) = plugin_views.iter().find(|view| view.id == plugin.id) {
                plugin.enabled = resolved.enabled;
                plugin.health = resolved.health.clone();
                plugin.status = resolved.status.clone();
            }
        }
    }

    // Deterministic ordering keeps console tables stable across polls.
    host_views.sort_by(|a, b| a.host_id.cmp(&b.host_id));
    plugin_views.sort_by(|a, b| a.id.cmp(&b.id));

    Json(PluginCatalog {
        total: plugin_views.len(),
        hosts: host_views,
        plugins: plugin_views,
    })
}

/// Builds catalog entries for plugins on disk that have no running host.
///
/// Without this fill the console would lose the ability to re-enable a plugin it had disabled
/// (and could not see a plugin whose runtime is missing, either), because both cases have no
/// host process to enumerate. The caller applies the enable/disable overlay afterwards.
fn discovered_plugin_views(
    plugins_dir: &std::path::Path,
    existing: &[PluginView],
) -> Vec<PluginView> {
    let discovered = match kanon_core::PluginScanner::scan(plugins_dir) {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(
                dir = %plugins_dir.display(),
                error = %err,
                "Could not scan the plugin directory while building the catalog"
            );
            return Vec::new();
        }
    };

    discovered
        .into_iter()
        .filter(|plugin| {
            !existing
                .iter()
                .any(|view| view.id == plugin.manifest.plugin.id)
        })
        .map(|plugin| {
            let manifest = plugin.manifest;
            let host_id = format!("host_{}", manifest.plugin.id.replace('.', "_"));
            // The state overlay in `list_plugins` runs before this fill, so mark these entries
            // directly: a plugin with no host is either disabled or could not be launched.
            // State-agnostic placeholder: the caller's overlay applies the operator's intent.
            let mut view = plugin_view_from_manifest_with_status(&manifest, "declared");
            view.host_id = host_id;
            view
        })
        .collect()
}

/// Returns the current configuration and declaration schema for a plugin.
async fn get_config(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
) -> Result<Json<PluginConfigView>, ApiError> {
    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let schema = host
        .manifest()
        .and_then(|manifest| manifest.config_schema.clone());

    let store = state.config_store();
    let stored = store.load(&plugin_id)?;
    let persisted = stored.as_object().is_some_and(|map| !map.is_empty());
    let values = PluginConfigStore::apply_defaults(schema.as_ref(), &stored);
    let version = state.supervisor().config_version(&plugin_id).await;

    Ok(Json(PluginConfigView {
        plugin_id,
        values,
        schema: schema.unwrap_or(Value::Null),
        persisted,
        version,
    }))
}

/// Validates, hot reloads and persists a plugin configuration.
async fn put_config(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
    Json(body): Json<UpdateConfigRequest>,
) -> Result<Json<UpdateConfigResponse>, ApiError> {
    if !body.values.is_object() {
        return Err(ApiError::BadRequest(
            "Field 'values' must be a JSON object".to_string(),
        ));
    }

    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let schema = host
        .manifest()
        .and_then(|manifest| manifest.config_schema.clone());

    PluginConfigStore::validate(schema.as_ref(), &body.values)
        .map_err(|reason| ApiError::BadRequest(format!("Invalid configuration: {reason}")))?;

    // Step 1: let the plugin host accept or reject the payload before anything becomes durable.
    // Optimistic concurrency control (CAS) verifies the version vector and rejects stale reloads.
    let applied_version = state
        .supervisor()
        .reload_plugin_config_cas(&plugin_id, &body.values, body.version)
        .await?;

    // Step 2: persist only an accepted configuration. `spawn_blocking` keeps small filesystem
    // writes off the async worker threads shared with request handling.
    let store = state.config_store().clone();
    let values = body.values.clone();
    let persist_id = plugin_id.clone();
    tokio::task::spawn_blocking(move || store.store(&persist_id, &values))
        .await
        .map_err(|err| ApiError::Internal(format!("Persistence task failed: {err}")))??;

    state
        .observability()
        .events
        .publish(TraceEvent::PluginConfigUpdated {
            plugin_id: plugin_id.clone(),
            host_id: host.host_id.clone(),
        });

    tracing::info!(
        plugin_id = %plugin_id,
        host_id = %host.host_id,
        version = applied_version,
        "Plugin configuration updated and hot reloaded"
    );

    Ok(Json(UpdateConfigResponse {
        plugin_id,
        host_id: host.host_id.clone(),
        values: body.values,
        reloaded: true,
        version: applied_version,
    }))
}

/// Restarts the host process that owns a plugin.
async fn restart_plugin(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
) -> Result<Json<RestartResponse>, ApiError> {
    // Checked before the host lookup: a disabled plugin has no host by design, and "enable it
    // first" is far more useful than "not loaded by any active host".
    if !state
        .plugin_state()
        .is_enabled(kanon_core::PLUGIN_SECTION, &plugin_id)
        .await
    {
        return Err(ApiError::Conflict(format!(
            "Plugin '{plugin_id}' is disabled; enable it before restarting its host"
        )));
    }

    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let host_id = host.host_id.clone();

    let restarted = state.supervisor().restart_host(&host_id).await?;

    state
        .observability()
        .events
        .publish(TraceEvent::PluginRestarted {
            host_id: host_id.clone(),
        });

    tracing::info!(host_id = %host_id, plugin_id = %plugin_id, "Plugin host restarted by control plane");

    Ok(Json(RestartResponse {
        host_id,
        plugins: plugin_views_for(&restarted),
    }))
}

/// Request body for invoking a plugin tool.
#[derive(Debug, Deserialize, Default)]
pub struct CallPluginToolRequest {
    /// Arguments payload passed to the tool.
    #[serde(default)]
    pub arguments: Value,
}

/// Response returned after executing a plugin tool.
#[derive(Debug, Serialize)]
pub struct CallPluginToolResponse {
    /// Whether the tool execution reported success.
    pub success: bool,
    /// Tool result payload.
    pub result: Value,
    /// Error message when tool execution failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Invokes a declared tool on a plugin over gRPC IPC.
async fn call_plugin_tool(
    State(state): State<ApiState>,
    AxumPath((plugin_id, tool_name)): AxumPath<(String, String)>,
    Json(body): Json<CallPluginToolRequest>,
) -> Result<Json<CallPluginToolResponse>, ApiError> {
    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let args_struct = json_to_prost_struct(&body.arguments).unwrap_or_default();
    let call_id = format!(
        "call-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let req = kanon_proto::v1::ToolCallRequest {
        call_id,
        tool_name,
        session_id: "api-tool-call".to_string(),
        payload: Some(kanon_proto::v1::tool_call_request::Payload::StructuredArgs(
            args_struct,
        )),
    };

    let response = host.on_call_tool(req).await.map_err(|status| {
        ApiError::Internal(format!("Tool execution failed: {}", status.message()))
    })?;

    let result = match response.payload {
        Some(kanon_proto::v1::tool_call_response::Payload::StructuredResult(res)) => {
            prost_struct_to_json(res)
        }
        _ => Value::Null,
    };

    Ok(Json(CallPluginToolResponse {
        success: response.success,
        result,
        error: if response.error_message.is_empty() {
            None
        } else {
            Some(response.error_message)
        },
    }))
}

/// Invokes a management action declared by a plugin.
///
/// Actions are the console counterpart of tools: unlike `tools/{name}`, the action is never
/// advertised to the LLM, so adapters can expose credential binding or diagnostics without the
/// model trying to call them during a conversation.
async fn invoke_plugin_action(
    State(state): State<ApiState>,
    AxumPath((plugin_id, action_name)): AxumPath<(String, String)>,
    Json(body): Json<CallPluginToolRequest>,
) -> Result<Json<CallPluginToolResponse>, ApiError> {
    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let parameters = json_to_prost_struct(&body.arguments).unwrap_or_default();
    let request = kanon_proto::v1::PluginActionRequest {
        plugin_id: plugin_id.clone(),
        action: action_name.clone(),
        parameters: Some(parameters),
    };

    let response = host.invoke_action(request).await.map_err(|status| {
        ApiError::Upstream(format!(
            "Plugin action '{action_name}' failed on host '{}': {}",
            host.host_id, status
        ))
    })?;

    if !response.success {
        tracing::warn!(
            plugin_id = %plugin_id,
            action = %action_name,
            error = %response.error_message,
            "Plugin management action reported a failure"
        );
    }

    Ok(Json(CallPluginToolResponse {
        success: response.success,
        result: response
            .result
            .map(prost_struct_to_json)
            .unwrap_or(Value::Null),
        error: if response.error_message.is_empty() {
            None
        } else {
            Some(response.error_message)
        },
    }))
}

/// Enables or disables a plugin, starting or stopping its host process.
///
/// Disabling is deliberately destructive to the process: a stopped host releases its memory and
/// removes its pre-filters, commands, tools and platform adapter from the node, which is what an
/// operator means by "turn this plugin off". Enabling spawns the host from the manifest on disk.
///
/// The recorded intent is *not* rolled back when the launch fails (for example a missing runtime):
/// the failure is reported as an upstream error and the plugin stays `enabled` while showing up as
/// declared, so a transient cause can be fixed and the next node start brings it up. Silently
/// reverting the toggle would erase what the operator asked for.
async fn set_plugin_enabled(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
    Json(body): Json<SetPluginEnabledRequest>,
) -> Result<Json<PluginStateResponse>, ApiError> {
    let running = state.supervisor().find_host_for_plugin(&plugin_id).await;
    let manifest_path = if running.is_none() {
        Some(find_manifest_path(state.plugins_dir(), &plugin_id)?)
    } else {
        None
    };

    if running.is_none() && manifest_path.is_none() {
        return Err(ApiError::NotFound(format!(
            "Plugin '{plugin_id}' is not loaded and no manifest for it exists on this node"
        )));
    }

    let changed = state
        .plugin_state()
        .set_enabled(kanon_core::PLUGIN_SECTION, &plugin_id, body.enabled)
        .await
        .map_err(ApiError::Internal)?;

    if !changed {
        return Ok(Json(PluginStateResponse {
            applied: false,
            message: if body.enabled {
                format!("Plugin '{plugin_id}' is already enabled")
            } else {
                format!("Plugin '{plugin_id}' is already disabled")
            },
            plugin_id,
            enabled: body.enabled,
            host_id: running.map(|host| host.host_id.clone()),
        }));
    }

    if body.enabled {
        let host_id = match running {
            Some(host) => host.host_id.clone(),
            None => {
                let manifest_path = manifest_path.expect("checked above");
                let host = state
                    .supervisor()
                    .spawn_from_manifest(&manifest_path, None)
                    .await
                    .map_err(|err| {
                        ApiError::Upstream(format!(
                            "Plugin '{plugin_id}' was enabled but its host failed to start: {err}"
                        ))
                    })?;
                host.host_id.clone()
            }
        };

        tracing::info!(plugin_id = %plugin_id, host_id = %host_id, "Plugin enabled by the control plane");
        Ok(Json(PluginStateResponse {
            applied: true,
            message: format!("Plugin '{plugin_id}' enabled and running"),
            plugin_id,
            enabled: true,
            host_id: Some(host_id),
        }))
    } else {
        // Stop every host that declares the plugin. Plugin hosts are either registered as
        // `host_<plugin_id>` or externally attached, so look the host up by declaration.
        let host_id = match state.supervisor().find_host_for_plugin(&plugin_id).await {
            Some(host) => {
                let host_id = host.host_id.clone();
                state
                    .supervisor()
                    .stop_host(&host_id)
                    .await
                    .map_err(|err| ApiError::Internal(format!("Failed to stop host: {err}")))?;
                Some(host_id)
            }
            None => None,
        };

        tracing::info!(
            plugin_id = %plugin_id,
            host_id = ?host_id,
            "Plugin disabled by the control plane; its host was stopped"
        );
        Ok(Json(PluginStateResponse {
            applied: true,
            message: format!("Plugin '{plugin_id}' disabled and its host stopped"),
            plugin_id,
            enabled: false,
            host_id,
        }))
    }
}

/// Resolves the manifest path of a plugin that is present on disk but not running.
fn find_manifest_path(
    plugins_dir: &std::path::Path,
    plugin_id: &str,
) -> Result<std::path::PathBuf, ApiError> {
    let discovered = kanon_core::PluginScanner::scan(plugins_dir).map_err(|err| {
        ApiError::Internal(format!("Could not scan {}: {err}", plugins_dir.display()))
    })?;

    discovered
        .into_iter()
        .find(|plugin| plugin.manifest.plugin.id == plugin_id)
        .map(|plugin| plugin.manifest_path)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded and no manifest for it exists on this node"
            ))
        })
}

/// Handles plugin installation from a local directory path or uploaded distribution archive.
async fn install_plugin(
    State(state): State<ApiState>,
    req: axum::extract::Request,
) -> Result<Json<InstallPluginResponse>, ApiError> {
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.starts_with("multipart/form-data") {
        let mut multipart = axum::extract::Multipart::from_request(req, &state)
            .await
            .map_err(|err| ApiError::BadRequest(format!("Invalid multipart payload: {err}")))?;

        let mut archive_bytes: Option<Vec<u8>> = None;
        let mut path_str: Option<String> = None;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?
        {
            let name = field.name().unwrap_or("").to_string();
            let file_name = field.file_name().map(ToString::to_string);

            if name == "path" {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
                if !text.trim().is_empty() {
                    path_str = Some(text.trim().to_string());
                }
            } else if name == "file"
                || file_name
                    .as_ref()
                    .is_some_and(|f| f.ends_with(".kpk") || f.ends_with(".zip"))
            {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
                archive_bytes = Some(bytes.to_vec());
            }
        }

        if let Some(bytes) = archive_bytes {
            let response = install_from_archive(&state, &bytes).await?;
            Ok(Json(response))
        } else if let Some(path) = path_str {
            let response = install_from_path(&state, Path::new(&path)).await?;
            Ok(Json(response))
        } else {
            Err(ApiError::BadRequest(
                "Multipart form must contain either 'file' (.kpk/.zip) or 'path'".to_string(),
            ))
        }
    } else if content_type.is_empty() || content_type.contains("application/json") {
        let bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
            .await
            .map_err(|e| ApiError::BadRequest(format!("Failed to read request body: {e}")))?;
        let payload: InstallPathRequest = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::BadRequest(format!("Invalid JSON request body: {e}")))?;
        let response = install_from_path(&state, Path::new(&payload.path)).await?;
        Ok(Json(response))
    } else {
        Err(ApiError::BadRequest(format!(
            "Unsupported Content-Type: '{content_type}'. Expected application/json or multipart/form-data"
        )))
    }
}

/// Installs a plugin from a local filesystem path.
async fn install_from_path(
    state: &ApiState,
    source_path: &Path,
) -> Result<InstallPluginResponse, ApiError> {
    if !source_path.exists() {
        return Err(ApiError::BadRequest(format!(
            "Source path does not exist: {}",
            source_path.display()
        )));
    }

    let manifest_file =
        if source_path.is_file() && source_path.file_name().is_some_and(|n| n == "plugin.toml") {
            source_path.to_path_buf()
        } else if source_path.is_dir() {
            let candidate = source_path.join("plugin.toml");
            if candidate.is_file() {
                candidate
            } else if let Some(found) = PluginScanner::find_manifest_in_dir(source_path) {
                found
            } else {
                return Err(ApiError::BadRequest(format!(
                    "No plugin.toml found in directory: {}",
                    source_path.display()
                )));
            }
        } else {
            return Err(ApiError::BadRequest(format!(
                "Source path is neither a directory nor a plugin.toml file: {}",
                source_path.display()
            )));
        };

    let manifest = PluginManifest::load_from_file(&manifest_file)
        .map_err(|e| ApiError::BadRequest(format!("Invalid plugin manifest: {e}")))?;

    PluginId::validate(&manifest.plugin.id)
        .map_err(|e| ApiError::BadRequest(format!("Invalid plugin identifier: {e}")))?;

    let plugin_source_dir = manifest_file.parent().unwrap_or(source_path);

    // Target installation directory: <plugins_root>/<plugin_id>
    let plugins_root = state.plugins_dir().to_path_buf();
    std::fs::create_dir_all(&plugins_root).map_err(|e| ApiError::Internal(e.to_string()))?;
    let dest_dir = plugins_root.join(&manifest.plugin.id);

    let is_same =
        if let (Ok(c1), Ok(c2)) = (plugin_source_dir.canonicalize(), dest_dir.canonicalize()) {
            c1 == c2
        } else {
            false
        };

    let host_id = manifest.plugin.id.replace('.', "_");
    // Terminate existing host if running
    let _ = state.supervisor().stop_host(&host_id).await;

    if !is_same {
        if dest_dir.exists() {
            let _ = std::fs::remove_dir_all(&dest_dir);
        }
        copy_dir_recursive(plugin_source_dir, &dest_dir).map_err(|e| {
            ApiError::Internal(format!("Failed to copy plugin to target directory: {e}"))
        })?;
    }

    ensure_entrypoint_executable(&dest_dir, &manifest.plugin.entrypoint);

    let target_manifest_path = dest_dir.join("plugin.toml");
    let spawn_result = state
        .supervisor()
        .spawn_from_manifest(&target_manifest_path, None)
        .await;

    match spawn_result {
        Ok(host) => {
            state
                .observability()
                .events
                .publish(TraceEvent::PluginInstalled {
                    plugin_id: manifest.plugin.id.clone(),
                    host_id: host.host_id.clone(),
                });

            let p_views = plugin_views_for(&host);
            let (commands, tools) = p_views
                .into_iter()
                .find(|p| p.id == manifest.plugin.id)
                .map(|p| (p.commands, p.tools))
                .unwrap_or_else(|| {
                    let declared = plugin_view_from_manifest_with_status(&manifest, "running");
                    (declared.commands, declared.tools)
                });

            tracing::info!(
                plugin_id = %manifest.plugin.id,
                host_id = %host.host_id,
                "Plugin installed and host spawned successfully"
            );

            Ok(InstallPluginResponse {
                plugin_id: manifest.plugin.id.clone(),
                name: manifest.plugin.name.clone(),
                version: manifest.plugin.version.clone(),
                runtime: manifest.plugin.runtime.clone(),
                commands,
                tools,
                status: "running".to_string(),
                message: Some("Plugin installed and launched successfully".to_string()),
            })
        }
        Err(SupervisorError::RuntimeUnavailable { runtime, reason }) => {
            tracing::warn!(
                plugin_id = %manifest.plugin.id,
                runtime = %runtime,
                reason = %reason,
                "Plugin installed but runtime is unavailable"
            );

            let declared = plugin_view_from_manifest_with_status(&manifest, "RuntimeUnavailable");
            Ok(InstallPluginResponse {
                plugin_id: manifest.plugin.id.clone(),
                name: manifest.plugin.name.clone(),
                version: manifest.plugin.version.clone(),
                runtime,
                commands: declared.commands,
                tools: declared.tools,
                status: "RuntimeUnavailable".to_string(),
                message: Some(format!(
                    "Plugin installed but runtime is unavailable: {reason}"
                )),
            })
        }
        Err(err) => Err(ApiError::BadRequest(format!(
            "Failed to launch plugin host: {err}"
        ))),
    }
}

/// Installs a plugin from raw archive bytes (.kpk or .zip).
async fn install_from_archive(
    state: &ApiState,
    archive_bytes: &[u8],
) -> Result<InstallPluginResponse, ApiError> {
    if archive_bytes.len() < 22 {
        return Err(ApiError::BadRequest(
            "Uploaded archive file is too small or corrupt".to_string(),
        ));
    }

    let temp_dir = tempfile::tempdir().map_err(|e| ApiError::Internal(e.to_string()))?;
    let cursor = std::io::Cursor::new(archive_bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| ApiError::BadRequest(format!("Failed to parse ZIP archive: {e}")))?;

    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;

        let enclosed = match file.enclosed_name() {
            Some(p) => p.to_owned(),
            None => {
                return Err(ApiError::BadRequest(
                    "Archive contains forbidden or malicious relative path".to_string(),
                ));
            }
        };

        let out_path = temp_dir.path().join(&enclosed);
        if file.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| ApiError::Internal(e.to_string()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ApiError::Internal(e.to_string()))?;
            }
            let mut outfile =
                std::fs::File::create(&out_path).map_err(|e| ApiError::Internal(e.to_string()))?;
            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| ApiError::Internal(e.to_string()))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let _ =
                        std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
                }
            }
        }
    }

    let source_dir = if temp_dir.path().join("plugin.toml").is_file() {
        temp_dir.path().to_path_buf()
    } else if let Some(manifest_path) = PluginScanner::find_manifest_in_dir(temp_dir.path()) {
        manifest_path
            .parent()
            .unwrap_or(temp_dir.path())
            .to_path_buf()
    } else {
        return Err(ApiError::BadRequest(
            "Uploaded archive does not contain a valid plugin.toml file".to_string(),
        ));
    };

    install_from_path(state, &source_dir).await
}

/// Recursively copies directory contents, skipping transient development and cache directories.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if name_str.starts_with('.')
            || name_str == "node_modules"
            || name_str == "target"
            || name_str == ".venv"
            || name_str == "venv"
            || name_str == "__pycache__"
        {
            continue;
        }

        let src_child = entry.path();
        let dst_child = dst.join(file_name);

        if file_type.is_dir() {
            copy_dir_recursive(&src_child, &dst_child)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_child, &dst_child)?;
        }
    }
    Ok(())
}

/// Sets Unix executable permission bits (0o755) on the declared entrypoint binary if present.
fn ensure_entrypoint_executable(plugin_dir: &Path, entrypoint: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exec_path = plugin_dir.join(entrypoint);
        if exec_path.is_file() {
            if let Ok(metadata) = std::fs::metadata(&exec_path) {
                let mut permissions = metadata.permissions();
                let mode = permissions.mode();
                permissions.set_mode(mode | 0o755);
                let _ = std::fs::set_permissions(&exec_path, permissions);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (plugin_dir, entrypoint);
    }
}

/// Builds the host view for a supervised process.
fn host_view(host: &ManagedHost, pid: Option<u32>, plugins: Vec<PluginView>) -> HostView {
    HostView {
        host_id: host.host_id.clone(),
        status: "running".to_string(),
        runtime: host
            .manifest()
            .map(|manifest| manifest.plugin.runtime.clone()),
        socket_path: host.socket_path.to_string_lossy().to_string(),
        priority: host.priority,
        plugin_ids: host.metas().iter().map(|meta| meta.id.clone()).collect(),
        restartable: host.launch_spec().is_some(),
        pid,
        plugins,
    }
}

/// Builds plugin views for a host, merging live metadata with the static manifest.
fn plugin_views_for(host: &ManagedHost) -> Vec<PluginView> {
    let manifest = host.manifest();

    let mut views: Vec<PluginView> = host
        .metas()
        .iter()
        .map(|meta| plugin_view_from_meta(meta, host, manifest))
        .collect();

    // A host that declares a manifest but has not reported its plugin (or reports it later)
    // still appears in the catalog as `declared`, so consoles never hide a configured plugin.
    if let Some(manifest) = manifest
        && !views.iter().any(|view| view.id == manifest.plugin.id)
    {
        views.push(plugin_view_from_manifest(host, manifest));
    }

    views
}

/// Builds a plugin view from live process metadata.
fn plugin_view_from_meta(
    meta: &PluginMeta,
    host: &ManagedHost,
    manifest: Option<&kanon_core::PluginManifest>,
) -> PluginView {
    let fallback = manifest.filter(|m| m.plugin.id == meta.id);

    PluginView {
        id: meta.id.clone(),
        name: non_empty_or(meta.name.clone(), || {
            fallback.map(|m| m.plugin.name.clone()).unwrap_or_default()
        }),
        version: non_empty_or(meta.version.clone(), || {
            fallback
                .map(|m| m.plugin.version.clone())
                .unwrap_or_default()
        }),
        author: non_empty_or(meta.author.clone(), || {
            fallback
                .and_then(|m| m.plugin.author.clone())
                .unwrap_or_default()
        }),
        description: non_empty_or(meta.description.clone(), || {
            fallback
                .and_then(|m| m.plugin.description.clone())
                .unwrap_or_default()
        }),
        host_id: host.host_id.clone(),
        runtime: fallback.map(|m| m.plugin.runtime.clone()),
        priority: host.priority,
        status: "running".to_string(),
        enabled: true,
        health: None,
        commands: meta
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone(),
                usage: command.usage.clone(),
                priority: command.priority,
            })
            .collect(),
        tools: meta
            .tools
            .iter()
            .map(|tool| ToolView {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool
                    .parameters
                    .clone()
                    .map(prost_struct_to_json)
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
            .collect(),
    }
}

/// Builds a plugin view from the static manifest alone.
fn plugin_view_from_manifest(
    host: &ManagedHost,
    manifest: &kanon_core::PluginManifest,
) -> PluginView {
    PluginView {
        id: manifest.plugin.id.clone(),
        name: manifest.plugin.name.clone(),
        version: manifest.plugin.version.clone(),
        author: manifest.plugin.author.clone().unwrap_or_default(),
        description: manifest.plugin.description.clone().unwrap_or_default(),
        host_id: host.host_id.clone(),
        runtime: Some(manifest.plugin.runtime.clone()),
        priority: host.priority,
        status: "declared".to_string(),
        enabled: true,
        health: None,
        commands: manifest
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone().unwrap_or_default(),
                usage: command.usage.clone().unwrap_or_default(),
                priority: command.priority.unwrap_or(500),
            })
            .collect(),
        tools: manifest
            .tools
            .iter()
            .map(|tool| ToolView {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                parameters: tool
                    .parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
            .collect(),
    }
}

/// Builds a plugin view from a manifest with an explicit lifecycle status.
fn plugin_view_from_manifest_with_status(
    manifest: &kanon_core::PluginManifest,
    status: &str,
) -> PluginView {
    let host_id = format!("host_{}", manifest.plugin.id.replace('.', "_"));
    PluginView {
        id: manifest.plugin.id.clone(),
        name: manifest.plugin.name.clone(),
        version: manifest.plugin.version.clone(),
        author: manifest.plugin.author.clone().unwrap_or_default(),
        description: manifest.plugin.description.clone().unwrap_or_default(),
        host_id,
        runtime: Some(manifest.plugin.runtime.clone()),
        priority: manifest.plugin.priority.unwrap_or(500),
        status: status.to_string(),
        enabled: true,
        health: None,
        commands: manifest
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone().unwrap_or_default(),
                usage: command.usage.clone().unwrap_or_default(),
                priority: command.priority.unwrap_or(500),
            })
            .collect(),
        tools: manifest
            .tools
            .iter()
            .map(|tool| ToolView {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                parameters: tool
                    .parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
            .collect(),
    }
}

/// Returns `value` when non-empty, otherwise the fallback.
fn non_empty_or(value: String, fallback: impl FnOnce() -> String) -> String {
    if value.is_empty() { fallback() } else { value }
}
