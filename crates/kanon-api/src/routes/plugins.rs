//! Plugin lifecycle and configuration control routes.
//!
//! # Design notes
//! - The **running process** is authoritative for plugin metadata: `GetPluginMeta` results are
//!   cached on [`kanon_core::ManagedHost`] at handshake time.
//! - The **static manifest** is authoritative for declaration-time facts the process does not
//!   report over gRPC, namely the `[config_schema]` JSON Schema used to render console forms.
//! - Configuration updates hold one plugin lock across *validate → persist → hot reload*.
//!   A rejected reload restores the previous file before releasing the lock; an uncertain RPC
//!   outcome also removes the host from routing. Reads use the same lock for values and version.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequest, Path as AxumPath, State};
use axum::routing::{get, post};
use kanon_core::{DiscoveredPlugin, LaunchSpec, ManagedHost, PluginManifest, UnavailablePlugin};
use kanon_llm::tool_router::{json_to_prost_struct, prost_struct_to_json};
use kanon_proto::v1::PluginMeta;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::observability::TraceEvent;
use crate::plugin_config::PluginConfigStore;
use crate::plugin_files::{has_pages, load_translations};
use crate::plugin_install::{self, InstallSource, MAX_PACKAGE_BYTES};
use crate::state::ApiState;

/// Registers all plugin management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        // The plugin directory is read only on request: listing serves the last scan, and this
        // route is what makes a folder copied in by hand show up.
        .route("/api/v1/plugins/rescan", post(rescan_plugins))
        // Uploaded packages exceed axum's 2 MB default body limit, so this route carries its own.
        .route(
            "/api/v1/plugins/install",
            post(install_plugin).layer(DefaultBodyLimit::max(install_body_limit())),
        )
        // Static segments (`rescan`, `install`, `market`) win over the parameter in the router,
        // so these never shadow each other.
        .route("/api/v1/plugins/:id", get(get_plugin))
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
    /// Lifecycle status after installation: `running`, `RuntimeUnavailable`, or `disabled` when
    /// the operator had switched the plugin off (a reinstall does not switch it back on).
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Project homepage declared by the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Source repository declared by the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Platforms the plugin was written for; empty means every platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Node versions the plugin supports, as the manifest's semver requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kanon_version: Option<String>,
    /// Whether the plugin ships console pages (`pages/index.html`), served under
    /// `/api/v1/plugins/<id>/pages/`.
    #[serde(default)]
    pub has_pages: bool,
    /// Whether the running plugin serves HTTP routes under `/api/v1/plugins/<id>/http/`.
    /// Only a live host reports this, so it is `false` while the plugin is not running.
    #[serde(default)]
    pub serves_http: bool,
    /// Display-text translations from `i18n/<locale>.json`, keyed by locale tag; the console
    /// picks the one matching its language and falls back to the manifest text.
    #[serde(default)]
    pub i18n: BTreeMap<String, BTreeMap<String, String>>,
    /// Problems found in the translation files, one sentence each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub i18n_errors: Vec<String>,
}

/// Where a plugin's files live on this node.
#[derive(Debug, Clone)]
pub(crate) struct PluginLocation {
    /// Plugin folder (the directory holding `plugin.toml`).
    pub dir: PathBuf,
    /// Manifest read from that folder, when known.
    pub manifest: Option<PluginManifest>,
}

/// Finds the folder of an installed plugin.
///
/// The running host's launch recipe is checked first, since that is the copy actually serving
/// the plugin; then the last directory scan (ignoring entries whose folder was deleted since);
/// then plugins recorded as unavailable because their runtime is missing.
fn locate_plugin(
    plugin_id: &str,
    hosts: &[std::sync::Arc<ManagedHost>],
    on_disk: &[DiscoveredPlugin],
    unavailable: &[UnavailablePlugin],
) -> Option<PluginLocation> {
    let from_host = hosts
        .iter()
        .filter(|host| host.metas().iter().any(|meta| meta.id == plugin_id))
        .chain(hosts.iter().filter(|host| {
            host.manifest()
                .is_some_and(|manifest| manifest.plugin.id == plugin_id)
        }))
        .find_map(|host| match host.launch_spec() {
            Some(LaunchSpec::Manifest { manifest_path, .. }) => {
                manifest_path.parent().map(|dir| PluginLocation {
                    dir: dir.to_path_buf(),
                    manifest: host.manifest().cloned(),
                })
            }
            _ => None,
        });
    if from_host.is_some() {
        return from_host;
    }

    if let Some(plugin) = on_disk
        .iter()
        .find(|plugin| plugin.manifest.plugin.id == plugin_id && plugin.manifest_path.is_file())
    {
        return Some(PluginLocation {
            dir: plugin.plugin_dir.clone(),
            manifest: Some(plugin.manifest.clone()),
        });
    }

    unavailable
        .iter()
        .find(|plugin| plugin.manifest.plugin.id == plugin_id)
        .and_then(|plugin| {
            plugin.manifest_path.parent().map(|dir| PluginLocation {
                dir: dir.to_path_buf(),
                manifest: Some(plugin.manifest.clone()),
            })
        })
}

/// Finds the folder of an installed plugin (see [`locate_plugin`]).
pub(crate) async fn plugin_location(state: &ApiState, plugin_id: &str) -> Option<PluginLocation> {
    let hosts = state.supervisor().get_all_hosts().await;
    let unavailable = state.supervisor().get_unavailable_plugins().await;
    locate_plugin(plugin_id, &hosts, &state.plugins_on_disk(), &unavailable)
}

/// Fills the facts that come from the plugin folder: manifest links, pages and translations.
fn apply_location(view: &mut PluginView, location: Option<&PluginLocation>) {
    let Some(location) = location else {
        return;
    };
    if let Some(manifest) = &location.manifest {
        view.homepage = manifest.plugin.homepage.clone();
        view.repository = manifest.plugin.repository.clone();
        view.platforms = manifest.plugin.platforms.clone();
        view.kanon_version = manifest.plugin.kanon_version.clone();
    }
    view.has_pages = has_pages(&location.dir);
    let translations = load_translations(&location.dir);
    view.i18n = translations.locales;
    view.i18n_errors = translations.errors;
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
async fn list_plugins(State(state): State<ApiState>) -> Result<Json<PluginCatalog>, ApiError> {
    Ok(Json(catalog(&state).await?))
}

/// Returns one plugin as the catalog presents it, including its translations.
async fn get_plugin(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
) -> Result<Json<PluginView>, ApiError> {
    catalog(&state)
        .await?
        .plugins
        .into_iter()
        .find(|view| view.id == plugin_id)
        .map(Json)
        .ok_or_else(|| {
            ApiError::NotFound(format!("Plugin '{plugin_id}' is not known to this node"))
        })
}

/// Assembles the catalog of hosts and plugins.
async fn catalog(state: &ApiState) -> Result<PluginCatalog, ApiError> {
    let hosts = state.supervisor().get_all_hosts().await;

    let mut host_views = Vec::with_capacity(hosts.len());
    let mut plugin_views = Vec::new();

    for host in &hosts {
        let p_views = plugin_views_for(host)?;
        let pid = host.pid().await;
        host_views.push(host_view(host, pid, p_views.clone()));
        plugin_views.extend(p_views);
    }

    // Include any plugins recorded as unavailable due to missing runtime environments
    let unavailable = state.supervisor().get_unavailable_plugins().await;
    let on_disk = state.plugins_on_disk();
    for unavail in unavailable.iter().cloned() {
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
    // console could never re-enable one. Present the rest of the last directory scan (with a
    // placeholder status: the overlay below decides the final one).
    for on_disk in on_disk_plugin_views(on_disk.clone(), &plugin_views) {
        plugin_views.push(on_disk);
    }

    // Facts read from the plugin folder: manifest links, pages and translations.
    for view in &mut plugin_views {
        let location = locate_plugin(&view.id, &hosts, &on_disk, &unavailable);
        apply_location(view, location.as_ref());
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
                *plugin = resolved.clone();
            }
        }
    }

    // Deterministic ordering keeps console tables stable across polls.
    host_views.sort_by(|a, b| a.host_id.cmp(&b.host_id));
    plugin_views.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(PluginCatalog {
        total: plugin_views.len(),
        hosts: host_views,
        plugins: plugin_views,
    })
}

/// Rescans the plugin directory on the operator's request and answers with the fresh catalog.
///
/// A scan that fails is reported rather than answered with an empty list, which would look like
/// every stopped plugin had been deleted.
async fn rescan_plugins(State(state): State<ApiState>) -> Result<Json<PluginCatalog>, ApiError> {
    let found = state.rescan_plugins().map_err(|err| {
        ApiError::Internal(format!(
            "Could not scan {}: {err}",
            state.plugins_dir().display()
        ))
    })?;
    tracing::info!(
        count = found.len(),
        dir = %state.plugins_dir().display(),
        "Plugin directory rescanned on request"
    );
    Ok(Json(catalog(&state).await?))
}

/// Builds catalog entries for plugins on disk that have no running host.
///
/// Without this fill the console would lose the ability to re-enable a plugin it had disabled
/// (and could not see a plugin whose runtime is missing, either), because both cases have no
/// host process to enumerate. The caller applies the enable/disable overlay afterwards.
fn on_disk_plugin_views(
    on_disk: Vec<DiscoveredPlugin>,
    existing: &[PluginView],
) -> Vec<PluginView> {
    on_disk
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
    let transaction = state.supervisor().lock_plugin_config(&plugin_id).await;
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
    let version = transaction.version();

    Ok(Json(PluginConfigView {
        plugin_id,
        values,
        schema: schema.unwrap_or(Value::Null),
        persisted,
        version,
    }))
}

/// Completes a configuration commit even if its HTTP caller disconnects during the host RPC.
async fn put_config(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
    Json(body): Json<UpdateConfigRequest>,
) -> Result<Json<UpdateConfigResponse>, ApiError> {
    // Reserve the transaction before detaching it. A caller cancelled while queued has made
    // no change, and shutdown can discover every detached commit through its held guard.
    let transaction = state.supervisor().lock_plugin_config(&plugin_id).await;
    tokio::spawn(async move {
        let result = commit_plugin_config(&state, &plugin_id, body, transaction).await;
        // A disconnected caller no longer consumes the response, so transaction failures must
        // be logged by the task that owns the lock and rollback, not just by IntoResponse.
        if let Err(error) = &result {
            tracing::error!(plugin_id = %plugin_id, %error, "Plugin configuration commit failed");
        }
        result.map(Json)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("Configuration task failed: {error}")))?
}

/// Saves and applies one candidate under the same lock used by readers and lifecycle routes.
async fn commit_plugin_config(
    state: &ApiState,
    plugin_id: &str,
    body: UpdateConfigRequest,
    mut transaction: kanon_core::supervisor::PluginConfigGuard,
) -> Result<UpdateConfigResponse, ApiError> {
    if !body.values.is_object() {
        return Err(ApiError::BadRequest(
            "Field 'values' must be a JSON object".to_string(),
        ));
    }

    transaction.check_version(body.version)?;
    let host = state
        .supervisor()
        .find_host_for_plugin(plugin_id)
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

    // Persist before changing the host: a full disk or read-only directory must not change live
    // behavior. Retain the exact previous file until the host accepts, including its absence.
    let store = state.config_store().clone();
    let values = body.values.clone();
    let persist_id = plugin_id.to_string();
    let backup = tokio::task::spawn_blocking(move || store.replace(&persist_id, &values))
        .await
        .map_err(|err| ApiError::Internal(format!("Persistence task failed: {err}")))??;

    let applied_version = match transaction.reload(&host, &body.values).await {
        Ok(version) => version,
        Err(error) => {
            let restored = tokio::task::spawn_blocking(move || backup.restore())
                .await
                .map_err(|err| ApiError::Internal(format!("Restoration task failed: {err}")))
                .and_then(std::convert::identity);
            let rejected = matches!(
                &error,
                kanon_core::SupervisorError::ConfigReloadRejected { .. }
            );
            if !rejected || restored.is_err() {
                // A lost RPC response cannot prove which values the host accepted. Restoring a
                // file alone is insufficient: remove this host until a restart reads that file.
                let stopped = state.supervisor().stop_host(&host.host_id).await;
                if let Err(restore_error) = restored {
                    return Err(ApiError::Internal(format!(
                        "{error}; previous configuration could not be restored: {restore_error}; \
                         host stop result: {stopped:?}. Inspect the saved configuration before restarting"
                    )));
                }
                stopped?;
                return Err(ApiError::Upstream(format!(
                    "{error}; the previous configuration was restored and the host was removed \
                     from routing because its applied configuration could not be confirmed"
                )));
            }
            return Err(error.into());
        }
    };

    state
        .observability()
        .events
        .publish(TraceEvent::PluginConfigUpdated {
            plugin_id: plugin_id.to_string(),
            host_id: host.host_id.clone(),
        });

    tracing::info!(
        plugin_id = %plugin_id,
        host_id = %host.host_id,
        version = applied_version,
        "Plugin configuration updated and hot reloaded"
    );

    Ok(UpdateConfigResponse {
        plugin_id: plugin_id.to_string(),
        host_id: host.host_id.clone(),
        values: body.values,
        reloaded: true,
        version: applied_version,
    })
}

/// Restarts the host process that owns a plugin, or starts it when no host runs it.
///
/// A plugin whose host crashed or failed to start is still installed and enabled; "restart" then
/// means starting it from its manifest on disk, exactly as enabling it would. That is what lets
/// `kanon-dev dev` recover once a broken edit is fixed.
async fn restart_plugin(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
) -> Result<Json<RestartResponse>, ApiError> {
    let _configuration = state.supervisor().lock_plugin_config(&plugin_id).await;
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

    let restarted = match state.supervisor().find_host_for_plugin(&plugin_id).await {
        Some(host) => state.supervisor().restart_host(&host.host_id).await?,
        None => {
            let manifest_path = find_manifest_path(&state, &plugin_id)?;
            state
                .supervisor()
                .spawn_from_manifest(&manifest_path, None)
                .await
                .map_err(|err| {
                    ApiError::Upstream(format!(
                        "Plugin '{plugin_id}' is not running and its host failed to start: {err}"
                    ))
                })?
        }
    };
    let host_id = restarted.host_id.clone();

    state
        .observability()
        .events
        .publish(TraceEvent::PluginRestarted {
            host_id: host_id.clone(),
        });

    tracing::info!(host_id = %host_id, plugin_id = %plugin_id, "Plugin host restarted by control plane");

    Ok(Json(RestartResponse {
        host_id,
        plugins: plugin_views_for(&restarted)?,
    }))
}

/// Request body for invoking a plugin tool.
#[derive(Debug, Deserialize, Default)]
pub struct CallPluginToolRequest {
    /// Arguments payload passed to the tool.
    #[serde(default)]
    pub arguments: serde_json::Map<String, Value>,
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

    let args_struct = json_to_prost_struct(&Value::Object(body.arguments))
        .expect("the request arguments are a JSON object");
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
        // A console test call happens outside any platform conversation.
        context: None,
    };

    let response = host.on_call_tool(req).await.map_err(|status| {
        ApiError::Internal(format!("Tool execution failed: {}", status.message()))
    })?;

    let result = match response.payload {
        Some(kanon_proto::v1::tool_call_response::Payload::StructuredResult(res)) => {
            prost_struct_to_json(res)?
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

    let parameters = json_to_prost_struct(&Value::Object(body.arguments))
        .expect("the request arguments are a JSON object");
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
            .transpose()?
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
    let _configuration = state.supervisor().lock_plugin_config(&plugin_id).await;
    let running = state.supervisor().find_host_for_plugin(&plugin_id).await;
    let manifest_path = if running.is_none() {
        Some(find_manifest_path(&state, &plugin_id)?)
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

    // Persisted intent alone does not prove that the requested lifecycle action finished. A
    // failed launch keeps the plugin enabled, so another enable must retry the missing host.
    if !changed && running.is_some() == body.enabled {
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

/// Resolves the manifest path of a plugin that the last directory scan found but is not running.
fn find_manifest_path(state: &ApiState, plugin_id: &str) -> Result<std::path::PathBuf, ApiError> {
    state
        .plugins_on_disk()
        .into_iter()
        .find(|plugin| plugin.manifest.plugin.id == plugin_id)
        .map(|plugin| plugin.manifest_path)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded and the last plugin directory scan did not \
                 find it; rescan the directory if it was added since"
            ))
        })
}

/// Request body for `POST /api/v1/plugins/install` as JSON.
///
/// Exactly one source must be given. Unknown fields are refused so a misspelled `replace` cannot
/// silently turn an intended upgrade into a `409`, or a misspelled source into "no source".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    /// Folder (or `plugin.toml`) on this node.
    #[serde(default)]
    pub path: Option<String>,
    /// `https` URL of a `.kpk` / `.zip` package.
    #[serde(default)]
    pub url: Option<String>,
    /// Git repository URL.
    #[serde(default)]
    pub git: Option<String>,
    /// Branch or tag to clone; only valid together with `git`.
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    /// Overwrite a plugin already installed under the same id.
    #[serde(default)]
    pub replace: bool,
}

impl InstallRequest {
    /// Turns the request into an install source, enforcing "exactly one source".
    fn into_source(self) -> Result<(InstallSource, bool), ApiError> {
        let non_empty = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let (path, url, git, git_ref) = (
            non_empty(self.path),
            non_empty(self.url),
            non_empty(self.git),
            non_empty(self.git_ref),
        );
        if git_ref.is_some() && git.is_none() {
            return Err(ApiError::BadRequest(
                "Field 'ref' is only valid together with 'git'".to_string(),
            ));
        }
        let source = match (path, url, git) {
            (Some(path), None, None) => InstallSource::Path(PathBuf::from(path)),
            (None, Some(url), None) => InstallSource::Url(url),
            (None, None, Some(url)) => InstallSource::Git { url, git_ref },
            (None, None, None) => {
                return Err(ApiError::BadRequest(
                    "Give the plugin to install as one of 'path', 'url' or 'git'".to_string(),
                ));
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "Give exactly one of 'path', 'url' or 'git'".to_string(),
                ));
            }
        };
        Ok((source, self.replace))
    }
}

/// Largest JSON install request; it only carries a path or a URL.
const MAX_INSTALL_JSON_BYTES: usize = 64 * 1024;

/// Body limit of the install route: a full package plus the multipart framing around it.
fn install_body_limit() -> usize {
    MAX_PACKAGE_BYTES as usize + 1024 * 1024
}

/// Installs a plugin from a local folder, an uploaded package, a package URL or a Git repository.
///
/// - `application/json`: [`InstallRequest`] (`path`, `url` or `git` + optional `ref`, `replace`).
/// - `multipart/form-data`: a `file` part holding the package (or a `path` part), plus an
///   optional `replace` part (`true` / `false`).
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

    let (source, replace) = if content_type.starts_with("multipart/form-data") {
        read_install_multipart(&state, req).await?
    } else if content_type.is_empty() || content_type.contains("application/json") {
        let bytes = axum::body::to_bytes(req.into_body(), MAX_INSTALL_JSON_BYTES)
            .await
            .map_err(|e| ApiError::BadRequest(format!("Failed to read request body: {e}")))?;
        let payload: InstallRequest = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::BadRequest(format!("Invalid JSON request body: {e}")))?;
        payload.into_source()?
    } else {
        return Err(ApiError::BadRequest(format!(
            "Unsupported Content-Type: '{content_type}'. Expected application/json or multipart/form-data"
        )));
    };

    Ok(Json(
        plugin_install::install(&state, source, replace).await?,
    ))
}

/// Reads a multipart install request (`file` or `path`, optional `replace`).
async fn read_install_multipart(
    state: &ApiState,
    req: axum::extract::Request,
) -> Result<(InstallSource, bool), ApiError> {
    // The route's `DefaultBodyLimit` bounds the whole body; going past it surfaces as a field
    // error with status 413, which is reported as such instead of as a malformed upload.
    let multipart_error = |err: axum::extract::multipart::MultipartError| {
        if err.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge(format!(
                "Plugin packages are limited to {MAX_PACKAGE_BYTES} bytes"
            ))
        } else {
            ApiError::BadRequest(format!("Invalid multipart payload: {}", err.body_text()))
        }
    };

    let mut multipart = axum::extract::Multipart::from_request(req, state)
        .await
        .map_err(|err| ApiError::BadRequest(format!("Invalid multipart payload: {err}")))?;

    let mut archive: Option<Vec<u8>> = None;
    let mut path: Option<String> = None;
    let mut replace = false;
    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or("").to_string();
        let is_package = field
            .file_name()
            .is_some_and(|file| file.ends_with(".kpk") || file.ends_with(".zip"));
        if name == "file" || is_package {
            archive = Some(field.bytes().await.map_err(multipart_error)?.to_vec());
        } else if name == "path" {
            let text = field.text().await.map_err(multipart_error)?;
            path = Some(text.trim().to_string()).filter(|text| !text.is_empty());
        } else if name == "replace" {
            let text = field.text().await.map_err(multipart_error)?;
            replace = match text.trim() {
                "true" | "1" => true,
                "false" | "0" | "" => false,
                other => {
                    return Err(ApiError::BadRequest(format!(
                        "Field 'replace' must be 'true' or 'false', not '{other}'"
                    )));
                }
            };
        } else {
            return Err(ApiError::BadRequest(format!(
                "Unknown multipart field '{name}'; expected 'file', 'path' or 'replace'"
            )));
        }
    }

    match (archive, path) {
        (Some(bytes), None) => Ok((InstallSource::Archive(bytes), replace)),
        (None, Some(path)) => Ok((InstallSource::Path(PathBuf::from(path)), replace)),
        (Some(_), Some(_)) => Err(ApiError::BadRequest(
            "Send either 'file' or 'path', not both".to_string(),
        )),
        (None, None) => Err(ApiError::BadRequest(
            "Multipart form must contain either 'file' (.kpk/.zip) or 'path'".to_string(),
        )),
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
pub(crate) fn plugin_views_for(host: &ManagedHost) -> Result<Vec<PluginView>, ApiError> {
    let manifest = host.manifest();

    let mut views: Vec<PluginView> = host
        .metas()
        .iter()
        .map(|meta| plugin_view_from_meta(meta, host, manifest))
        .collect::<Result<_, _>>()?;

    // A host that declares a manifest but has not reported its plugin (or reports it later)
    // still appears in the catalog as `declared`, so consoles never hide a configured plugin.
    if let Some(manifest) = manifest
        && !views.iter().any(|view| view.id == manifest.plugin.id)
    {
        views.push(plugin_view_from_manifest(host, manifest));
    }

    Ok(views)
}

/// Builds a plugin view from live process metadata.
fn plugin_view_from_meta(
    meta: &PluginMeta,
    host: &ManagedHost,
    manifest: Option<&kanon_core::PluginManifest>,
) -> Result<PluginView, ApiError> {
    let fallback = manifest.filter(|m| m.plugin.id == meta.id);

    Ok(PluginView {
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
            .map(|tool| {
                Ok(ToolView {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool
                        .parameters
                        .clone()
                        .map(prost_struct_to_json)
                        .transpose()?
                        .unwrap_or_else(|| json!({ "type": "object" })),
                })
            })
            .collect::<Result<_, ApiError>>()?,
        serves_http: meta.serves_http,
        ..PluginView::default()
    })
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
        ..PluginView::default()
    }
}

/// Builds a plugin view from a manifest with an explicit lifecycle status.
pub(crate) fn plugin_view_from_manifest_with_status(
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
        ..PluginView::default()
    }
}

/// Returns `value` when non-empty, otherwise the fallback.
fn non_empty_or(value: String, fallback: impl FnOnce() -> String) -> String {
    if value.is_empty() { fallback() } else { value }
}
