//! RESTful management routes (`/api/v1`).
//!
//! Each submodule owns one resource family and exposes a `routes()` constructor returning a
//! `Router<ApiState>` fragment. [`api_router`] merges the fragments so the gateway can be
//! assembled once in [`crate::server`] and reused verbatim by integration tests.
//!
//! Endpoint map (mirrors the management gateway spec):
//!
//! | Method | Path | Purpose |
//! | :--- | :--- | :--- |
//! | `GET` | `/api/v1/health` | Liveness, uptime, memory footprint |
//! | `GET` | `/api/v1/metrics` | Prometheus text exposition |
//! | `GET` | `/api/v1/adapters` | Platform adapter catalog (built-in and plugin) |
//! | `POST` | `/api/v1/adapters/:platform/ingest` | Fast-ACK inbound message ingress |
//! | `GET` | `/api/v1/adapters/milky/config` | Milky adapter configuration and live status |
//! | `PUT` | `/api/v1/adapters/milky/config` | Validate, persist and hot-apply the Milky adapter |
//! | `POST` | `/api/v1/adapters/milky/config/test` | Probe a Milky endpoint without saving |
//! | `GET` | `/api/v1/instances` | Bot instances gating and partitioning inbound traffic |
//! | `POST` | `/api/v1/instances` | Create a bot instance |
//! | `PUT` | `/api/v1/instances/{id}` | Update a bot instance (adapters, persona, model) |
//! | `DELETE` | `/api/v1/instances/{id}` | Delete a bot instance |
//! | `GET` | `/api/v1/plugins` | Plugin and host catalog |
//! | `GET` | `/api/v1/plugins/:id/config` | Current values plus declaration schema |
//! | `PUT` | `/api/v1/plugins/:id/config` | Validate, hot reload, then persist |
//! | `POST` | `/api/v1/plugins/:id/restart` | Restart the owning host process |
//! | `GET` | `/api/v1/sessions` | Paginated session metadata |
//! | `POST` | `/api/v1/sessions/:id/reset` | Clear history, keep persona and variables |
//! | `POST` | `/api/v1/sessions/:id/persona` | Hot-swap the session persona |
//! | `GET` | `/api/v1/personas` | Persona catalog |
//! | `GET` | `/api/v1/providers` | Provider catalog plus the node's effective provider |
//! | `PUT` | `/api/v1/providers/active` | Configure the node's provider (persisted, live) |
//! | `DELETE` | `/api/v1/providers/active` | Clear the node's provider |
//! | `GET` | `/api/v1/skills` | Installed skills with their node-wide switch |
//! | `POST` | `/api/v1/skills` | Install a skill from a zip archive or local directory |
//! | `DELETE` | `/api/v1/skills/:id` | Remove an installed skill |
//! | `PUT` | `/api/v1/skills/:id/enabled` | Enable or disable a skill node-wide |
//! | `GET` | `/api/v1/tools` | Every tool the model can call, with its provider |
//! | `GET` | `/api/v1/mcp/servers` | Configured MCP servers with health |
//! | `PUT` | `/api/v1/mcp/servers/:id` | Create or replace an MCP server definition |
//! | `DELETE` | `/api/v1/mcp/servers/:id` | Remove an MCP server definition |
//! | `PUT` | `/api/v1/mcp/servers/:id/enabled` | Enable or disable an MCP server node-wide |
//! | `POST` | `/api/v1/chat/completions` | Sandbox chat with JSON or SSE responses |

pub mod adapters;
pub mod chat;
pub mod health;
pub mod instances;
pub mod mcp;
pub mod metrics;
pub mod milky;
pub mod personas;
pub mod plugins;
pub mod providers;
pub mod sessions;
pub mod skills;
pub mod system;
pub mod tools;

use axum::Router;

use crate::error::ApiError;
use crate::state::ApiState;

/// Assembles every `/api/v1` REST route fragment.
pub fn api_router() -> Router<ApiState> {
    Router::new()
        .merge(health::routes())
        .merge(metrics::routes())
        .merge(adapters::routes())
        .merge(milky::routes())
        .merge(instances::routes())
        .merge(plugins::routes())
        .merge(sessions::routes())
        .merge(skills::routes())
        .merge(mcp::routes())
        .merge(tools::routes())
        .merge(personas::routes())
        .merge(chat::routes())
        .merge(system::routes())
        .merge(providers::routes())
}

/// Fallback handler returning a structured `404` for unknown paths.
///
/// A JSON body is returned instead of Axum's empty default so console clients can rely on a
/// single error envelope for every failure, including routing mistakes.
pub async fn not_found() -> ApiError {
    ApiError::NotFound("No such management endpoint".to_string())
}
