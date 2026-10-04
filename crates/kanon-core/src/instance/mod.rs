//! Bot instances: the unit an operator actually runs.
//!
//! # Why instances exist
//! An adapter tells the node *where* messages come from; an instance decides *whether and how*
//! they are answered. Without an enabled instance claiming a platform, the node has no bot to
//! answer as, so inbound events are dropped instead of being fed to the model.
//!
//! An instance owns, among its policy overrides:
//!
//! - **adapters**: the platform identifiers it serves. A platform can be claimed by at most one
//!   *enabled* instance, which keeps routing deterministic (no "first match wins" ambiguity);
//! - **persona**: either a persona from the node catalog or a prompt written for this instance;
//! - **agent**: an optional override of the node's default agent (see
//!   [`kanon_llm::selectable_agents`]);
//! - **model**: an optional override of the node's default model;
//! - **sessions**: conversations answered by this instance are namespaced by it. A chat can hold
//!   several conversations (`/new`, `/ls`, `/switch`, `/del`); the instance remembers which one
//!   is current, and the others keep their history until they are deleted.
//!
//! Instances are persisted as one JSON document (`data/instances.json`) so the node comes back
//! with the same bots after a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kanon_llm::prompt::{Persona, PersonaError, PersonaRegistry};
use kanon_llm::{SessionManager, error::MemoryError};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::access::CommandPolicy;
use crate::conversation::{ContextPolicy, ReplyPolicy};

/// Default location of the instance catalog, relative to the node working directory.
pub const DEFAULT_INSTANCE_CATALOG: &str = "./data/instances.json";

/// Prefix of personas generated from an instance's custom prompt.
///
/// The prefix is the contract between [`restore_instance_personas`] (which writes them) and the
/// pipeline (which resolves them for individual turns).
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
    /// A generated persona is still selected by another surviving instance.
    #[error(
        "persona '{persona}' is used by instance(s) {instances:?}; select another persona first"
    )]
    PersonaInUse {
        /// Generated persona that would be removed.
        persona: String,
        /// Surviving instances that still select it.
        instances: Vec<String>,
    },
    /// Persona publication or lookup failed.
    #[error(transparent)]
    Persona(#[from] PersonaError),
    /// A bound session is busy or its binding could not be durably cleared.
    #[error(transparent)]
    Session(#[from] MemoryError),
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

/// Whose conversation a group message belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionScope {
    /// Each member of a group has a private session with the bot (the default): memories and
    /// `/new` never affect anyone else.
    #[default]
    User,
    /// The whole group shares one session, so the bot follows a discussion between several
    /// people. Every message is labelled with its speaker.
    Group,
}

/// Where an instance lets its administrators run Bash; the node-wide Bash switch still wins.
///
/// "Own context" means the model sees only the administrator's own conversation: a private chat,
/// or a per-member group session in a group the instance does not observe. A shared or observed
/// group session also carries other members' words, and those can steer the commands the model
/// runs on the administrator's behalf — so allowing it is a separate, explicit choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashScope {
    /// Never, whatever the node-wide switch says.
    Disabled,
    /// Only in conversations whose context is the administrator's own (the default).
    #[default]
    OwnContext,
    /// Also in shared or observed group sessions.
    SharedContext,
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
    /// Agent override; `None` means "use the node's default agent".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Model override; `None` means "use the node's default model".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reply policy override; `None` inherits the node-wide policy.
    ///
    /// Kept per instance because "answer only when mentioned" is a property of a bot, not of the
    /// platform: the same group may host a chatty bot and a quiet one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_policy: Option<ReplyPolicy>,
    /// Context-extras override; `None` inherits the node-wide policy.
    ///
    /// Controls whether the sender id and the message time are prepended to the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_policy: Option<ContextPolicy>,
    /// Whether group sessions are per member or shared by the whole group.
    #[serde(default)]
    pub session_scope: SessionScope,
    /// Record group messages the bot does not answer, and show the model what was said since its
    /// last turn when it is next addressed. Needs an adapter that delivers every group message.
    #[serde(default)]
    pub observe_group: bool,
    /// Command-permission override; `None` inherits the node-wide policy.
    ///
    /// It replaces the node policy as a whole, administrators included: two bots on one node can
    /// serve different communities, and each community has its own administrators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_policy: Option<CommandPolicy>,
    /// Where this instance's administrators may run Bash.
    #[serde(default)]
    pub bash: BashScope,
    /// Per-plugin overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub plugins: HashMap<String, ItemPolicy>,
    /// Per-skill overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub skills: HashMap<String, ItemPolicy>,
    /// Per-MCP-server overrides; absent identifiers inherit the node-wide switch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp: HashMap<String, ItemPolicy>,
    /// Conversation key -> current session generation, chosen by `/new`, `/switch` and `/del`.
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
    /// Optional agent override; absent inherits the node's default agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Optional model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reply-policy override; absent inherits the node-wide policy.
    #[serde(default)]
    pub reply_policy: Option<ReplyPolicy>,
    /// Optional context-extras override; absent inherits the node-wide policy.
    #[serde(default)]
    pub context_policy: Option<ContextPolicy>,
    /// Per-member or shared group sessions.
    #[serde(default)]
    pub session_scope: SessionScope,
    /// Whether unanswered group messages are shown to the model when it is next addressed.
    #[serde(default)]
    pub observe_group: bool,
    /// Optional command-permission override; absent inherits the node-wide policy.
    #[serde(default)]
    pub command_policy: Option<CommandPolicy>,
    /// Where this instance's administrators may run Bash.
    #[serde(default)]
    pub bash: BashScope,
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
        self.session_id_at(conversation, self.session_generation(conversation))
    }

    /// Session identifier of one generation of a conversation.
    pub fn session_id_at(&self, conversation: &str, generation: u64) -> String {
        format!(
            "{}{generation}",
            self.conversation_session_prefix(conversation)
        )
    }

    /// The part every session of a conversation shares: everything before the generation.
    ///
    /// Ends with `#`, so `instance:a:chat#` never matches the sessions of `instance:a:chat:x`.
    pub fn conversation_session_prefix(&self, conversation: &str) -> String {
        format!("instance:{}:{}#", self.id, conversation)
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

    /// Context-extras policy that governs this instance, given the node-wide default.
    pub fn effective_context_policy(&self, node_policy: ContextPolicy) -> ContextPolicy {
        self.context_policy.unwrap_or(node_policy)
    }

    /// Command permissions and administrators that govern this instance, given the node-wide
    /// default.
    pub fn effective_command_policy(&self, node_policy: CommandPolicy) -> CommandPolicy {
        self.command_policy.clone().unwrap_or(node_policy)
    }
}

/// Identifier of the generated persona backing an instance's custom prompt.
pub fn instance_persona_id(instance_id: &str) -> String {
    format!("{INSTANCE_PERSONA_PREFIX}{instance_id}")
}

/// Restores generated personas and validates bindings before the node starts serving requests.
///
/// All generated personas are published before checking cross-instance references. Missing
/// generated targets in legacy sessions follow deleted-owner semantics and are durably unbound;
/// valid bindings are preserved without guessing whether an older node inherited or selected them.
/// Unknown custom references and failed cleanup stop startup rather than silently changing prompts.
pub fn restore_instance_personas(
    instances: &[BotInstance],
    personas: &PersonaRegistry,
    sessions: &SessionManager,
) -> Result<(), InstanceError> {
    let generated = instances
        .iter()
        .filter_map(generated_persona)
        .collect::<Vec<_>>();
    for persona in &generated {
        persona.validate()?;
    }
    for persona in generated {
        personas.register(persona)?;
    }
    for instance in instances {
        InstanceRegistry::validate_persona(instance, Some(personas))?;
    }

    let mut missing_generated = std::collections::BTreeSet::new();
    for session in sessions.list_sessions() {
        let Some(id) = session.persona_id else {
            continue;
        };
        if personas.get(&id).is_some() {
            continue;
        }
        if id.starts_with(INSTANCE_PERSONA_PREFIX) {
            missing_generated.insert(id);
        } else {
            return Err(InstanceError::Invalid(format!(
                "session '{}' refers to unknown persona '{id}'",
                session.session_key
            )));
        }
    }
    for id in missing_generated {
        let unbound = sessions.unbind_persona(&id)?;
        tracing::warn!(persona_id = %id, unbound_sessions = unbound,
            "Removed stale session bindings to a deleted instance persona during startup");
    }
    Ok(())
}

/// Builds the generated persona owned by one instance, when it has its own prompt.
fn generated_persona(instance: &BotInstance) -> Option<Persona> {
    instance
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| {
            Persona::instance(
                instance_persona_id(&instance.id),
                format!("{} (instance)", instance.name),
                format!(
                    "Persona prompt configured on bot instance '{}'",
                    instance.name
                ),
                prompt,
            )
        })
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

    /// Snapshots the current instance persona while its configuration cannot change.
    ///
    /// The owned persona keeps a turn's prefix stable after this short read lock is released.
    /// Standalone instances need no catalog when they select no persona; a configured persona
    /// must resolve explicitly, never through an earlier instance snapshot or implicit fallback.
    pub async fn persona_for_instance(
        &self,
        id: &str,
        personas: Option<&PersonaRegistry>,
    ) -> Result<Option<Persona>, InstanceError> {
        let instances = self.instances.read().await;
        let instance = instances
            .get(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;
        let Some(persona_id) = instance.effective_persona_id() else {
            return Ok(None);
        };
        let personas = personas.ok_or_else(|| {
            InstanceError::Invalid(format!(
                "instance '{id}' selects persona '{persona_id}' but has no persona catalog"
            ))
        })?;
        personas
            .get(&persona_id)
            .map(Some)
            .ok_or_else(|| PersonaError::NotFound(persona_id).into())
    }

    /// Number of configured instances.
    pub async fn len(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Whether no instance is configured.
    pub async fn is_empty(&self) -> bool {
        self.instances.read().await.is_empty()
    }

    /// Creates an instance, validating and publishing personas under the instance write lock.
    ///
    /// `None` keeps standalone in-memory catalogs independent of a node persona registry.
    pub async fn create(
        &self,
        draft: InstanceDraft,
        runtime: Option<(&PersonaRegistry, &SessionManager)>,
    ) -> Result<BotInstance, InstanceError> {
        let mut instances = self.instances.write().await;

        let name = normalize_name(&draft.name)?;
        let id = unique_id(&instances, &name);
        let candidate = build_instance(id, name, draft)?;

        Self::validate_persona(&candidate, runtime.map(|(personas, _)| personas))?;
        Self::validate_claims(&instances, &candidate)?;
        let mut next = instances.clone();
        next.insert(candidate.id.clone(), candidate.clone());
        self.commit_instance_change(&mut instances, next, &candidate.id, runtime)?;

        Ok(candidate)
    }

    /// Replaces the editable fields of an instance, keeping its identity and session history.
    pub async fn update(
        &self,
        id: &str,
        draft: InstanceDraft,
        runtime: Option<(&PersonaRegistry, &SessionManager)>,
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

        Self::validate_persona(&candidate, runtime.map(|(personas, _)| personas))?;
        Self::validate_claims(&instances, &candidate)?;
        let mut next = instances.clone();
        next.insert(candidate.id.clone(), candidate.clone());
        self.commit_instance_change(&mut instances, next, &candidate.id, runtime)?;

        Ok(candidate)
    }

    /// Deletes an instance.
    pub async fn delete(
        &self,
        id: &str,
        runtime: Option<(&PersonaRegistry, &SessionManager)>,
    ) -> Result<(), InstanceError> {
        let mut instances = self.instances.write().await;
        let mut next = instances.clone();
        if next.remove(id).is_none() {
            return Err(InstanceError::NotFound(id.to_string()));
        }
        self.commit_instance_change(&mut instances, next, id, runtime)
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

    /// Identifiers of the instances that select `persona_id` from the catalog.
    ///
    /// A persona in use cannot be deleted: those instances would silently start answering with
    /// different instructions.
    pub async fn instances_using_persona(&self, persona_id: &str) -> Vec<String> {
        self.with_persona_users(persona_id, |users| users).await
    }

    /// Checks references and runs a synchronous operation while instance writes remain excluded.
    ///
    /// Persona deletion uses this boundary so a create/update cannot validate a persona before
    /// deletion and publish its reference afterwards. The callback must not re-enter this catalog.
    pub async fn with_persona_users<T>(
        &self,
        persona_id: &str,
        operation: impl FnOnce(Vec<String>) -> T,
    ) -> T {
        let instances = self.instances.read().await;
        let mut users: Vec<String> = instances
            .values()
            .filter(|instance| instance.persona_id.as_deref() == Some(persona_id))
            .map(|instance| instance.id.clone())
            .collect();
        users.sort();
        let result = operation(users);
        drop(instances);
        result
    }

    /// Checks the live persona catalog only after taking the instance write lock.
    fn validate_persona(
        candidate: &BotInstance,
        personas: Option<&PersonaRegistry>,
    ) -> Result<(), InstanceError> {
        if let Some(personas) = personas
            && let Some(id) = candidate.persona_id.as_deref()
            && personas.get(id).is_none()
        {
            return Err(InstanceError::Invalid(format!(
                "persona '{id}' does not exist on this node"
            )));
        }
        Ok(())
    }

    /// Commits one instance mutation together with its generated persona's lifecycle.
    ///
    /// Lock order is instance write lock -> generated persona shard -> session writers (try only)
    /// -> session storage -> instance document. No callback re-enters the persona registry. A later
    /// failure retains the old instance/persona, but earlier durable unbindings remain committed.
    fn commit_instance_change(
        &self,
        live: &mut HashMap<String, BotInstance>,
        next: HashMap<String, BotInstance>,
        id: &str,
        runtime: Option<(&PersonaRegistry, &SessionManager)>,
    ) -> Result<(), InstanceError> {
        let Some((personas, sessions)) = runtime else {
            return self.commit(live, next);
        };
        let previous = live.get(id).and_then(generated_persona);
        let generated = next.get(id).and_then(generated_persona);
        if let Some(persona) = &generated {
            persona.validate()?;
        }
        if let Some(previous) = previous.filter(|_| generated.is_none()) {
            let mut users: Vec<String> = next
                .values()
                .filter(|instance| instance.persona_id.as_deref() == Some(previous.id.as_str()))
                .map(|instance| instance.id.clone())
                .collect();
            users.sort();
            if !users.is_empty() {
                return Err(InstanceError::PersonaInUse {
                    persona: previous.id,
                    instances: users,
                });
            }
            personas.remove_after(&previous.id, |_| {
                sessions.unbind_persona(&previous.id)?;
                self.commit(live, next)
            })?;
        } else {
            self.commit(live, next)?;
            if let Some(persona) = generated {
                // Validation finished before committing. The generated `instance:` namespace can
                // never replace the built-in `assistant`, the only other register rejection.
                personas
                    .register(persona)
                    .expect("validated generated persona cannot replace the base assistant");
            }
        }
        Ok(())
    }

    /// Makes `generation` the current session of one conversation and returns its identifier.
    ///
    /// Used by `/new` (a generation no session has used), `/switch` (an existing one) and `/del`.
    /// Other sessions of the conversation are left untouched: their history stays where it is.
    pub async fn select_session(
        &self,
        id: &str,
        conversation: &str,
        generation: u64,
    ) -> Result<String, InstanceError> {
        let mut instances = self.instances.write().await;
        let mut next = instances.clone();
        let instance = next
            .get_mut(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;

        instance
            .session_generations
            .insert(conversation.to_string(), generation);
        let session_id = instance.conversation_session_id(conversation);
        self.commit(&mut instances, next)?;

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
        let mut next = instances.clone();
        let instance = next
            .get_mut(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;

        instance.model = model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        let updated = instance.clone();
        self.commit(&mut instances, next)?;

        Ok(updated)
    }

    /// Writes `next` to disk and only then makes it the live catalog.
    ///
    /// Every mutation stages its change on a copy: if the write fails, the running node keeps
    /// serving exactly what the file says, instead of an unsaved change that a restart would
    /// silently revert.
    fn commit(
        &self,
        live: &mut HashMap<String, BotInstance>,
        next: HashMap<String, BotInstance>,
    ) -> Result<(), InstanceError> {
        Self::persist(self.path.as_deref(), &next)?;
        *live = next;
        Ok(())
    }

    /// Validates the stored document: every agent override names a selectable agent and no two
    /// enabled instances claim the same platform.
    async fn validate_all(&self) -> Result<(), InstanceError> {
        let instances = self.instances.read().await;
        for instance in instances.values() {
            if let Some(agent) = instance.agent.as_deref() {
                kanon_llm::check_agent_id(agent).map_err(|err| {
                    InstanceError::Invalid(format!("instance '{}': {err}", instance.id))
                })?;
            }
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

    let agent = draft
        .agent
        .map(|agent| agent.trim().to_string())
        .filter(|agent| !agent.is_empty());
    if let Some(agent) = agent.as_deref() {
        kanon_llm::check_agent_id(agent).map_err(InstanceError::Invalid)?;
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
    // Normalized like the node-wide policy, so a stored override matches exactly what is enforced.
    let command_policy = draft
        .command_policy
        .map(CommandPolicy::prepare)
        .transpose()
        .map_err(InstanceError::Invalid)?;

    Ok(BotInstance {
        id,
        name,
        enabled: draft.enabled,
        adapters,
        persona_id,
        system_prompt,
        agent,
        model,
        reply_policy: draft.reply_policy,
        context_policy: draft.context_policy,
        session_scope: draft.session_scope,
        observe_group: draft.observe_group,
        command_policy,
        bash: draft.bash,
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
