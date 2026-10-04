//! Skill management routes.
//!
//! A skill is a directory under `data/skills/<id>/` holding a `SKILL.md`. Uploads arrive either as
//! a zip archive (whose single top-level directory or root must contain `SKILL.md`) or as a local
//! directory path; both are funnelled through [`kanon_core::SkillStore::install_from_dir`], so the
//! store stays the only component that writes installed skills.
//!
//! Enablement is node-wide operator state kept in the [`kanon_core::ToggleStore`] `skills` section,
//! mirroring plugins. A bot instance may still restrict a skill for its own conversations; that
//! override lives on the instance and is enforced when the catalog is built.

use std::io::Read;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::Router;
use axum::extract::{FromRequest, Multipart, Path as AxumPath, State};
use axum::routing::get;
use kanon_core::{SKILL_SECTION, SkillError, SkillMeta};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::ApiState;

/// Largest accepted upload, in bytes.
///
/// Skills are instructions: a few hundred kilobytes of archive is already generous, and refusing
/// oversized bodies here keeps a malformed upload from filling the data directory.
const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Registers all skill management routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/skills", get(list_skills).post(install_skill))
        .route("/api/v1/skills/:id", axum::routing::delete(remove_skill))
        .route(
            "/api/v1/skills/:id/enabled",
            axum::routing::put(set_skill_enabled),
        )
}

/// One installed skill as the console sees it.
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
    /// Identifier (directory name).
    pub id: String,
    /// Human/LLM readable name.
    pub name: String,
    /// One-line description shown in the model's catalog.
    pub description: String,
    /// Whether the skill is enabled node-wide.
    pub enabled: bool,
}

impl SkillView {
    /// Projects a stored skill with its node-wide switch applied.
    fn new(skill: SkillMeta, enabled: bool) -> Self {
        Self {
            id: skill.id,
            name: skill.name,
            description: skill.description,
            enabled,
        }
    }
}

/// Response of the skill catalog.
#[derive(Debug, Serialize)]
pub struct SkillCatalog {
    /// Installed skills, ordered by identifier.
    pub skills: Vec<SkillView>,
}

/// Request body toggling one skill node-wide.
#[derive(Debug, Deserialize)]
pub struct SetSkillEnabledRequest {
    /// Desired state.
    pub enabled: bool,
}

/// Confirmation of a skill state change or installation.
#[derive(Debug, Serialize)]
pub struct SkillStateResponse {
    /// Whether the requested change took effect (false when already in that state).
    pub applied: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Identifier of the affected skill.
    pub skill_id: String,
    /// Resulting node-wide state.
    pub enabled: bool,
}

/// Lists installed skills with their node-wide switch.
async fn list_skills(State(state): State<ApiState>) -> Result<Json<SkillCatalog>, ApiError> {
    let installed = state.skills().list().map_err(skill_error)?;

    let mut skills = Vec::with_capacity(installed.len());
    for skill in installed {
        let enabled = state
            .plugin_state()
            .is_enabled(SKILL_SECTION, &skill.id)
            .await;
        skills.push(SkillView::new(skill, enabled));
    }

    Ok(Json(SkillCatalog { skills }))
}

/// Enables or disables one skill node-wide.
///
/// The switch is consulted every time a catalog is built, so the change takes effect on the next
/// model request without touching the installed files.
async fn set_skill_enabled(
    State(state): State<ApiState>,
    AxumPath(skill_id): AxumPath<String>,
    Json(body): Json<SetSkillEnabledRequest>,
) -> Result<Json<SkillStateResponse>, ApiError> {
    let id = kanon_core::SkillStore::validate_id(&skill_id).map_err(skill_error)?;

    // Only an installed skill may be toggled: recording state for a typo would silently hide the
    // mistake until the operator wonders why their skill never appears.
    let installed = state.skills().list().map_err(skill_error)?;
    if !installed.iter().any(|skill| skill.id == id) {
        return Err(ApiError::NotFound(format!(
            "Skill '{id}' is not installed on this node"
        )));
    }

    let changed = state
        .plugin_state()
        .set_enabled(SKILL_SECTION, &id, body.enabled)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(SkillStateResponse {
        applied: changed,
        message: if !changed {
            format!(
                "Skill '{id}' is already {}",
                if body.enabled { "enabled" } else { "disabled" }
            )
        } else if body.enabled {
            format!("Skill '{id}' enabled")
        } else {
            format!("Skill '{id}' disabled")
        },
        skill_id: id,
        enabled: body.enabled,
    }))
}

/// Removes an installed skill and forgets its switch.
async fn remove_skill(
    State(state): State<ApiState>,
    AxumPath(skill_id): AxumPath<String>,
) -> Result<Json<SkillStateResponse>, ApiError> {
    let id = kanon_core::SkillStore::validate_id(&skill_id).map_err(skill_error)?;
    state.skills().remove(&id).map_err(skill_error)?;

    // A leftover entry would resurrect the old state if a skill with the same id is installed
    // again, which is exactly the kind of hidden carry-over the operator cannot see.
    state
        .plugin_state()
        .set_enabled(SKILL_SECTION, &id, true)
        .await
        .map_err(ApiError::Internal)?;

    tracing::info!(skill_id = %id, "Skill removed by the control plane");
    Ok(Json(SkillStateResponse {
        applied: true,
        message: format!("Skill '{id}' removed"),
        skill_id: id,
        enabled: false,
    }))
}

/// Installs a skill from an uploaded archive or a local directory.
async fn install_skill(
    State(state): State<ApiState>,
    req: axum::extract::Request,
) -> Result<Json<SkillView>, ApiError> {
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(req, &state)
            .await
            .map_err(|err| ApiError::BadRequest(format!("Invalid multipart payload: {err}")))?;

        let mut archive: Option<Vec<u8>> = None;
        let mut path: Option<String> = None;
        let mut requested_id: Option<String> = None;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|err| ApiError::BadRequest(err.to_string()))?
        {
            let name = field.name().unwrap_or("").to_string();
            match name.as_str() {
                "id" => {
                    let text = field
                        .text()
                        .await
                        .map_err(|err| ApiError::BadRequest(err.to_string()))?;
                    if !text.trim().is_empty() {
                        requested_id = Some(text.trim().to_string());
                    }
                }
                "path" => {
                    let text = field
                        .text()
                        .await
                        .map_err(|err| ApiError::BadRequest(err.to_string()))?;
                    if !text.trim().is_empty() {
                        path = Some(text.trim().to_string());
                    }
                }
                "file" => {
                    let bytes = field
                        .bytes()
                        .await
                        .map_err(|err| ApiError::BadRequest(err.to_string()))?;
                    if bytes.len() > MAX_UPLOAD_BYTES {
                        return Err(ApiError::BadRequest(format!(
                            "Upload is {} bytes, above the {MAX_UPLOAD_BYTES} byte limit",
                            bytes.len()
                        )));
                    }
                    archive = Some(bytes.to_vec());
                }
                _ => {}
            }
        }

        if let Some(bytes) = archive {
            let view = install_from_archive(&state, &bytes, requested_id.as_deref()).await?;
            Ok(Json(view))
        } else if let Some(path) = path {
            let view = install_from_path(&state, Path::new(&path), requested_id.as_deref()).await?;
            Ok(Json(view))
        } else {
            Err(ApiError::BadRequest(
                "Multipart form must contain either 'file' (a zip archive) or 'path'".to_string(),
            ))
        }
    } else if content_type.is_empty() || content_type.contains("application/json") {
        let bytes = axum::body::to_bytes(req.into_body(), MAX_UPLOAD_BYTES)
            .await
            .map_err(|err| ApiError::BadRequest(format!("Failed to read request body: {err}")))?;
        let payload: InstallPathRequest = serde_json::from_slice(&bytes)
            .map_err(|err| ApiError::BadRequest(format!("Invalid JSON request body: {err}")))?;
        let view =
            install_from_path(&state, Path::new(&payload.path), payload.id.as_deref()).await?;
        Ok(Json(view))
    } else {
        Err(ApiError::BadRequest(format!(
            "Unsupported Content-Type: '{content_type}'. Expected application/json or multipart/form-data"
        )))
    }
}

/// Request body for installing a skill from a local directory path.
#[derive(Debug, Deserialize)]
pub struct InstallPathRequest {
    /// Filesystem path to a directory containing `SKILL.md`.
    pub path: String,
    /// Optional identifier overriding the directory name.
    #[serde(default)]
    pub id: Option<String>,
}

/// Installs from a local directory, requiring an identifier when the directory name is unusable.
async fn install_from_path(
    state: &ApiState,
    source: &Path,
    requested_id: Option<&str>,
) -> Result<SkillView, ApiError> {
    let id = resolve_id(
        requested_id,
        source.file_name().and_then(|name| name.to_str()),
    )?;
    let meta = state
        .skills()
        .install_from_dir(source, &id)
        .map_err(skill_error)?;
    tracing::info!(skill_id = %meta.id, "Skill installed by the control plane");
    // Replacing files does not change the operator's existing enablement choice.
    let enabled = state.plugin_state().is_enabled(SKILL_SECTION, &id).await;
    Ok(SkillView::new(meta, enabled))
}

/// Installs from a zip archive extracted into a scratch directory.
async fn install_from_archive(
    state: &ApiState,
    bytes: &[u8],
    requested_id: Option<&str>,
) -> Result<SkillView, ApiError> {
    let scratch = tempfile::tempdir().map_err(|err| {
        ApiError::Internal(format!("Failed to create a scratch directory: {err}"))
    })?;
    extract_archive(bytes, scratch.path())?;

    // Two layouts are accepted: `SKILL.md` at the archive root, or one wrapping directory — which
    // is what every "zip this folder" UI produces.
    let root = match skill_root(scratch.path()) {
        Some(root) => root,
        None => {
            return Err(ApiError::BadRequest(
                "Archive must contain a SKILL.md, either at its root or inside a single top-level \
                 directory"
                    .to_string(),
            ));
        }
    };

    let fallback = root.file_name().and_then(|name| name.to_str());
    let id = resolve_id(requested_id, fallback)?;
    install_from_path(state, &root, Some(&id)).await
}

/// Resolves the identifier to install under, validating whatever the client supplied.
fn resolve_id(requested: Option<&str>, fallback: Option<&str>) -> Result<String, ApiError> {
    let candidate = requested
        .map(str::to_string)
        .or_else(|| fallback.map(str::to_string))
        .ok_or_else(|| {
            ApiError::BadRequest(
                "Cannot derive a skill identifier; supply one explicitly as 'id'".to_string(),
            )
        })?;
    kanon_core::SkillStore::validate_id(&candidate).map_err(skill_error)
}

/// Returns the directory holding `SKILL.md`, if the extracted archive contains one.
fn skill_root(root: &Path) -> Option<PathBuf> {
    if root.join("SKILL.md").is_file() {
        return Some(root.to_path_buf());
    }

    let mut directories = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false));
    let candidate = directories.next()?.path();
    // More than one top-level directory is ambiguous: guessing would install the wrong tree.
    if directories.next().is_some() {
        return None;
    }
    candidate.join("SKILL.md").is_file().then_some(candidate)
}

/// Extracts a zip archive, rejecting entries that would escape the destination.
fn extract_archive(bytes: &[u8], destination: &Path) -> Result<(), ApiError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|err| ApiError::BadRequest(format!("Invalid zip archive: {err}")))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| ApiError::BadRequest(format!("Corrupt zip entry: {err}")))?;

        // Zip-slip guard: an archive must never be able to write outside the scratch directory.
        let Some(relative) = entry.enclosed_name() else {
            return Err(ApiError::BadRequest(format!(
                "Archive entry '{}' escapes the destination directory",
                entry.name()
            )));
        };
        let target = destination.join(relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|err| {
                ApiError::Internal(format!("Failed to create {}: {err}", target.display()))
            })?;
            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                ApiError::Internal(format!("Failed to create {}: {err}", parent.display()))
            })?;
        }
        let mut buffer = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buffer)
            .map_err(|err| ApiError::BadRequest(format!("Failed to read a zip entry: {err}")))?;
        std::fs::write(&target, buffer).map_err(|err| {
            ApiError::Internal(format!("Failed to write {}: {err}", target.display()))
        })?;
    }

    Ok(())
}

/// Maps a store failure onto the closest HTTP status.
fn skill_error(err: SkillError) -> ApiError {
    match err {
        SkillError::NotFound(_) => ApiError::NotFound(err.to_string()),
        SkillError::InvalidId(_) | SkillError::TooLarge { .. } | SkillError::InvalidSource(_) => {
            ApiError::BadRequest(err.to_string())
        }
        SkillError::Io(_) => ApiError::Internal(err.to_string()),
    }
}
