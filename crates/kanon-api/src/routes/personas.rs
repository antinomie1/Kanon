//! Persona library routes (`/api/v1/personas`).
//!
//! Personas are served straight from the shared [`kanon_llm::PersonaRegistry`], which is the same
//! registry the agent consults through its persona hook, so a persona created here is usable on the
//! very next turn with no restart.
//!
//! # Who owns what
//! - The **base assistant** ships with the node: read-only, never removable.
//! - **Custom** personas belong to the operator. They are created, edited and removed here and
//!   persisted to `data/personas.json`.
//! - **Instance** personas are generated from a bot instance's own prompt and are owned by that
//!   instance; they are listed for visibility but cannot be changed here.
//!
//! Every mutation follows *validate → persist → apply*: nothing reaches the running registry
//! unless the file was written, so the library and the disk can never disagree.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use kanon_llm::{BASE_PERSONA_ID, Persona, PersonaError, PersonaKind};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::ApiState;

/// Registers the persona library routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/personas", get(list_personas).post(create_persona))
        .route(
            "/api/v1/personas/:id",
            axum::routing::put(update_persona).delete(delete_persona),
        )
}

/// Serializable description of a registered persona.
#[derive(Debug, Serialize)]
pub struct PersonaView {
    /// Persona identifier used by the session and instance persona fields.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Optional one-line description.
    pub description: String,
    /// The system prompt, exactly as it is sent to the model.
    pub prompt: String,
    /// Where the persona comes from: `builtin`, `custom` or `instance`.
    pub kind: &'static str,
    /// Bot instances that select this persona, which is what blocks its removal.
    pub used_by: Vec<String>,
}

/// Persona library payload.
#[derive(Debug, Serialize)]
pub struct PersonaCatalog {
    /// Total number of registered personas.
    pub total: usize,
    /// The persona a conversation uses when none was chosen.
    pub base_persona_id: &'static str,
    /// Registered personas: the base assistant first, then the rest ordered by identifier.
    pub personas: Vec<PersonaView>,
}

/// Request body of `POST /api/v1/personas`.
#[derive(Debug, Deserialize)]
pub struct CreatePersonaRequest {
    /// Identifier slug. Omitted, it is derived from the name and made unique.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name.
    pub name: String,
    /// Optional one-line description.
    #[serde(default)]
    pub description: String,
    /// The system prompt.
    pub prompt: String,
}

/// Request body of `PUT /api/v1/personas/:id`.
#[derive(Debug, Deserialize)]
pub struct UpdatePersonaRequest {
    /// Display name.
    pub name: String,
    /// Optional one-line description.
    #[serde(default)]
    pub description: String,
    /// The system prompt.
    pub prompt: String,
}

/// Wire name of a persona kind.
fn kind_name(kind: PersonaKind) -> &'static str {
    match kind {
        PersonaKind::Builtin => "builtin",
        PersonaKind::Custom => "custom",
        PersonaKind::Instance => "instance",
    }
}

/// Maps persona failures onto management-gateway semantics.
fn map_error(err: PersonaError) -> ApiError {
    match err {
        PersonaError::InvalidId(_) | PersonaError::EmptyName | PersonaError::EmptyPrompt => {
            ApiError::BadRequest(err.to_string())
        }
        PersonaError::ReadOnly(_) => ApiError::Conflict(err.to_string()),
        PersonaError::NotFound(_) => ApiError::NotFound(err.to_string()),
    }
}

/// Every operator-defined persona currently registered.
fn custom_personas(state: &ApiState) -> Vec<Persona> {
    state
        .personas()
        .list()
        .into_iter()
        .filter(|persona| persona.kind == PersonaKind::Custom)
        .collect()
}

/// Persists the given custom personas, reporting a storage failure as an internal error.
fn persist(state: &ApiState, personas: &[Persona]) -> Result<(), ApiError> {
    state
        .persona_store()
        .save(personas)
        .map_err(ApiError::Internal)
}

/// Turns a display name into an identifier slug (`Code Reviewer` becomes `code-reviewer`).
///
/// Names without any ASCII letter or digit (a Chinese name, for instance) yield `persona`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "persona".to_string()
    } else {
        slug.to_string()
    }
}

/// First identifier derived from `name` that no persona uses yet.
fn unique_id(state: &ApiState, name: &str) -> String {
    let base = slugify(name);
    if state.personas().get(&base).is_none() {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| state.personas().get(candidate).is_none())
        .expect("an unbounded range always yields an unused identifier")
}

/// Ids of the instances that select a persona.
async fn used_by(state: &ApiState, persona_id: &str) -> Vec<String> {
    state
        .instances()
        .list()
        .await
        .into_iter()
        .filter(|instance| instance.persona_id.as_deref() == Some(persona_id))
        .map(|instance| instance.id)
        .collect()
}

/// Renders the whole library.
async fn catalog(state: &ApiState) -> PersonaCatalog {
    let instances = state.instances().list().await;
    let mut personas: Vec<PersonaView> = state
        .personas()
        .list()
        .into_iter()
        .map(|persona| PersonaView {
            used_by: instances
                .iter()
                .filter(|instance| instance.persona_id.as_deref() == Some(persona.id.as_str()))
                .map(|instance| instance.id.clone())
                .collect(),
            kind: kind_name(persona.kind),
            id: persona.id,
            name: persona.name,
            description: persona.description,
            prompt: persona.prompt,
        })
        .collect();

    // The base assistant leads; the rest keep the registry's identifier order.
    personas.sort_by_key(|persona| persona.id != BASE_PERSONA_ID);

    PersonaCatalog {
        total: personas.len(),
        base_persona_id: BASE_PERSONA_ID,
        personas,
    }
}

/// Lists every built-in, operator-defined and instance persona.
async fn list_personas(State(state): State<ApiState>) -> Json<PersonaCatalog> {
    Json(catalog(&state).await)
}

/// Creates an operator-defined persona.
async fn create_persona(
    State(state): State<ApiState>,
    Json(payload): Json<CreatePersonaRequest>,
) -> Result<Json<PersonaCatalog>, ApiError> {
    let id = match payload
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => id.to_string(),
        None => unique_id(&state, &payload.name),
    };

    if state.personas().get(&id).is_some() {
        return Err(ApiError::Conflict(format!(
            "persona '{id}' already exists; edit it instead"
        )));
    }

    let persona = Persona::custom(id, payload.name, payload.description, payload.prompt)
        .map_err(map_error)?;

    let mut all = custom_personas(&state);
    all.push(persona.clone());
    persist(&state, &all)?;
    state
        .personas()
        .register(persona.clone())
        .map_err(map_error)?;

    tracing::info!(persona_id = %persona.id, "Persona created through the control plane");
    Ok(Json(catalog(&state).await))
}

/// Edits an operator-defined persona; the id never changes, so references to it keep working.
async fn update_persona(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdatePersonaRequest>,
) -> Result<Json<PersonaCatalog>, ApiError> {
    let existing = state
        .personas()
        .get(&id)
        .ok_or_else(|| map_error(PersonaError::NotFound(id.clone())))?;
    require_custom(&existing)?;

    let persona = Persona::custom(
        id.clone(),
        payload.name,
        payload.description,
        payload.prompt,
    )
    .map_err(map_error)?;

    let all: Vec<Persona> = custom_personas(&state)
        .into_iter()
        .map(|current| {
            if current.id == id {
                persona.clone()
            } else {
                current
            }
        })
        .collect();
    persist(&state, &all)?;
    state.personas().register(persona).map_err(map_error)?;

    tracing::info!(persona_id = %id, "Persona updated through the control plane");
    Ok(Json(catalog(&state).await))
}

/// Removes an operator-defined persona.
///
/// Refused while a bot instance selects it: an instance whose persona vanished would silently start
/// answering with different instructions. Sessions bound to it are unbound (they use the base
/// assistant again) because a session binding is transient, not configuration.
async fn delete_persona(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<PersonaCatalog>, ApiError> {
    let existing = state
        .personas()
        .get(&id)
        .ok_or_else(|| map_error(PersonaError::NotFound(id.clone())))?;
    require_custom(&existing)?;

    let users = used_by(&state, &id).await;
    if !users.is_empty() {
        return Err(ApiError::Conflict(format!(
            "persona '{id}' is used by instance(s) {}; pick another persona there first",
            users.join(", ")
        )));
    }

    let all: Vec<Persona> = custom_personas(&state)
        .into_iter()
        .filter(|persona| persona.id != id)
        .collect();
    persist(&state, &all)?;
    state.personas().remove(&id).map_err(map_error)?;
    let unbound = state.sessions().unbind_persona(&id);

    tracing::info!(persona_id = %id, unbound_sessions = unbound, "Persona removed through the control plane");
    Ok(Json(catalog(&state).await))
}

/// Only operator-defined personas can be edited or removed here.
fn require_custom(persona: &Persona) -> Result<(), ApiError> {
    match persona.kind {
        PersonaKind::Custom => Ok(()),
        PersonaKind::Builtin => Err(map_error(PersonaError::ReadOnly(persona.id.clone()))),
        PersonaKind::Instance => Err(ApiError::Conflict(format!(
            "persona '{}' is generated from a bot instance's own prompt; edit it on that instance",
            persona.id
        ))),
    }
}
