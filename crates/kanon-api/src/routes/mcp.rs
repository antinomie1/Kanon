//! MCP server management routes.
//!
//! The console edits *definitions* here; the pool turns them into live tool hosts. Writing a
//! definition re-syncs the pool immediately, so a server added in the UI is callable by the model
//! on the next turn without a node restart.
//!
//! # Two switches, two jobs
//! - The node-wide switch lives in the [`kanon_core::ToggleStore`] `mcp` section, exactly like
//!   plugins and skills. It is the only global enablement source; the stored definition carries no
//!   `enabled` flag, so there is never a second truth to reconcile.
//! - A bot instance may additionally restrict a server through its own policy map, enforced when
//!   the pipeline assembles the tool list.

use axum::Json;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, put};
use kanon_core::{MCP_SECTION, McpHealth, McpServerConfig, McpTransport};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers all MCP management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/mcp/servers", get(list_servers))
        .route(
            "/api/v1/mcp/servers/:id",
            put(upsert_server).delete(remove_server),
        )
        .route("/api/v1/mcp/servers/:id/enabled", put(set_server_enabled))
}

/// One MCP server as the console sees it.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerView {
    /// Stable identifier, also used in tool names (`mcp__<id>__<tool>`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Transport configuration verbatim, so the console can round-trip an edit.
    pub transport: McpTransport,
    /// Owning host identifier used in tool metadata.
    pub host_id: String,
    /// Whether the server is enabled node-wide.
    pub enabled: bool,
    /// Live connection state and advertised tool count.
    pub health: McpHealth,
}

/// Response of the MCP server catalog.
#[derive(Debug, Serialize)]
pub struct McpCatalog {
    /// Configured servers, ordered by identifier.
    pub servers: Vec<McpServerView>,
}

/// Request body creating or replacing a server definition.
#[derive(Debug, Deserialize)]
pub struct UpsertServerRequest {
    /// Human-readable name; defaults to the identifier.
    #[serde(default)]
    pub name: Option<String>,
    /// How to reach the server.
    pub transport: McpTransport,
}

/// Request body toggling one server node-wide.
#[derive(Debug, Deserialize)]
pub struct SetEnabledRequest {
    /// Desired state.
    pub enabled: bool,
}

/// Confirmation of an MCP state change.
#[derive(Debug, Serialize)]
pub struct McpStateResponse {
    /// Whether the requested change took effect (false when already in that state).
    pub applied: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Identifier of the affected server.
    pub server_id: String,
    /// Resulting node-wide state.
    pub enabled: bool,
}

/// Lists configured servers with their node-wide switch and live health.
async fn list_servers(State(state): State<ApiState>) -> Result<Json<McpCatalog>, ApiError> {
    // The pool is synced at startup and on every edit; syncing here as well would make a read
    // mutate connection state, so this reports exactly what the node currently holds.
    let described = state.mcp().describe().await;

    let mut servers = Vec::with_capacity(described.len());
    for (config, health) in described {
        let enabled = state
            .plugin_state()
            .is_enabled(MCP_SECTION, &config.id)
            .await;
        servers.push(McpServerView {
            host_id: kanon_core::mcp::host_id(&config.id),
            id: config.id,
            name: config.name,
            transport: config.transport,
            enabled,
            health,
        });
    }

    Ok(Json(McpCatalog { servers }))
}

/// Creates or replaces a server definition and re-syncs the pool.
async fn upsert_server(
    State(state): State<ApiState>,
    AxumPath(server_id): AxumPath<String>,
    Json(body): Json<UpsertServerRequest>,
) -> Result<Json<McpServerView>, ApiError> {
    let id = validate_server_id(&server_id)?;
    let name = body
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.clone());

    let config = McpServerConfig {
        id: id.clone(),
        name,
        transport: body.transport,
    };

    state
        .mcp_config()
        .upsert(config)
        .await
        .map_err(|err| ApiError::BadRequest(err.to_string()))?;
    state.mcp().sync_from_config(state.mcp_config()).await;

    // Bring enabled definitions up right away: reporting "ok" while the server is unreachable would
    // hide the failure until the model's first tool call.
    let view = match state.mcp().get(&id).await {
        Some(server) => {
            // Editing a definition must preserve the operator's switch. A disabled server must
            // stay disconnected even when the edit creates a fresh pool entry.
            if state.plugin_state().is_enabled(MCP_SECTION, &id).await {
                if let Err(err) = server.connect().await {
                    tracing::warn!(server = %id, error = %err, "Configured MCP server could not be reached");
                }
            } else {
                server.disconnect().await;
            }
            view_of(&state, server.config().clone(), server.health().await).await
        }
        None => {
            return Err(ApiError::Internal(format!(
                "MCP server '{id}' was saved but is missing from the pool"
            )));
        }
    };

    tracing::info!(server = %id, "MCP server saved by the control plane");
    Ok(Json(view))
}

/// Removes a server definition, disconnecting it and forgetting its switch.
async fn remove_server(
    State(state): State<ApiState>,
    AxumPath(server_id): AxumPath<String>,
) -> Result<Json<McpStateResponse>, ApiError> {
    let id = validate_server_id(&server_id)?;
    if state.mcp_config().get(&id).await.is_none() {
        return Err(ApiError::NotFound(format!(
            "No MCP server '{id}' is configured on this node"
        )));
    }

    if let Some(server) = state.mcp().get(&id).await {
        server.disconnect().await;
    }
    state
        .mcp_config()
        .remove(&id)
        .await
        .map_err(|err| ApiError::Internal(err.to_string()))?;
    state.mcp().sync_from_config(state.mcp_config()).await;

    // Drop the recorded switch with the definition: keeping it would silently disable a future
    // server that happens to reuse the identifier.
    state
        .plugin_state()
        .set_enabled(MCP_SECTION, &id, true)
        .await
        .map_err(ApiError::Internal)?;

    tracing::info!(server = %id, "MCP server removed by the control plane");
    Ok(Json(McpStateResponse {
        applied: true,
        message: format!("MCP server '{id}' removed"),
        server_id: id,
        enabled: false,
    }))
}

/// Enables or disables one server node-wide.
///
/// Disabling disconnects at once so a disabled server releases its child process or socket instead
/// of lingering until the next watchdog pass.
async fn set_server_enabled(
    State(state): State<ApiState>,
    AxumPath(server_id): AxumPath<String>,
    Json(body): Json<SetEnabledRequest>,
) -> Result<Json<McpStateResponse>, ApiError> {
    let id = validate_server_id(&server_id)?;
    if state.mcp_config().get(&id).await.is_none() {
        return Err(ApiError::NotFound(format!(
            "No MCP server '{id}' is configured on this node"
        )));
    }

    let changed = state
        .plugin_state()
        .set_enabled(MCP_SECTION, &id, body.enabled)
        .await
        .map_err(ApiError::Internal)?;

    match state.mcp().get(&id).await {
        Some(server) if body.enabled => {
            if let Err(err) = server.connect().await {
                tracing::warn!(server = %id, error = %err, "Enabled MCP server could not be reached");
            }
        }
        Some(server) => server.disconnect().await,
        None => {}
    }

    Ok(Json(McpStateResponse {
        applied: changed,
        message: if !changed {
            format!(
                "MCP server '{id}' is already {}",
                if body.enabled { "enabled" } else { "disabled" }
            )
        } else if body.enabled {
            format!("MCP server '{id}' enabled")
        } else {
            format!("MCP server '{id}' disabled")
        },
        server_id: id,
        enabled: body.enabled,
    }))
}

/// Builds a view from a live pool entry, resolving the node-wide switch.
async fn view_of(state: &ApiState, config: McpServerConfig, health: McpHealth) -> McpServerView {
    let enabled = state
        .plugin_state()
        .is_enabled(MCP_SECTION, &config.id)
        .await;
    McpServerView {
        host_id: kanon_core::mcp::host_id(&config.id),
        id: config.id,
        name: config.name,
        transport: config.transport,
        enabled,
        health,
    }
}

/// Validates a server identifier supplied in the URL.
///
/// The identifier becomes part of a tool name and of the host identifier, so the same conservative
/// character set used for skills applies here.
fn validate_server_id(id: &str) -> Result<String, ApiError> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 64
        || id.starts_with('.')
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(ApiError::BadRequest(format!(
            "Invalid MCP server identifier '{id}'"
        )));
    }
    Ok(id.to_string())
}
