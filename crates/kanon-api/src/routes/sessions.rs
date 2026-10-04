//! Session lifecycle management routes.
//!
//! Sessions are keyed by opaque strings (`channel:123:user:456`, `user:42`, ...). The gateway
//! never invents keys: it lists what the session manager tracks and mutates only existing
//! entries, so a console typo surfaces as `404` instead of silently creating phantom sessions.

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use kanon_llm::{SessionMetadata, SessionStatus};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::observability::TraceEvent;
use crate::state::ApiState;

/// Maximum page size accepted by the listing endpoint.
///
/// Bounded to protect the control plane from a console requesting an unbounded scan of every
/// tracked session in a single request.
const MAX_PAGE_SIZE: u32 = 200;

/// Registers all session management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/sessions", get(list_sessions))
        .route("/api/v1/sessions/:id/reset", post(reset_session))
        .route("/api/v1/sessions/:id/persona", post(set_persona))
}

/// Query parameters accepted by the session listing endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct SessionsQuery {
    /// 1-based page number (default 1).
    pub page: Option<u32>,
    /// Page size between 1 and [`MAX_PAGE_SIZE`] (default 50).
    pub page_size: Option<u32>,
    /// Optional lifecycle status filter (`active`, `idle`, `closed`, `archived`).
    pub status: Option<String>,
    /// Optional scope filter (`user`, `channel`, `channel_user`, `thread`).
    pub scope: Option<String>,
    /// Optional case-insensitive substring match on the session key.
    pub search: Option<String>,
}

/// Paginated session listing.
#[derive(Debug, Serialize)]
pub struct SessionPage {
    /// Sessions in this page, most recently active first.
    pub items: Vec<SessionMetadata>,
    /// 1-based page number.
    pub page: u32,
    /// Requested page size.
    pub page_size: u32,
    /// Total number of sessions matching the filter.
    pub total: usize,
    /// Total number of pages for the current page size.
    pub total_pages: usize,
}

/// Request body for `POST /api/v1/sessions/:id/persona`.
#[derive(Debug, Deserialize)]
pub struct PersonaSwitchRequest {
    /// Persona identifier to bind to the session; `null` or blank removes the binding, after which
    /// the session uses the base assistant.
    #[serde(default)]
    pub persona_id: Option<String>,
}

/// Result of a persona switch.
#[derive(Debug, Serialize)]
pub struct PersonaSwitchResponse {
    /// Session that was updated.
    pub session_key: String,
    /// Newly bound persona identifier; `null` when the binding was removed.
    pub persona_id: Option<String>,
    /// Display name of the persona now in effect, for immediate console feedback.
    pub persona_name: String,
}

/// Result of a session reset.
#[derive(Debug, Serialize)]
pub struct SessionResetResponse {
    /// Session that was reset.
    pub session_key: String,
    /// Always `true` on success, so clients can assert the outcome explicitly.
    pub reset: bool,
    /// Persona preserved across the reset.
    pub persona_id: Option<String>,
    /// Session variables preserved across the reset.
    pub variables: std::collections::HashMap<String, String>,
    /// Turn counter after the reset (always zero).
    pub turn_count: usize,
}

/// Lists tracked sessions with pagination and optional filtering.
async fn list_sessions(
    State(state): State<ApiState>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<SessionPage>, ApiError> {
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return Err(ApiError::BadRequest(
            "Query parameter 'page' must be >= 1".to_string(),
        ));
    }

    let page_size = query.page_size.unwrap_or(50);
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(ApiError::BadRequest(format!(
            "Query parameter 'page_size' must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }

    let status_filter = match query.status.as_deref() {
        None => None,
        Some(raw) => Some(parse_status(raw)?),
    };
    let scope_filter = query.scope.as_deref().map(|raw| raw.to_ascii_lowercase());
    let search = query
        .search
        .as_deref()
        .map(|raw| raw.to_ascii_lowercase())
        .filter(|raw| !raw.is_empty());

    let mut sessions = state.sessions().list_sessions();

    sessions.retain(|meta| {
        status_filter.is_none_or(|status| meta.status == status)
            && scope_filter
                .as_deref()
                .is_none_or(|scope| scope_name(meta) == scope)
            && search
                .as_deref()
                .is_none_or(|needle| meta.session_key.to_ascii_lowercase().contains(needle))
    });

    // Most recently active first, with the session key as a deterministic tie-breaker so
    // pagination stays stable while sessions are being updated concurrently.
    sessions.sort_by(|a, b| {
        b.last_active_at
            .cmp(&a.last_active_at)
            .then_with(|| a.session_key.cmp(&b.session_key))
    });

    let total = sessions.len();
    let total_pages = total.div_ceil(page_size as usize);
    let offset = (page as usize - 1) * page_size as usize;
    let items = sessions
        .into_iter()
        .skip(offset)
        .take(page_size as usize)
        .collect();

    Ok(Json(SessionPage {
        items,
        page,
        page_size,
        total,
        total_pages,
    }))
}

/// Clears a session's history while preserving its persona and variables.
async fn reset_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionResetResponse>, ApiError> {
    // Resetting an unknown session would be a silent no-op; report it instead so console
    // operators notice stale dashboards or mistyped keys immediately.
    if state.sessions().get_metadata(&session_id).is_none() {
        return Err(ApiError::NotFound(format!(
            "Session '{session_id}' is not tracked; nothing to reset"
        )));
    }

    state.sessions().reset_session(&session_id).await?;

    state
        .observability()
        .events
        .publish(TraceEvent::SessionReset {
            session_id: session_id.clone(),
        });

    let metadata = state.sessions().get_or_create(&session_id);

    tracing::info!(session_id = %session_id, "Session history reset by control plane");

    Ok(Json(SessionResetResponse {
        session_key: session_id,
        reset: true,
        persona_id: metadata.persona_id,
        variables: metadata.variables,
        turn_count: metadata.turn_count,
    }))
}

/// Hot-swaps the persona bound to a session.
async fn set_persona(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(body): Json<PersonaSwitchRequest>,
) -> Result<Json<PersonaSwitchResponse>, ApiError> {
    let _writing = state.sessions().try_write(&session_id)?;
    let requested = body
        .persona_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());

    let Some(persona_id) = requested else {
        state.sessions().clear_persona(&session_id)?;
        tracing::info!(session_id = %session_id, "Session persona binding removed by control plane");
        return Ok(Json(PersonaSwitchResponse {
            session_key: session_id,
            persona_id: None,
            persona_name: state.personas().base().name,
        }));
    };

    let persona = state
        .personas()
        .get(persona_id)
        .ok_or_else(|| ApiError::NotFound(format!("Persona '{persona_id}' is not registered")))?;

    // The binding itself creates and persists a missing session after persona validation, so a
    // rejected persona or failed write cannot leave behind an empty session record.
    state.sessions().set_persona(&session_id, persona_id)?;

    state
        .observability()
        .events
        .publish(TraceEvent::PersonaSwitched {
            session_id: session_id.clone(),
            persona_id: persona_id.to_string(),
        });

    tracing::info!(
        session_id = %session_id,
        persona_id = %persona_id,
        "Session persona switched by control plane"
    );

    Ok(Json(PersonaSwitchResponse {
        session_key: session_id,
        persona_id: Some(persona_id.to_string()),
        persona_name: persona.name,
    }))
}

/// Parses a lifecycle status filter.
fn parse_status(raw: &str) -> Result<SessionStatus, ApiError> {
    match raw.to_ascii_lowercase().as_str() {
        "active" => Ok(SessionStatus::Active),
        "idle" => Ok(SessionStatus::Idle),
        "closed" => Ok(SessionStatus::Closed),
        "archived" => Ok(SessionStatus::Archived),
        other => Err(ApiError::BadRequest(format!(
            "Unknown session status '{other}'; expected active, idle, closed or archived"
        ))),
    }
}

/// Returns the wire name of a session's scope category.
fn scope_name(meta: &SessionMetadata) -> &'static str {
    match meta.scope {
        kanon_llm::SessionScope::User => "user",
        kanon_llm::SessionScope::Channel => "channel",
        kanon_llm::SessionScope::ChannelUser => "channel_user",
        kanon_llm::SessionScope::Thread => "thread",
        kanon_llm::SessionScope::Custom(_) => "custom",
    }
}
