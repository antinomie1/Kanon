//! Personas: the static top layer of every prompt.
//!
//! # Why a persona is plain text
//! Providers cache a prompt by its *prefix*, so whatever sits at the top of the request must be
//! byte-identical from one turn to the next. A persona is therefore a fixed piece of text — no
//! template slots, no per-turn variables. Anything that changes while the bot runs (the time, who
//! is speaking, the user's message) belongs at the *end* of the request, where a change only costs
//! the tokens after it.
//!
//! # What ships, what the operator owns
//! The node ships exactly one persona, the base assistant, with a deliberately minimal prompt. It
//! is what a conversation uses when nothing else was chosen, so it is read-only and cannot be
//! removed: without it a conversation could end up with no instructions at all. Every other
//! persona is created by the operator ([`PersonaKind::Custom`]) or derived from a bot instance's
//! own prompt ([`PersonaKind::Instance`]).

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;

use crate::agent::AgentHook;
use crate::error::AgentError;
use crate::gateway::types::{ChatMessage, ChatRequest};
use crate::session::SessionManager;

/// Identifier of the base assistant persona.
pub const BASE_PERSONA_ID: &str = "assistant";

/// Prompt of the base assistant persona: intentionally minimal, so it adds no opinions of its own.
pub const BASE_PERSONA_PROMPT: &str = "You are a helpful assistant.";

/// Longest persona identifier accepted.
const MAX_PERSONA_ID_LEN: usize = 64;

/// Where a persona comes from, which decides who may change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersonaKind {
    /// Shipped with the node; read-only.
    Builtin,
    /// Created by the operator through the console; persisted by the management layer.
    Custom,
    /// Generated from a bot instance's own prompt; owned by that instance.
    Instance,
}

/// Failures raised while building or registering a persona.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PersonaError {
    /// The identifier is not a usable slug.
    #[error(
        "persona id '{0}' is invalid: use 1-64 lowercase letters, digits, '-' or '_', starting with a letter or digit"
    )]
    InvalidId(String),
    /// The display name is blank.
    #[error("persona name must not be empty")]
    EmptyName,
    /// The prompt is blank.
    #[error("persona prompt must not be empty")]
    EmptyPrompt,
    /// A built-in persona cannot be replaced or removed.
    #[error("persona '{0}' is built in and cannot be changed or removed")]
    ReadOnly(String),
    /// No persona has this identifier.
    #[error("persona '{0}' does not exist")]
    NotFound(String),
}

/// Normalizes prompt text so equal text always renders to equal bytes.
///
/// Windows line endings and leading/trailing whitespace would otherwise make two saves of the same
/// prompt differ by a character — and a single changed character at the top of the request
/// invalidates the provider's cache for everything after it.
fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_string()
}

/// One persona: a named, static system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    /// Unique identifier slug (e.g. `"translator"`).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Optional one-line description shown next to the name.
    pub description: String,
    /// The system prompt, exactly as it is sent (already normalized).
    pub prompt: String,
    /// Where the persona comes from.
    pub kind: PersonaKind,
}

impl Persona {
    /// The base assistant.
    pub fn base() -> Self {
        Self {
            id: BASE_PERSONA_ID.to_string(),
            name: "Assistant".to_string(),
            description: "Minimal general-purpose assistant".to_string(),
            prompt: BASE_PERSONA_PROMPT.to_string(),
            kind: PersonaKind::Builtin,
        }
    }

    /// Builds an operator-defined persona, validating and normalizing every field.
    pub fn custom(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<Self, PersonaError> {
        let persona = Self {
            id: id.into().trim().to_string(),
            name: normalize_text(&name.into()),
            description: normalize_text(&description.into()),
            prompt: normalize_text(&prompt.into()),
            kind: PersonaKind::Custom,
        };
        persona.validate()?;
        Ok(persona)
    }

    /// Builds the persona backing a bot instance's own prompt.
    ///
    /// The identifier is chosen by the instance catalog (it carries a prefix no custom persona may
    /// use), so only the text is normalized here.
    pub fn instance(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: normalize_text(&name.into()),
            description: normalize_text(&description.into()),
            prompt: normalize_text(&prompt.into()),
            kind: PersonaKind::Instance,
        }
    }

    /// Checks the fields an operator can get wrong.
    pub fn validate(&self) -> Result<(), PersonaError> {
        if self.kind == PersonaKind::Custom && !is_valid_slug(&self.id) {
            return Err(PersonaError::InvalidId(self.id.clone()));
        }
        if self.name.is_empty() {
            return Err(PersonaError::EmptyName);
        }
        if self.prompt.is_empty() {
            return Err(PersonaError::EmptyPrompt);
        }
        Ok(())
    }
}

/// Whether `id` is a lowercase slug: `[a-z0-9][a-z0-9_-]{0,63}`.
///
/// No colon is allowed, which keeps every operator-chosen id clear of the `instance:` prefix that
/// generated personas use.
pub fn is_valid_slug(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= MAX_PERSONA_ID_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Thread-safe catalog of personas.
///
/// Always contains the base assistant. Everything else is added and removed at runtime by the
/// layer that owns it (the console for custom personas, the instance catalog for instance ones).
pub struct PersonaRegistry {
    personas: DashMap<String, Persona>,
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PersonaRegistry {
    /// Creates a registry holding only the base assistant.
    pub fn new() -> Self {
        let personas = DashMap::new();
        personas.insert(BASE_PERSONA_ID.to_string(), Persona::base());
        Self { personas }
    }

    /// Registers (or replaces) a persona.
    ///
    /// Validation happens here as well as in the constructors because [`Persona`]'s fields are
    /// public; the built-in persona can never be replaced.
    pub fn register(&self, persona: Persona) -> Result<(), PersonaError> {
        if persona.kind == PersonaKind::Builtin
            || self
                .personas
                .get(&persona.id)
                .is_some_and(|existing| existing.kind == PersonaKind::Builtin)
        {
            return Err(PersonaError::ReadOnly(persona.id));
        }
        persona.validate()?;
        self.personas.insert(persona.id.clone(), persona);
        Ok(())
    }

    /// Retrieves a persona by its identifier slug.
    pub fn get(&self, id: &str) -> Option<Persona> {
        self.personas.get(id).map(|p| p.clone())
    }

    /// The base assistant, which is always present.
    pub fn base(&self) -> Persona {
        self.get(BASE_PERSONA_ID).unwrap_or_else(Persona::base)
    }

    /// Removes a persona; the built-in one cannot be removed.
    pub fn remove(&self, id: &str) -> Result<Persona, PersonaError> {
        match self.personas.get(id).map(|p| p.kind) {
            None => Err(PersonaError::NotFound(id.to_string())),
            Some(PersonaKind::Builtin) => Err(PersonaError::ReadOnly(id.to_string())),
            Some(_) => self
                .personas
                .remove(id)
                .map(|(_, persona)| persona)
                .ok_or_else(|| PersonaError::NotFound(id.to_string())),
        }
    }

    /// Returns every persona, ordered by identifier so listings are stable.
    pub fn list(&self) -> Vec<Persona> {
        let mut personas: Vec<Persona> = self.personas.iter().map(|p| p.clone()).collect();
        personas.sort_by(|a, b| a.id.cmp(&b.id));
        personas
    }

    /// Returns the number of registered personas (the base assistant included).
    pub fn len(&self) -> usize {
        self.personas.len()
    }

    /// Always `false`: the base assistant is always registered.
    pub fn is_empty(&self) -> bool {
        self.personas.is_empty()
    }
}

/// Lifecycle hook that puts the session's persona at the top of every request.
///
/// The persona is the most stable part of a prompt, so it goes first. The session's persona is
/// resolved on every call, which is what makes a console switch effective on the very next turn.
pub struct PersonaHook {
    session_manager: Arc<SessionManager>,
    persona_registry: Arc<PersonaRegistry>,
}

impl PersonaHook {
    /// Constructs the hook over the node's sessions and personas.
    pub fn new(
        session_manager: Arc<SessionManager>,
        persona_registry: Arc<PersonaRegistry>,
    ) -> Self {
        Self {
            session_manager,
            persona_registry,
        }
    }

    /// Persona that governs a session: the one bound to it, otherwise the base assistant.
    fn persona_for(&self, session_id: &str) -> Persona {
        let Some(bound) = self.session_manager.get_persona(session_id) else {
            return self.persona_registry.base();
        };
        match self.persona_registry.get(&bound) {
            Some(persona) => persona,
            None => {
                // Deleting a persona unbinds the sessions using it, so reaching this means a stale
                // binding slipped through. Say so instead of silently changing behaviour.
                tracing::warn!(
                    session_id = %session_id,
                    persona_id = %bound,
                    "Session is bound to an unknown persona; using the base assistant"
                );
                self.persona_registry.base()
            }
        }
    }
}

#[async_trait]
impl AgentHook for PersonaHook {
    async fn on_llm_request(
        &self,
        session_id: &str,
        request: &mut ChatRequest,
    ) -> Result<(), AgentError> {
        // The persona is the first part of the static system block, ahead of anything else placed
        // there, because it is the part that changes least.
        let prompt = self.persona_for(session_id).prompt;
        request.messages.insert(0, ChatMessage::system(prompt));
        Ok(())
    }
}
