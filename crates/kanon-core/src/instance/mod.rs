//! Bot instances: the unit an operator actually runs.
//!
//! # Why instances exist
//! An adapter tells the node *where* messages come from; an instance decides *whether and how*
//! they are answered. Without an enabled instance claiming a platform, the node has no bot to
//! answer as, so inbound events are dropped instead of being fed to the model.
//!
//! An instance owns exactly four things:
//!
//! - **adapters**: the platform identifiers it serves. A platform can be claimed by at most one
//!   *enabled* instance, which keeps routing deterministic (no "first match wins" ambiguity);
//! - **persona**: either a persona from the node catalog or a prompt written for this instance;
//! - **model**: an optional override of the node's default model;
//! - **sessions**: conversations answered by this instance are namespaced by it, and the
//!   built-in `/new` command rotates the session of the conversation that issued it while the
//!   previous session is retained (never deleted) for inspection in the console.
//!
//! Instances are persisted as one JSON document (`data/instances.json`) so the node comes back
//! with the same bots after a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kanon_llm::prompt::{Persona, PersonaRegistry};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::conversation::ReplyPolicy;

/// Default location of the instance catalog, relative to the node working directory.
pub const DEFAULT_INSTANCE_CATALOG: &str = "./data/instances.json";

/// Prefix of personas generated from an instance's custom prompt.
///
/// The prefix is the contract between [`sync_instance_personas`] (which writes them) and the
/// pipeline (which points sessions at them).
pub const INSTANCE_PERSONA_PREFIX: &str = "instance:";

/// Failures raised while reading or mutating the instance catalog.
#[derive(Debug, Error)]
pub enum InstanceError {
    /// No instance with the requested identifier exists.
    #[error("instance '{0}' does not exist")]
    NotFound(String),
    /// An enabled instance already claims one of the requested adapters.
    #[error("adapter '{platform}' is already enabled by instance '{owner}'")]
    Conflict {
        /// Platform identifier that is claimed twice.
        platform: String,
        /// Identifier of the instance that already owns it.
        owner: String,
    },
    /// The same platform is claimed by several enabled instances in the stored document.
    #[error("adapter '{platform}' is claimed by multiple enabled instances: {owners:?}")]
    AmbiguousPlatform {
        /// Platform identifier with more than one owner.
        platform: String,
        /// Identifiers of every claiming instance, sorted for determinism.
        owners: Vec<String>,
    },
    /// The submitted instance description is not usable.
    #[error("invalid instance: {0}")]
    Invalid(String),
    /// The catalog could not be read or written.
    #[error("instance catalog I/O failed: {0}")]
    Io(String),
}

/// Per-instance override for a toggleable item (plugin, skill or MCP server).
///
/// The global switch always wins: `Enable` cannot resurrect something the operator disabled
/// node-wide, it only documents an explicit opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemPolicy {
    /// Follow the node-wide switch (default).
    Inherit,
    /// Use the item, provided it is enabled node-wide.
    Enable,
    /// Never use the item for this instance.
    Disable,
}

impl Default for ItemPolicy {
    fn default() -> Self {
        Self::Inherit
    }
}

impl ItemPolicy {
    /// Resolves the policy against the node-wide switch.
    pub fn allows(self, globally_enabled: bool) -> bool {
        globally_enabled && self != Self::Disable
    }
}

/// One bot instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotInstance {
    /// Stable identifier, derived from the name and never reused.
    pub id: String,
    /// Human-readable name shown in the console.
    pub name: String,
    /// Whether the instance accepts messages. A disabled instance processes nothing.
    pub enabled: bool,
    /// Platform identifiers served by this instance.
    #[serde(default)]
    pub adapters: Vec<String>,
    /// Persona identifier from the node catalog, when one is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_id: Option<String>,
    /// Prompt written for this instance; takes precedence over `persona_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Model override; `None` means "use the node's default model".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reply policy override; `None` inherits the node-wide policy.
    ///
    /// Kept per instance because "answer only when mentioned" is a property of a bot, not of the
    /// platform: the same group may host a chatty bot and a quiet one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_policy: Option<ReplyPolicy>,
    /// Per-plugin overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub plugins: HashMap<String, ItemPolicy>,
    /// Per-skill overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub skills: HashMap<String, ItemPolicy>,
    /// Per-MCP-server overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp: HashMap<String, ItemPolicy>,
    /// Conversation key -> session generation, rotated by the built-in `/new` command.
    ///
    /// Kept on the instance so a restart does not silently continue the conversation an operator
    /// already reset.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    session_generations: HashMap<String, u64>,
}

/// Fields an operator can submit when creating or updating an instance.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstanceDraft {
    /// Human-readable name.
    pub name: String,
    /// Whether the instance should accept messages.
    #[serde(default)]
    pub enabled: bool,
    /// Platform identifiers to claim.
    #[serde(default)]
    pub adapters: Vec<String>,
    /// Selected persona from the node catalog.
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Prompt written specifically for this instance.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reply-policy override; absent inherits the node-wide policy.
    #[serde(default)]
    pub reply_policy: Option<ReplyPolicy>,
    /// Per-plugin overrides.
    #[serde(default)]
    pub plugins: HashMap<String, ItemPolicy>,
    /// Per-skill overrides.
    #[serde(default)]
    pub skills: HashMap<String, ItemPolicy>,
    /// Per-MCP-server overrides.
    #[serde(default)]
    pub mcp: HashMap<String, ItemPolicy>,
}

impl BotInstance {
    /// Session identifier for one conversation, including this instance and its generation.
    ///
    /// The generation suffix is what makes `/new` work: the rotated session is a *different* key,
    /// so the previous session keeps its history and stays visible in the console.
    pub fn conversation_session_id(&self, conversation: &str) -> String {
        let generation = self.session_generation(conversation);
        format!("instance:{}:{}#{generation}", self.id, conversation)
    }

    /// Current session generation for a conversation (0 until `/new` is used).
    pub fn session_generation(&self, conversation: &str) -> u64 {
        self.session_generations
            .get(conversation)
            .copied()
            .unwrap_or(0)
    }

    /// Whether this instance may use a plugin, given the node-wide switch.
    pub fn allows_plugin(&self, plugin_id: &str, globally_enabled: bool) -> bool {
        self.plugins
            .get(plugin_id)
            .copied()
            .unwrap_or_default()
            .allows(globally_enabled)
    }

    /// Whether this instance may use a skill, given the node-wide switch.
    pub fn allows_skill(&self, skill_id: &str, globally_enabled: bool) -> bool {
        self.skills
            .get(skill_id)
            .copied()
            .unwrap_or_default()
            .allows(globally_enabled)
    }

    /// Whether this instance may use an MCP server, given the node-wide switch.
    pub fn allows_mcp(&self, server_id: &str, globally_enabled: bool) -> bool {
        self.mcp
            .get(server_id)
            .copied()
            .unwrap_or_default()
            .allows(globally_enabled)
    }

    /// Resolves the instance identifier encoded in a session key, if any.
    ///
    /// Sessions are namespaced as `instance:<id>:<conversation>#<generation>`, which is what lets
    /// hooks and native tools enforce per-instance policy without carrying extra state.
    pub fn instance_id_from_session(session_id: &str) -> Option<&str> {
        session_id
            .strip_prefix("instance:")
            .and_then(|rest| rest.split(':').next())
            .filter(|id| !id.is_empty())
    }

    /// Persona that sessions of this instance must use, if any.
    ///
    /// A prompt written for the instance wins over a catalog persona, and is materialized as a
    /// generated persona (see [`instance_persona_id`]) so the existing prompt-composition path
    /// applies it without a special case in the agent.
    pub fn effective_persona_id(&self) -> Option<String> {
        if self
            .system_prompt
            .as_ref()
            .is_some_and(|p| !p.trim().is_empty())
        {
            return Some(instance_persona_id(&self.id));
        }
        self.persona_id.clone()
    }

    /// Reply policy that governs this instance, given the node-wide default.
    pub fn effective_reply_policy(&self, node_policy: ReplyPolicy) -> ReplyPolicy {
        self.reply_policy.unwrap_or(node_policy)
    }
}

/// Identifier of the generated persona backing an instance's custom prompt.
pub fn instance_persona_id(instance_id: &str) -> String {
    format!("{INSTANCE_PERSONA_PREFIX}{instance_id}")
}

/// Registers a persona for every instance that carries a custom prompt, and drops generated
/// personas that no longer correspond to one.
///
/// Called at startup and after every catalog mutation so the persona catalog always mirrors the
/// instance catalog; the console therefore shows instance prompts alongside built-in personas.
pub fn sync_instance_personas(instances: &[BotInstance], personas: &PersonaRegistry) {
    let mut desired: HashMap<String, (&str, &str)> = HashMap::new();
    for instance in instances {
        if let Some(prompt) = instance
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            desired.insert(
                instance_persona_id(&instance.id),
                (instance.name.as_str(), prompt),
            );
        }
    }

    for (id, (name, prompt)) in &desired {
        personas.register(Persona::new(
            id.clone(),
            format!("{name} (instance)"),
            format!("Persona prompt configured on bot instance '{name}'"),
            *prompt,
        ));
    }

    // A prompt that was cleared must not linger as a selectable persona.
    for persona in personas.list() {
        if persona.id.starts_with(INSTANCE_PERSONA_PREFIX)
            && !desired.contains_key(&persona.id)
            && let Some(stale) = personas.remove(&persona.id)
        {
            tracing::debug!(persona_id = %stale.id, "Removed generated instance persona");
        }
    }
}

/// Document persisted at the catalog path.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceCatalogDocument {
    /// Schema version, so a future format change can be migrated explicitly.
    version: u32,
    /// Persisted instances, in stable order.
    instances: Vec<BotInstance>,
}

impl Default for InstanceCatalogDocument {
    fn default() -> Self {
        Self {
            version: 1,
            instances: Vec::new(),
        }
    }
}

/// Thread-safe catalog of bot instances, optionally persisted as one JSON document.
///
/// A catalog without a path is *in-memory*: it behaves identically within the process but writes
/// nothing. Tests and embedded cores use that mode so they can never touch a real node's data
/// directory.
pub struct InstanceRegistry {
    /// Path of the persisted document; `None` for an in-memory catalog.
    path: Option<PathBuf>,
    /// Instances by identifier.
    instances: RwLock<HashMap<String, BotInstance>>,
}

impl std::fmt::Debug for InstanceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceRegistry")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Default for InstanceRegistry {
    /// An in-memory catalog: the safe default for tests and embedded cores.
    fn default() -> Self {
        Self::in_memory()
    }
}

impl InstanceRegistry {
    /// Opens (or creates) the catalog at `path`.
    ///
    /// A missing file is an empty catalog; a malformed file is an error, because silently
    /// starting with no instances would look exactly like "no bot is configured" and leave the
    /// operator chasing a phantom routing problem.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, InstanceError> {
        let path = path.into();
        let instances = match Self::read_document(&path)? {
            Some(document) => document
                .instances
                .into_iter()
                .map(|instance| (instance.id.clone(), instance))
                .collect(),
            None => HashMap::new(),
        };

        let registry = Self {
            path: Some(path),
            instances: RwLock::new(instances),
        };

        // Refuse to run on an ambiguous document instead of routing arbitrarily.
        registry.validate_all().await?;
        Ok(registry)
    }

    /// Creates a catalog that lives only for this process.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            instances: RwLock::new(HashMap::new()),
        }
    }

    /// Path of the persisted catalog, or `None` for an in-memory catalog.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Every instance, ordered by name then id for stable console output.
    pub async fn list(&self) -> Vec<BotInstance> {
        let mut instances: Vec<BotInstance> =
            self.instances.read().await.values().cloned().collect();
        instances.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        instances
    }

    /// Looks up an instance by identifier.
    pub async fn get(&self, id: &str) -> Option<BotInstance> {
        self.instances.read().await.get(id).cloned()
    }

    /// Number of configured instances.
    pub async fn len(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Whether no instance is configured.
    pub async fn is_empty(&self) -> bool {
        self.instances.read().await.is_empty()
    }

    /// Creates an instance and returns the stored record.
    pub async fn create(&self, draft: InstanceDraft) -> Result<BotInstance, InstanceError> {
        let mut instances = self.instances.write().await;

        let name = normalize_name(&draft.name)?;
        let id = unique_id(&instances, &name);
        let candidate = build_instance(id, name, draft)?;

        Self::validate_claims(&instances, &candidate)?;
        instances.insert(candidate.id.clone(), candidate.clone());
        Self::persist(self.path.as_deref(), &instances)?;

        Ok(candidate)
    }

    /// Replaces the editable fields of an instance, keeping its identity and session history.
    pub async fn update(
        &self,
        id: &str,
        draft: InstanceDraft,
    ) -> Result<BotInstance, InstanceError> {
        let mut instances = self.instances.write().await;

        if !instances.contains_key(id) {
            return Err(InstanceError::NotFound(id.to_string()));
        }

        let name = normalize_name(&draft.name)?;
        let mut candidate = build_instance(id.to_string(), name, draft)?;
        // Session generations are runtime state owned by the instance, never by a form submit.
        candidate.session_generations = instances
            .get(id)
            .map(|existing| existing.session_generations.clone())
            .unwrap_or_default();

        Self::validate_claims(&instances, &candidate)?;
        instances.insert(candidate.id.clone(), candidate.clone());
        Self::persist(self.path.as_deref(), &instances)?;

        Ok(candidate)
    }

    /// Deletes an instance.
    pub async fn delete(&self, id: &str) -> Result<(), InstanceError> {
        let mut instances = self.instances.write().await;
        if instances.remove(id).is_none() {
            return Err(InstanceError::NotFound(id.to_string()));
        }
        Self::persist(self.path.as_deref(), &instances)
    }

    /// Resolves the enabled instance that serves `platform`.
    ///
    /// Returns `Ok(None)` when no enabled instance claims the platform — the caller must then drop
    /// the event, because there is no bot to answer as.
    pub async fn resolve_by_platform(
        &self,
        platform: &str,
    ) -> Result<Option<BotInstance>, InstanceError> {
        let instances = self.instances.read().await;
        let platform = platform.trim();

        let mut owners: Vec<&BotInstance> = instances
            .values()
            .filter(|instance| instance.enabled)
            .filter(|instance| instance.adapters.iter().any(|a| a == platform))
            .collect();

        owners.sort_by(|a, b| a.id.cmp(&b.id));
        match owners.as_slice() {
            [] => Ok(None),
            [owner] => Ok(Some((*owner).clone())),
            many => Err(InstanceError::AmbiguousPlatform {
                platform: platform.to_string(),
                owners: many.iter().map(|instance| instance.id.clone()).collect(),
            }),
        }
    }

    /// Rotates the session of one conversation and returns the new session identifier.
    ///
    /// The previous session is left untouched: its history remains in the session catalog.
    pub async fn rotate_session(
        &self,
        id: &str,
        conversation: &str,
    ) -> Result<String, InstanceError> {
        let mut instances = self.instances.write().await;
        let instance = instances
            .get_mut(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;

        let next = instance.session_generation(conversation) + 1;
        instance
            .session_generations
            .insert(conversation.to_string(), next);
        let session_id = instance.conversation_session_id(conversation);
        Self::persist(self.path.as_deref(), &instances)?;

        Ok(session_id)
    }

    /// Replaces the model override of one instance and persists the catalog.
    ///
    /// Used by the built-in `/model` command: switching a model must survive a restart, otherwise
    /// a conversation would silently drift back to the previous model after a reboot.
    pub async fn set_model(
        &self,
        id: &str,
        model: Option<String>,
    ) -> Result<BotInstance, InstanceError> {
        let mut instances = self.instances.write().await;
        let instance = instances
            .get_mut(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;

        instance.model = model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        let updated = instance.clone();
        Self::persist(self.path.as_deref(), &instances)?;

        Ok(updated)
    }

    /// Validates that no two enabled instances claim the same platform.
    async fn validate_all(&self) -> Result<(), InstanceError> {
        let instances = self.instances.read().await;
        for instance in instances.values() {
            Self::validate_claims(&instances, instance)?;
        }
        Ok(())
    }

    /// Validates a candidate instance against the rest of the catalog.
    fn validate_claims(
        instances: &HashMap<String, BotInstance>,
        candidate: &BotInstance,
    ) -> Result<(), InstanceError> {
        if !candidate.enabled {
            // A disabled instance serves nothing, so it cannot collide with anyone.
            return Ok(());
        }

        for platform in &candidate.adapters {
            if let Some(owner) = instances
                .values()
                .filter(|other| other.id != candidate.id && other.enabled)
                .find(|other| other.adapters.iter().any(|a| a == platform))
            {
                return Err(InstanceError::Conflict {
                    platform: platform.clone(),
                    owner: owner.id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Reads the catalog document, returning `None` when the file does not exist.
    fn read_document(path: &Path) -> Result<Option<InstanceCatalogDocument>, InstanceError> {
        if !path.exists() {
            return Ok(None);
        }

        let raw = std::fs::read_to_string(path).map_err(|err| {
            InstanceError::Io(format!("failed to read {}: {err}", path.display()))
        })?;
        let document: InstanceCatalogDocument = serde_json::from_str(&raw).map_err(|err| {
            InstanceError::Io(format!("failed to parse {}: {err}", path.display()))
        })?;

        Ok(Some(document))
    }

    /// Atomically writes the catalog so a crash cannot truncate it.
    ///
    /// An in-memory catalog has no path to write to: that is its documented mode, not a failure.
    fn persist(
        path: Option<&Path>,
        instances: &HashMap<String, BotInstance>,
    ) -> Result<(), InstanceError> {
        let Some(path) = path else {
            return Ok(());
        };

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                InstanceError::Io(format!("failed to create {}: {err}", parent.display()))
            })?;
        }

        let mut ordered: Vec<BotInstance> = instances.values().cloned().collect();
        ordered.sort_by(|a, b| a.id.cmp(&b.id));

        let document = InstanceCatalogDocument {
            version: 1,
            instances: ordered,
        };
        let payload = serde_json::to_string_pretty(&document)
            .map_err(|err| InstanceError::Io(format!("failed to serialize catalog: {err}")))?;

        let temp_path = path.with_extension("json.tmp");
        std::fs::write(&temp_path, payload).map_err(|err| {
            InstanceError::Io(format!("failed to write {}: {err}", temp_path.display()))
        })?;
        std::fs::rename(&temp_path, path).map_err(|err| {
            InstanceError::Io(format!(
                "failed to move {} into place at {}: {err}",
                temp_path.display(),
                path.display()
            ))
        })
    }
}

/// Normalizes and validates a submitted instance name.
fn normalize_name(raw: &str) -> Result<String, InstanceError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(InstanceError::Invalid("name must not be empty".to_string()));
    }
    if name.chars().count() > 64 {
        return Err(InstanceError::Invalid(
            "name must be at most 64 characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

/// Builds the persisted record from a draft, normalizing every optional field.
fn build_instance(
    id: String,
    name: String,
    draft: InstanceDraft,
) -> Result<BotInstance, InstanceError> {
    let mut adapters: Vec<String> = Vec::new();
    for adapter in draft.adapters {
        let platform = adapter.trim();
        if platform.is_empty() {
            return Err(InstanceError::Invalid(
                "adapter identifiers must not be empty".to_string(),
            ));
        }
        if !adapters.iter().any(|existing| existing == platform) {
            adapters.push(platform.to_string());
        }
    }

    let model = draft
        .model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());

    let system_prompt = draft
        .system_prompt
        .map(|prompt| prompt.trim().to_string())
        .filter(|prompt| !prompt.is_empty());

    let persona_id = draft
        .persona_id
        .map(|persona| persona.trim().to_string())
        .filter(|persona| !persona.is_empty());

    if let Some(policy) = draft.reply_policy.as_ref() {
        policy.validate().map_err(InstanceError::Invalid)?;
    }

    Ok(BotInstance {
        id,
        name,
        enabled: draft.enabled,
        adapters,
        persona_id,
        system_prompt,
        model,
        reply_policy: draft.reply_policy,
        plugins: draft.plugins,
        skills: draft.skills,
        mcp: draft.mcp,
        session_generations: HashMap::new(),
    })
}

/// Derives a unique, readable identifier from an instance name.
fn unique_id(instances: &HashMap<String, BotInstance>, name: &str) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    // Non-ASCII names (e.g. 黑猪AI) slugify to nothing, so fall back to a stable prefix.
    let base = if slug.is_empty() {
        "bot".to_string()
    } else {
        slug
    };

    if !instances.contains_key(&base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !instances.contains_key(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}
