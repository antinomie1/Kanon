//! Skills: operator-installed instruction bundles the model can pull in on demand.
//!
//! # Layout and why it is progressive
//! A skill is a directory under `data/skills/<id>/` containing a `SKILL.md` file: a short front
//! matter block (`name`, `description`) followed by the full instructions.
//!
//! Only the name and description reach the model's system prompt (see [`SkillCatalogHook`]); the
//! body is fetched with the [`ReadSkillTool`] (`read_skill`) once the model decides the skill is
//! relevant. Injecting every skill in full would spend the context window on instructions the
//! current conversation never needs — which is exactly the problem the catalog/read split solves.
//!
//! # Policy
//! Global enablement lives in the [`ToggleStore`] (`skills` section); a bot instance may further
//! restrict a skill with its own [`ItemPolicy`](crate::instance::ItemPolicy). Both are enforced
//! here, keyed off the instance identifier encoded in the session key, so a shared agent cannot
//! leak one instance's skills into another's conversation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kanon_llm::agent::{AgentTool, ToolOutput};
use kanon_llm::gateway::types::ToolDefinition;
use thiserror::Error;

use crate::instance::InstanceRegistry;
use crate::toggle::{SKILL_SECTION, ToggleStore};

/// Default directory holding installed skills, relative to the node working directory.
pub const DEFAULT_SKILLS_DIR: &str = "./data/skills";

/// Largest skill body handed to the model, in bytes.
///
/// A skill is instructions, not a dataset: anything larger belongs in a tool or a file the model
/// reads selectively. The cap keeps one `read_skill` call from consuming the whole context window.
pub const MAX_SKILL_BYTES: usize = 64 * 1024;

/// Failures raised while scanning or reading skills.
#[derive(Debug, Error)]
pub enum SkillError {
    /// No skill with the requested identifier is installed.
    #[error("skill '{0}' is not installed")]
    NotFound(String),
    /// The requested identifier cannot be a directory name.
    #[error("invalid skill identifier '{0}'")]
    InvalidId(String),
    /// The supplied source is not an installable skill.
    ///
    /// A client mistake rather than a storage failure, so the gateway can answer `400` instead of
    /// blaming the node for an archive the operator picked.
    #[error("{0}")]
    InvalidSource(String),
    /// The skill body is too large to hand to the model.
    #[error("skill '{id}' exceeds the {limit} byte limit ({size} bytes read)")]
    TooLarge {
        /// Skill identifier.
        id: String,
        /// Observed bytes; reading stops once the limit has been exceeded.
        size: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The skills directory could not be read or written.
    #[error("skills storage failed: {0}")]
    Io(String),
}

/// One installed skill.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SkillMeta {
    /// Identifier (directory name).
    pub id: String,
    /// Human/LLM readable name.
    pub name: String,
    /// One-line description used in the model's catalog.
    pub description: String,
    /// Whether the operator enabled this skill node-wide.
    pub enabled: bool,
}

/// Installed skills rooted at one directory.
#[derive(Debug)]
pub struct SkillStore {
    /// Directory holding one sub-directory per skill.
    root: PathBuf,
    /// Readers must not observe the gap between parking the old directory and publishing its replacement.
    files: RwLock<()>,
}

impl SkillStore {
    /// Creates a store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            files: RwLock::new(()),
        }
    }

    /// Root directory of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lists every installed skill, ordered by identifier.
    ///
    /// Unreadable skills are logged and skipped. Hidden staging directories and unusable identifiers
    /// are never advertised as callable skills.
    pub fn list(&self) -> Result<Vec<SkillMeta>, SkillError> {
        let _reading = self
            .files
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(SkillError::Io(format!(
                    "failed to read {}: {error}",
                    self.root.display()
                )));
            }
        };

        let mut skills = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|err| SkillError::Io(format!("failed to read a skill entry: {err}")))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !matches!(Self::validate_id(id), Ok(valid) if valid == id) {
                continue;
            }

            match read_skill(&path, id) {
                Ok((name, description)) => skills.push(SkillMeta {
                    id: id.to_string(),
                    name,
                    description,
                    // Filled by the caller from the toggle store: enablement is operator state,
                    // not part of the skill on disk.
                    enabled: true,
                }),
                Err(err) => {
                    tracing::warn!(skill_id = %id, error = %err, "Ignoring unreadable skill");
                }
            }
        }

        skills.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(skills)
    }

    /// Reads the full body of one skill.
    pub fn read(&self, id: &str) -> Result<String, SkillError> {
        let path = self.skill_path(id)?;
        let _reading = self
            .files
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        read_body(&path, id)
    }

    /// Validates and normalizes a skill identifier supplied by a client.
    pub fn validate_id(id: &str) -> Result<String, SkillError> {
        let id = id.trim();
        if id.is_empty()
            || id.len() > 64
            || id.starts_with('.')
            || !id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
        {
            return Err(SkillError::InvalidId(id.to_string()));
        }
        Ok(id.to_string())
    }

    /// Removes an installed skill.
    pub fn remove(&self, id: &str) -> Result<(), SkillError> {
        let path = self.skill_path(id)?;
        let _writing = self
            .files
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::fs::remove_dir_all(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                SkillError::NotFound(id.to_string())
            } else {
                SkillError::Io(format!("failed to remove skill '{id}': {err}"))
            }
        })
    }

    /// Installs a skill by copying a prepared directory into the store.
    ///
    /// Archive extraction stays outside this type (the management gateway owns upload formats),
    /// so the store only has to accept a directory that already contains `SKILL.md`.
    pub fn install_from_dir(&self, source: &Path, id: &str) -> Result<SkillMeta, SkillError> {
        let id = Self::validate_id(id)?;
        let _writing = self
            .files
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !source.join("SKILL.md").is_file() {
            return Err(SkillError::InvalidSource(format!(
                "'{}' does not contain a SKILL.md file",
                source.display()
            )));
        }

        // Validate before copying, and again from the finished candidate: a mutable source may
        // change during the copy. No candidate failure may delete a working installed version.
        read_skill(source, &id)?;
        std::fs::create_dir_all(&self.root)
            .map_err(|err| SkillError::Io(format!("failed to create skills directory: {err}")))?;
        let source = source
            .canonicalize()
            .map_err(|err| SkillError::Io(err.to_string()))?;
        let root = self
            .root
            .canonicalize()
            .map_err(|err| SkillError::Io(err.to_string()))?;
        if root.starts_with(&source) {
            return Err(SkillError::InvalidSource(
                "skill source must not contain the destination store".into(),
            ));
        }
        let staging = tempfile::Builder::new()
            .prefix(".kanon-skill-")
            .tempdir_in(&root)
            .map_err(|err| SkillError::Io(format!("failed to stage skill '{id}': {err}")))?;
        copy_dir(&source, staging.path())?;
        let (name, description) = read_skill(staging.path(), &id)?;
        crate::directory::swap_into_place(staging, &root.join(&id), &root)
            .map_err(|err| SkillError::Io(err.to_string()))?;
        Ok(SkillMeta {
            id,
            name,
            description,
            enabled: true,
        })
    }

    /// Absolute path of one skill directory.
    fn skill_path(&self, id: &str) -> Result<PathBuf, SkillError> {
        Ok(self.root.join(Self::validate_id(id)?))
    }
}

/// Reads the same bounded UTF-8 body for installation, discovery and tool calls.
fn read_body(dir: &Path, id: &str) -> Result<String, SkillError> {
    let file = std::fs::File::open(dir.join("SKILL.md")).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            SkillError::NotFound(id.to_string())
        } else {
            SkillError::Io(format!("failed to read skill '{id}': {err}"))
        }
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_SKILL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| SkillError::Io(format!("failed to read skill '{id}': {err}")))?;
    if bytes.len() > MAX_SKILL_BYTES {
        return Err(SkillError::TooLarge {
            id: id.to_string(),
            size: bytes.len(),
            limit: MAX_SKILL_BYTES,
        });
    }
    String::from_utf8(bytes)
        .map_err(|err| SkillError::InvalidSource(format!("skill '{id}' is not UTF-8: {err}")))
}

/// Reads the front matter of a skill, falling back to sensible defaults.
fn read_skill(dir: &Path, id: &str) -> Result<(String, String), SkillError> {
    let body = read_body(dir, id)?;

    let mut name = id.to_string();
    let mut description = String::new();
    let mut in_front_matter = false;

    for (index, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        if index == 0 && trimmed == "---" {
            in_front_matter = true;
            continue;
        }
        if in_front_matter && trimmed == "---" {
            break;
        }
        if in_front_matter {
            if let Some(value) = trimmed.strip_prefix("name:") {
                name = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
            } else if let Some(value) = trimmed.strip_prefix("description:") {
                description = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
            }
            continue;
        }
        // No front matter: the first prose line becomes the description.
        if description.is_empty() && !trimmed.is_empty() && !trimmed.starts_with('#') {
            description = trimmed.to_string();
        }
    }

    if description.is_empty() {
        description = format!("Instructions from skill '{name}'");
    }
    // Keep the catalog line to one sentence so a large skill cannot bloat the system prompt.
    if description.chars().count() > 200 {
        description = description.chars().take(200).collect::<String>() + "…";
    }

    Ok((name, description))
}

/// Recursively copies a directory tree.
fn copy_dir(source: &Path, target: &Path) -> Result<(), SkillError> {
    std::fs::create_dir_all(target)
        .map_err(|err| SkillError::Io(format!("failed to create {}: {err}", target.display())))?;

    let entries = std::fs::read_dir(source)
        .map_err(|err| SkillError::Io(format!("failed to read {}: {err}", source.display())))?;

    for entry in entries {
        let entry =
            entry.map_err(|err| SkillError::Io(format!("failed to read a skill entry: {err}")))?;
        let path = entry.path();
        let destination = target.join(entry.file_name());

        let kind = entry
            .file_type()
            .map_err(|err| SkillError::Io(format!("failed to read {}: {err}", path.display())))?;
        if kind.is_dir() {
            copy_dir(&path, &destination)?;
        } else if kind.is_file() {
            std::fs::copy(&path, &destination).map_err(|err| {
                SkillError::Io(format!("failed to copy {}: {err}", path.display()))
            })?;
        } else {
            // Following a directory symlink could recursively copy the staging directory itself.
            return Err(SkillError::InvalidSource(format!(
                "skill source contains a symlink or special file: {}",
                path.display()
            )));
        }
    }

    Ok(())
}

/// Renders the compact skill catalog injected into the system prompt.
pub fn catalog_prompt(skills: &[SkillMeta]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut catalog = String::from(
        "You have these skills available. Load one with the read_skill tool when it matches the \
         user's request:\n",
    );
    for skill in skills {
        catalog.push_str(&format!("- {}: {}\n", skill.id, skill.description));
    }
    catalog
}

/// Skills this instance may use, honouring the node-wide switch and its own overrides.
pub async fn allowed_skills(
    store: &SkillStore,
    toggles: &ToggleStore,
    instances: &InstanceRegistry,
    instance_id: Option<&str>,
) -> Vec<SkillMeta> {
    let instance = match instance_id {
        Some(id) => instances.get(id).await,
        None => None,
    };

    let installed = match store.list() {
        Ok(skills) => skills,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to list skills");
            return Vec::new();
        }
    };

    let mut allowed = Vec::new();
    for mut skill in installed {
        let globally_enabled = toggles.is_enabled(SKILL_SECTION, &skill.id).await;
        let instance_allows = instance
            .as_ref()
            .map(|instance| instance.allows_skill(&skill.id, globally_enabled))
            // No instance (console sandbox): the node-wide switch alone decides.
            .unwrap_or(globally_enabled);
        if instance_allows {
            skill.enabled = true;
            allowed.push(skill);
        }
    }
    allowed
}

/// Native tool that returns the full body of one skill.
pub struct ReadSkillTool {
    /// Installed skills.
    store: Arc<SkillStore>,
    /// Node-wide enable switches.
    toggles: Arc<ToggleStore>,
    /// Bot instances carrying per-instance overrides.
    instances: Arc<InstanceRegistry>,
}

impl ReadSkillTool {
    /// Creates the tool over the node's skill storage.
    pub fn new(
        store: Arc<SkillStore>,
        toggles: Arc<ToggleStore>,
        instances: Arc<InstanceRegistry>,
    ) -> Self {
        Self {
            store,
            toggles,
            instances,
        }
    }
}

#[async_trait]
impl AgentTool for ReadSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_skill".to_string(),
            description:
                "Reads the full instructions of one available skill. Call this when the skill \
                 catalog in your instructions matches the user's request."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill identifier as listed in the available skills catalog"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(
        &self,
        session_id: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, String> {
        self.read(session_id, arguments).await.map(ToolOutput::from)
    }
}

impl ReadSkillTool {
    /// Returns the body of the requested skill, if this session's instance may use it.
    async fn read(&self, session_id: &str, arguments: serde_json::Value) -> Result<String, String> {
        let requested = arguments
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required argument 'name'".to_string())?;

        let instance_id = crate::instance::BotInstance::instance_id_from_session(session_id);
        let allowed =
            allowed_skills(&self.store, &self.toggles, &self.instances, instance_id).await;

        // Enforced here as well as in the catalog: the model may invent a skill name, and an
        // instance that disabled a skill must not receive it through a guessed name.
        if !allowed.iter().any(|skill| skill.id == requested) {
            return Err(format!(
                "Skill '{requested}' is not available for this conversation"
            ));
        }

        self.store.read(requested).map_err(|err| err.to_string())
    }
}

/// Hook that snapshots the per-instance skill catalog into each turn's static system text.
pub struct SkillCatalogHook {
    /// Installed skills.
    store: Arc<SkillStore>,
    /// Node-wide enable switches.
    toggles: Arc<ToggleStore>,
    /// Bot instances carrying per-instance overrides.
    instances: Arc<InstanceRegistry>,
}

impl SkillCatalogHook {
    /// Creates the hook over the node's skill storage.
    pub fn new(
        store: Arc<SkillStore>,
        toggles: Arc<ToggleStore>,
        instances: Arc<InstanceRegistry>,
    ) -> Self {
        Self {
            store,
            toggles,
            instances,
        }
    }
}

#[async_trait]
impl kanon_llm::agent::AgentHook for SkillCatalogHook {
    async fn on_system_prompt(
        &self,
        session_id: &str,
        prompt: &mut String,
    ) -> Result<(), kanon_llm::AgentError> {
        let instance_id = crate::instance::BotInstance::instance_id_from_session(session_id);
        let allowed =
            allowed_skills(&self.store, &self.toggles, &self.instances, instance_id).await;

        // Debug-level: the console shows the effective catalog through /api/v1/skills, but an
        // operator chasing "why does the model not know my skill" needs the request-time view.
        tracing::debug!(
            session_id = %session_id,
            instance_id = ?instance_id,
            allowed = allowed.len(),
            "Skill catalog prepared for agent turn"
        );

        let catalog = catalog_prompt(&allowed);
        if catalog.is_empty() {
            return Ok(());
        }

        // The turn retains this text through its tool rounds. A hot edit becomes visible on the
        // next turn, without changing the prefix halfway through the current conversation turn.
        if !prompt.trim().is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&catalog);
        Ok(())
    }
}
