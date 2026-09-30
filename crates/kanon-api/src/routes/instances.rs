//! Bot instance management (`/api/v1/instances`).
//!
//! # Why instances are managed here
//! An adapter only declares *where* messages come from. An instance decides *whether and how*
//! they are answered: which platforms it serves, which persona it speaks with, which model it
//! uses, and which session each conversation continues in. Nothing answers inbound traffic until
//! an enabled instance claims the platform, so this catalog is the difference between "adapters
//! configured" and "a bot is running".
//!
//! Writes follow *validate → persist → publish*: the draft is checked against the rest of the
//! catalog (adapter ownership, persona existence) before it reaches disk, and generated instance
//! personas are synchronized afterwards so the persona catalog always mirrors the instances.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, put};
use serde::{Deserialize, Serialize};

use kanon_core::instance::{InstanceDraft, InstanceError, sync_instance_personas};
use kanon_core::{AdapterDescriptor, BotInstance, ContextPolicy, ReplyPolicy};

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the instance endpoints.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/instances",
            get(list_instances).post(create_instance),
        )
        .route(
            "/api/v1/instances/:id",
            put(update_instance).delete(delete_instance),
        )
}

/// Connectivity of one adapter claimed by an instance.
#[derive(Debug, Serialize)]
pub struct AdapterStatus {
    /// Platform identifier as claimed by the instance.
    pub platform: String,
    /// Whether the node knows this platform at all (catches typos in the console).
    pub known: bool,
    /// Whether the adapter can currently serve messages.
    pub connected: bool,
    /// Console-facing adapter name, when the platform is known.
    pub display_name: Option<String>,
    /// `builtin` or `plugin`, when the platform is known.
    pub kind: Option<String>,
}

/// Console-facing view of one instance.
#[derive(Debug, Serialize)]
pub struct InstanceView {
    /// Stable identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether the instance answers messages.
    pub enabled: bool,
    /// Claimed platform identifiers.
    pub adapters: Vec<String>,
    /// Selected persona from the node catalog.
    pub persona_id: Option<String>,
    /// Prompt written for this instance.
    pub system_prompt: Option<String>,
    /// Model override; `null` means the node's default model.
    pub model: Option<String>,
    /// Reply-policy override; `null` inherits the node-wide policy.
    pub reply_policy: Option<ReplyPolicy>,
    /// Context-extras override; `null` inherits the node-wide policy.
    pub context_policy: Option<ContextPolicy>,
    /// Per-member or shared group sessions.
    pub session_scope: kanon_core::SessionScope,
    /// Whether unanswered group messages reach the model on its next turn.
    pub observe_group: bool,
    /// Per-plugin overrides.
    pub plugins: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
    /// Per-skill overrides.
    pub skills: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
    /// Per-MCP-server overrides.
    pub mcp: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
    /// Live state of every claimed adapter.
    pub adapter_status: Vec<AdapterStatus>,
}

/// Response of `GET /api/v1/instances`.
#[derive(Debug, Serialize)]
pub struct InstancesResponse {
    /// Total configured instances.
    pub total: usize,
    /// Instances that currently accept messages.
    pub enabled: usize,
    /// Node-wide reply policy inherited by instances without an override.
    pub node_reply_policy: ReplyPolicy,
    /// Node-wide context-extras policy inherited by instances without an override.
    pub node_context_policy: ContextPolicy,
    /// The instances themselves.
    pub instances: Vec<InstanceView>,
}

/// Response of a create/update/delete call.
#[derive(Debug, Serialize)]
pub struct InstanceMutationResponse {
    /// Whether the catalog changed.
    pub applied: bool,
    /// Human-readable confirmation.
    pub message: String,
    /// The stored instance, when one remains.
    pub instance: Option<InstanceView>,
}

/// Request payload for creating or updating an instance.
#[derive(Debug, Deserialize)]
pub struct InstanceRequest {
    /// Human-readable name.
    pub name: String,
    /// Whether the instance should answer messages.
    #[serde(default)]
    pub enabled: bool,
    /// Platform identifiers to claim.
    #[serde(default)]
    pub adapters: Vec<String>,
    /// Persona identifier from the node catalog.
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Prompt written specifically for this instance.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional model override; omit or `null` to use the node's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reply-policy override; omit or `null` to inherit the node-wide policy.
    #[serde(default)]
    pub reply_policy: Option<ReplyPolicy>,
    /// Optional context-extras override; omit or `null` to inherit the node-wide policy.
    #[serde(default)]
    pub context_policy: Option<ContextPolicy>,
    /// `user` (default) or `group`.
    #[serde(default)]
    pub session_scope: kanon_core::SessionScope,
    /// Show unanswered group messages to the model on its next turn.
    #[serde(default)]
    pub observe_group: bool,
    /// Per-plugin overrides (`inherit` | `enable` | `disable`).
    #[serde(default)]
    pub plugins: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
    /// Per-skill overrides (`inherit` | `enable` | `disable`).
    #[serde(default)]
    pub skills: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
    /// Per-MCP-server overrides (`inherit` | `enable` | `disable`).
    #[serde(default)]
    pub mcp: std::collections::HashMap<String, kanon_core::instance::ItemPolicy>,
}

impl From<InstanceRequest> for InstanceDraft {
    fn from(request: InstanceRequest) -> Self {
        Self {
            name: request.name,
            enabled: request.enabled,
            adapters: request.adapters,
            persona_id: request.persona_id,
            system_prompt: request.system_prompt,
            model: request.model,
            reply_policy: request.reply_policy,
            context_policy: request.context_policy,
            session_scope: request.session_scope,
            observe_group: request.observe_group,
            plugins: request.plugins,
            skills: request.skills,
            mcp: request.mcp,
        }
    }
}

impl AdapterStatus {
    /// Builds the status of one claimed platform from the node's adapter catalog.
    fn from_catalog(platform: &str, catalog: &[AdapterDescriptor]) -> Self {
        match catalog.iter().find(|adapter| adapter.platform == platform) {
            Some(adapter) => Self {
                platform: platform.to_string(),
                known: true,
                connected: adapter.connected,
                display_name: Some(adapter.display_name.clone()),
                kind: Some(match adapter.kind {
                    kanon_core::AdapterKind::Builtin => "builtin".to_string(),
                    kanon_core::AdapterKind::Plugin => "plugin".to_string(),
                }),
            },
            None => Self {
                platform: platform.to_string(),
                known: false,
                connected: false,
                display_name: None,
                kind: None,
            },
        }
    }
}

/// Renders one instance together with the live state of its adapters.
async fn view(state: &ApiState, instance: &BotInstance) -> InstanceView {
    let catalog = state.supervisor().adapter_catalog().await;

    InstanceView {
        id: instance.id.clone(),
        name: instance.name.clone(),
        enabled: instance.enabled,
        adapters: instance.adapters.clone(),
        persona_id: instance.persona_id.clone(),
        system_prompt: instance.system_prompt.clone(),
        model: instance.model.clone(),
        reply_policy: instance.reply_policy,
        context_policy: instance.context_policy,
        session_scope: instance.session_scope,
        observe_group: instance.observe_group,
        plugins: instance.plugins.clone(),
        skills: instance.skills.clone(),
        mcp: instance.mcp.clone(),
        adapter_status: instance
            .adapters
            .iter()
            .map(|platform| AdapterStatus::from_catalog(platform, &catalog))
            .collect(),
    }
}

/// Maps catalog failures onto management-gateway semantics.
fn map_error(err: InstanceError) -> ApiError {
    match err {
        InstanceError::NotFound(id) => {
            ApiError::NotFound(format!("instance '{id}' does not exist"))
        }
        InstanceError::Conflict { platform, owner } => ApiError::Conflict(format!(
            "adapter '{platform}' is already enabled by instance '{owner}'; disable it there first"
        )),
        InstanceError::AmbiguousPlatform { platform, owners } => ApiError::Conflict(format!(
            "adapter '{platform}' is claimed by several enabled instances ({owners:?}); resolve the catalog first"
        )),
        InstanceError::Invalid(reason) => ApiError::BadRequest(reason),
        InstanceError::Io(reason) => ApiError::Internal(reason),
    }
}

/// Rejects a persona that the node does not know, so the console cannot store a typo.
fn validate_persona(state: &ApiState, draft: &InstanceDraft) -> Result<(), ApiError> {
    if let Some(persona_id) = draft.persona_id.as_deref()
        && state.personas().get(persona_id).is_none()
    {
        return Err(ApiError::BadRequest(format!(
            "persona '{persona_id}' does not exist on this node"
        )));
    }
    Ok(())
}

/// Publishes instance prompts as personas so the existing prompt pipeline applies them.
async fn publish_personas(state: &ApiState) {
    let instances = state.instances().list().await;
    sync_instance_personas(&instances, state.personas());
}

/// Handler for `GET /api/v1/instances`.
async fn list_instances(State(state): State<ApiState>) -> Json<InstancesResponse> {
    let instances = state.instances().list().await;
    let enabled = instances.iter().filter(|instance| instance.enabled).count();

    let mut views = Vec::with_capacity(instances.len());
    for instance in &instances {
        views.push(view(&state, instance).await);
    }

    Json(InstancesResponse {
        total: instances.len(),
        enabled,
        node_reply_policy: state.reply_policy().get(),
        node_context_policy: state.context_policy().get(),
        instances: views,
    })
}

/// Handler for `POST /api/v1/instances`.
async fn create_instance(
    State(state): State<ApiState>,
    Json(payload): Json<InstanceRequest>,
) -> Result<Json<InstanceMutationResponse>, ApiError> {
    let draft: InstanceDraft = payload.into();
    validate_persona(&state, &draft)?;

    let instance = state.instances().create(draft).await.map_err(map_error)?;
    publish_personas(&state).await;

    tracing::info!(
        instance_id = %instance.id,
        enabled = instance.enabled,
        adapters = ?instance.adapters,
        "Bot instance created"
    );

    Ok(Json(InstanceMutationResponse {
        applied: true,
        message: format!("实例 '{}' 已创建", instance.name),
        instance: Some(view(&state, &instance).await),
    }))
}

/// Handler for `PUT /api/v1/instances/:id`.
async fn update_instance(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<InstanceRequest>,
) -> Result<Json<InstanceMutationResponse>, ApiError> {
    let draft: InstanceDraft = payload.into();
    validate_persona(&state, &draft)?;

    let instance = state
        .instances()
        .update(&id, draft)
        .await
        .map_err(map_error)?;
    publish_personas(&state).await;

    tracing::info!(
        instance_id = %instance.id,
        enabled = instance.enabled,
        adapters = ?instance.adapters,
        model = ?instance.model,
        "Bot instance updated"
    );

    Ok(Json(InstanceMutationResponse {
        applied: true,
        message: format!("实例 '{}' 已更新并立即生效", instance.name),
        instance: Some(view(&state, &instance).await),
    }))
}

/// Handler for `DELETE /api/v1/instances/:id`.
async fn delete_instance(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<InstanceMutationResponse>, ApiError> {
    state.instances().delete(&id).await.map_err(map_error)?;
    publish_personas(&state).await;

    tracing::info!(instance_id = %id, "Bot instance deleted");

    Ok(Json(InstanceMutationResponse {
        applied: true,
        message: format!("实例 '{id}' 已删除，其适配器不再由任何实例处理"),
        instance: None,
    }))
}
