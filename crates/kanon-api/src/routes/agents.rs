//! Agent selection (`/api/v1/agents`).
//!
//! # What an agent is here
//! The agent is the engine that answers a conversation turn (see [`kanon_llm::Agent`]). The node
//! has one default agent, and a bot instance may override it the same way it overrides the model.
//!
//! # Why a selection with a single choice exists
//! Only the built-in agent is selectable today. The setting is in place so operators already see,
//! node-wide and per instance, which engine answers; agents provided by plugins are meant to join
//! [`kanon_llm::selectable_agents`] without the console or this contract changing.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, put};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the agent selection endpoints.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/agents/default", put(set_default_agent))
}

/// Response of `GET /api/v1/agents`.
#[derive(Debug, Serialize)]
pub struct AgentsResponse {
    /// Identifiers of the agents an operator may select, node-wide or per instance.
    pub agents: Vec<String>,
    /// Agent that answers for every instance without an agent override.
    pub default_agent: String,
}

/// Request body of `PUT /api/v1/agents/default`.
#[derive(Debug, Deserialize)]
pub struct SetDefaultAgentRequest {
    /// Identifier of a selectable agent.
    pub agent: String,
}

/// Handler for `GET /api/v1/agents`.
async fn list_agents(State(state): State<ApiState>) -> Json<AgentsResponse> {
    Json(AgentsResponse {
        agents: kanon_llm::selectable_agents()
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        default_agent: state.node_settings().default_agent,
    })
}

/// Handler for `PUT /api/v1/agents/default`.
///
/// An unknown agent is refused by the settings validation before anything is stored, so the
/// document never names an engine the node cannot run.
async fn set_default_agent(
    State(state): State<ApiState>,
    Json(payload): Json<SetDefaultAgentRequest>,
) -> Result<Json<AgentsResponse>, ApiError> {
    state.update_node_settings(|settings| {
        settings.default_agent = payload.agent.trim().to_string();
        Ok(())
    })?;
    Ok(list_agents(State(state)).await)
}
