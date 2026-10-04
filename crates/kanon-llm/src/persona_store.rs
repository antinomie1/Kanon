//! Persistence of operator-defined personas (`data/personas.json`).
//!
//! Only [`PersonaKind::Custom`] personas live here. The base assistant ships with the node and
//! instance personas are derived from the instance catalog, so persisting either would create a
//! second owner for them.
//!
//! The document is small, human-readable JSON written atomically (temporary file plus rename): a
//! crash mid-write can never leave a truncated file that would block the next startup.
//!
//! The console and plugins (through the core's persona RPCs) both change personas. Every change
//! goes through [`PersonaStore::create`], [`PersonaStore::upsert`] or [`PersonaStore::remove`].
//! They follow *validate → persist → apply* under one lock: two writers can never interleave their
//! read-modify-write of the document, and the running registry never holds a persona the file
//! does not.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::MemoryError;
use crate::prompt::{Persona, PersonaError, PersonaKind, PersonaRegistry};
use crate::session::SessionManager;

/// Default location of the persona document, relative to the node working directory.
pub const DEFAULT_PERSONA_FILE: &str = "./data/personas.json";

/// Current schema version of the document.
const DOCUMENT_VERSION: u32 = 1;

/// One persisted persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersonaRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    prompt: String,
}

/// Root document of `data/personas.json`.
#[derive(Debug, Serialize, Deserialize)]
struct PersonaDocument {
    /// Schema version, so a future format change can be migrated explicitly.
    version: u32,
    personas: Vec<PersonaRecord>,
}

/// Why a persona change was refused.
#[derive(Debug, thiserror::Error)]
pub enum PersonaChangeError {
    /// The persona is invalid, built in, or does not exist.
    #[error(transparent)]
    Persona(#[from] PersonaError),
    /// The persona is generated from a bot instance's own prompt and changes with that instance.
    #[error(
        "persona '{0}' is generated from a bot instance's own prompt; edit it on that instance"
    )]
    InstanceOwned(String),
    /// The catalog is unchanged after a JSON write failure; session unbindings may already persist.
    #[error("{0}")]
    Storage(String),
    /// A bound session is busy or could not be durably unbound; the persona remains registered.
    #[error(transparent)]
    Session(#[from] MemoryError),
}

/// File-backed store of the operator's personas.
#[derive(Debug)]
pub struct PersonaStore {
    path: PathBuf,
    /// Serializes changes: each one reads the registry, writes the file and applies the result.
    changes: Mutex<()>,
}

impl Default for PersonaStore {
    fn default() -> Self {
        Self::new(DEFAULT_PERSONA_FILE)
    }
}

impl PersonaStore {
    /// Creates a store bound to an explicit path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            changes: Mutex::new(()),
        }
    }

    /// Path of the persisted document.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads every persisted persona, validating each one.
    ///
    /// A missing file is "no custom personas yet". A malformed document, or a persona that fails
    /// validation, is an error: silently dropping the operator's personas would change how the bot
    /// behaves without anyone noticing.
    pub fn load(&self) -> Result<Vec<Persona>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)
            .map_err(|err| format!("Failed to read {}: {err}", self.path.display()))?;
        let document: PersonaDocument = serde_json::from_str(&raw)
            .map_err(|err| format!("Failed to parse {}: {err}", self.path.display()))?;
        if document.version != DOCUMENT_VERSION {
            return Err(format!(
                "{} has schema version {}, expected {DOCUMENT_VERSION}",
                self.path.display(),
                document.version
            ));
        }

        document
            .personas
            .into_iter()
            .map(|record| {
                Persona::custom(
                    record.id.clone(),
                    record.name,
                    record.description,
                    record.prompt,
                )
                .map_err(|err| {
                    format!(
                        "Invalid persona '{}' in {}: {err}",
                        record.id,
                        self.path.display()
                    )
                })
            })
            .collect()
    }

    /// Builds a registry holding the base assistant plus every persisted persona.
    ///
    /// This is how the composition root restores the operator's personas at startup.
    pub fn load_registry(&self) -> Result<PersonaRegistry, String> {
        let registry = PersonaRegistry::new();
        for persona in self.load()? {
            let id = persona.id.clone();
            registry.register(persona).map_err(|err| {
                format!(
                    "Persona '{id}' in {} is unusable: {err}",
                    self.path.display()
                )
            })?;
        }
        Ok(registry)
    }

    /// Creates an operator-defined persona only if its identifier is unused.
    ///
    /// Returns `false` on a collision without changing the registry or file. The existence check
    /// shares the write lock with updates and removals, so concurrent creates cannot overwrite.
    pub fn create(
        &self,
        registry: &PersonaRegistry,
        persona: Persona,
    ) -> Result<bool, PersonaChangeError> {
        let _change = self.lock();
        if registry.get(&persona.id).is_some() {
            return Ok(false);
        }
        self.upsert_locked(registry, persona)?;
        Ok(true)
    }

    /// Creates or replaces an operator-defined persona and returns whether one was replaced.
    ///
    /// The persona must be [`PersonaKind::Custom`] (build it with [`Persona::custom`]); the
    /// built-in persona and instance personas are refused. The file is written before the
    /// registry changes, so a failed write leaves both as they were.
    pub fn upsert(
        &self,
        registry: &PersonaRegistry,
        persona: Persona,
    ) -> Result<bool, PersonaChangeError> {
        let _change = self.lock();
        self.upsert_locked(registry, persona)
    }

    /// Replaces an existing custom persona without recreating one deleted by another writer.
    pub fn update(
        &self,
        registry: &PersonaRegistry,
        persona: Persona,
    ) -> Result<(), PersonaChangeError> {
        let _change = self.lock();
        if registry.get(&persona.id).is_none() {
            return Err(PersonaError::NotFound(persona.id).into());
        }
        self.upsert_locked(registry, persona)?;
        Ok(())
    }

    /// Writes a persona while the caller holds the change lock.
    fn upsert_locked(
        &self,
        registry: &PersonaRegistry,
        persona: Persona,
    ) -> Result<bool, PersonaChangeError> {
        match persona.kind {
            PersonaKind::Custom => persona.validate()?,
            PersonaKind::Builtin => return Err(PersonaError::ReadOnly(persona.id).into()),
            PersonaKind::Instance => return Err(PersonaChangeError::InstanceOwned(persona.id)),
        }
        let replaced = match registry.get(&persona.id).map(|existing| existing.kind) {
            None => false,
            Some(PersonaKind::Custom) => true,
            Some(PersonaKind::Builtin) => return Err(PersonaError::ReadOnly(persona.id).into()),
            Some(PersonaKind::Instance) => {
                return Err(PersonaChangeError::InstanceOwned(persona.id));
            }
        };
        let mut all: Vec<Persona> = custom_personas(registry)
            .into_iter()
            .filter(|existing| existing.id != persona.id)
            .collect();
        all.push(persona.clone());
        self.save(&all).map_err(PersonaChangeError::Storage)?;
        registry.register(persona)?;
        Ok(replaced)
    }

    /// Removes an operator-defined persona and returns it.
    ///
    /// The caller keeps instance references locked against changes. Bound sessions are unbound
    /// durably before the catalog is removed; `None` is for standalone catalogs without sessions.
    /// If unbinding or the JSON write fails, the persona remains, but any earlier successful
    /// unbindings remain committed. JSON and SQLite do not share a transaction; retry is safe.
    pub fn remove(
        &self,
        registry: &PersonaRegistry,
        id: &str,
        sessions: Option<&SessionManager>,
    ) -> Result<Persona, PersonaChangeError> {
        let _change = self.lock();
        // Snapshot before taking the target's exclusive shard: listing under that guard would
        // re-enter DashMap. The changes mutex keeps other custom catalog writes serialized.
        let remaining: Vec<Persona> = custom_personas(registry)
            .into_iter()
            .filter(|persona| persona.id != id)
            .collect();
        registry.remove_after(id, |persona| {
            if persona.kind == PersonaKind::Instance {
                return Err(PersonaChangeError::InstanceOwned(id.to_string()));
            }
            // Lock order is instance catalog -> store changes -> persona shard -> session
            // writers (try only) -> metadata -> storage. Setters may already own a session
            // writer before looking up the persona, so waiting for a writer here would deadlock.
            if let Some(sessions) = sessions {
                sessions.unbind_persona(id)?;
            }
            self.save(&remaining).map_err(PersonaChangeError::Storage)
        })
    }

    /// Holds the change lock. A poisoned lock only means an earlier change panicked; the file
    /// and registry are still consistent (the file is replaced atomically), so it is reused.
    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Persists the custom personas, replacing the previous document.
    ///
    /// Personas of any other kind are rejected: they are not this store's to keep.
    pub fn save(&self, personas: &[Persona]) -> Result<(), String> {
        if let Some(other) = personas
            .iter()
            .find(|persona| persona.kind != PersonaKind::Custom)
        {
            return Err(format!(
                "persona '{}' is not an operator-defined persona and is not persisted",
                other.id
            ));
        }

        let mut records: Vec<PersonaRecord> = personas
            .iter()
            .map(|persona| PersonaRecord {
                id: persona.id.clone(),
                name: persona.name.clone(),
                description: persona.description.clone(),
                prompt: persona.prompt.clone(),
            })
            .collect();
        // Stable order keeps the file diff-friendly and independent of insertion order.
        records.sort_by(|a, b| a.id.cmp(&b.id));

        let payload = serde_json::to_string_pretty(&PersonaDocument {
            version: DOCUMENT_VERSION,
            personas: records,
        })
        .map_err(|err| format!("Failed to serialize personas: {err}"))?;

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create {}: {err}", parent.display()))?;
        }

        let temp_path = self.path.with_extension("json.tmp");
        std::fs::write(&temp_path, payload)
            .map_err(|err| format!("Failed to write {}: {err}", temp_path.display()))?;
        std::fs::rename(&temp_path, &self.path).map_err(|err| {
            format!(
                "Failed to move {} into place at {}: {err}",
                temp_path.display(),
                self.path.display()
            )
        })
    }
}

/// Every operator-defined persona currently registered.
fn custom_personas(registry: &PersonaRegistry) -> Vec<Persona> {
    registry
        .list()
        .into_iter()
        .filter(|persona| persona.kind == PersonaKind::Custom)
        .collect()
}
