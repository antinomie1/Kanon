//! Tool catalog route.
//!
//! Answers one operator question: *which tools can the model actually call right now, and who
//! provides them?* The three sources are merged exactly the way the model sees them:
//!
//! - **builtin** — native in-process tools registered by the node itself (e.g. `read_skill`);
//! - **plugin** — tools declared by a running plugin host;
//! - **mcp** — tools advertised by an enabled MCP server, already namespaced as
//!   `mcp__<server>__<tool>` by the MCP client.
//!
//! Plugin and MCP tools are resolved through the same [`resolve_tools`] helper the agent uses, so
//! a name the console shows is by construction a name the model receives — including the
//! namespacing applied when two providers declare the same tool name.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use kanon_core::mcp::host_id;
use kanon_llm::tool_router::{ToolHost, resolve_tools};

use crate::error::ApiError;
use crate::state::ApiState;

/// Identifier reported for tools the node itself provides.
const BUILTIN_PROVIDER: &str = "kanon-core";

/// Registers the tool catalog route.
pub fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/tools", get(list_tools))
}

/// Where one tool comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Native in-process tool registered by the node.
    Builtin,
    /// Tool declared by a plugin host.
    Plugin,
    /// Tool advertised by an MCP server.
    Mcp,
}

/// One callable tool as the console sees it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolView {
    /// Name the model must use when calling the tool.
    pub name: String,
    /// Natural-language description presented to the model.
    pub description: String,
    /// Provider kind.
    pub source: ToolSource,
    /// Plugin identifier, MCP server identifier or `kanon-core` for builtins.
    pub provider_id: String,
    /// Host process exposing the tool; absent for builtin tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// JSON Schema of the accepted arguments.
    pub parameters: serde_json::Value,
}

/// Response of `GET /api/v1/tools`.
#[derive(Debug, serde::Serialize)]
pub struct ToolCatalog {
    /// Total number of tools the model can call.
    pub total: usize,
    /// Count per source, so the console can summarize without walking the list.
    pub builtin: usize,
    /// Plugin-provided tools.
    pub plugin: usize,
    /// MCP-provided tools.
    pub mcp: usize,
    /// The tools themselves: builtins first, then plugins and MCP sorted by name.
    pub tools: Vec<ToolView>,
}

/// Lists every tool the model can call on this node.
async fn list_tools(State(state): State<ApiState>) -> Result<Json<ToolCatalog>, ApiError> {
    let mut tools: Vec<ToolView> = state
        .agent_factory()
        .native_tools()
        .iter()
        .map(|tool| {
            let definition = tool.definition();
            ToolView {
                name: definition.name,
                description: definition.description,
                source: ToolSource::Builtin,
                provider_id: BUILTIN_PROVIDER.to_string(),
                host_id: None,
                parameters: definition.parameters,
            }
        })
        .collect();
    let builtin = tools.len();

    // Plugin hosts and MCP servers form one slice, exactly as they do for the model. The MCP
    // filter is applied here as well, so a server switched off in the console disappears from the
    // catalog along with its tools.
    let mut hosts: Vec<Arc<dyn ToolHost>> = state
        .supervisor()
        .get_all_hosts()
        .await
        .into_iter()
        .map(|host| host as Arc<dyn ToolHost>)
        .collect();
    let mcp_servers: HashMap<String, String> = state
        .mcp()
        .describe()
        .await
        .into_iter()
        .map(|(config, _health)| (host_id(&config.id), config.id))
        .collect();
    hosts.extend(state.mcp().hosts_for_instance(None).await);

    let mut provided: Vec<ToolView> = resolve_tools(&hosts)?
        .into_iter()
        .map(|resolved| {
            // A host identifier registered by the pool identifies an MCP server; anything else is
            // a plugin host. Server ids are recovered from the configuration so the console shows
            // the identifier the operator typed, not the derived host name.
            let (source, provider_id) = match mcp_servers.get(&resolved.host_id) {
                Some(server_id) => (ToolSource::Mcp, server_id.clone()),
                None => (ToolSource::Plugin, resolved.plugin_id),
            };
            ToolView {
                name: resolved.definition.name,
                description: resolved.definition.description,
                source,
                provider_id,
                host_id: Some(resolved.host_id),
                parameters: resolved.definition.parameters,
            }
        })
        .collect();
    provided.sort_by(|a, b| {
        let rank = |source: ToolSource| match source {
            ToolSource::Builtin => 0,
            ToolSource::Plugin => 1,
            ToolSource::Mcp => 2,
        };
        rank(a.source)
            .cmp(&rank(b.source))
            .then_with(|| a.name.cmp(&b.name))
    });

    let plugin = provided
        .iter()
        .filter(|tool| tool.source == ToolSource::Plugin)
        .count();
    let mcp = provided
        .iter()
        .filter(|tool| tool.source == ToolSource::Mcp)
        .count();
    tools.extend(provided);

    Ok(Json(ToolCatalog {
        total: tools.len(),
        builtin,
        plugin,
        mcp,
        tools,
    }))
}
