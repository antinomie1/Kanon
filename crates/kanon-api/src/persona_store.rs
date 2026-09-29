//! Persistence of operator-defined personas (`data/personas.json`).
//!
//! Only [`PersonaKind::Custom`] personas live here. The base assistant ships with the node and
//! instance personas are derived from the instance catalog, so persisting either would create a
//! second owner for them.
//!
//! The document is small, human-readable JSON written atomically (temporary file plus rename): a
//! crash mid-write can never leave a truncated file that would block the next startup.

use std::path::{Path, PathBuf};

use kanon_llm::{Persona, PersonaKind, PersonaRegistry};
use serde::{Deserialize, Serialize};

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

/// File-backed store of the operator's personas.
#[derive(Debug, Clone)]
pub struct PersonaStore {
    path: PathBuf,
}

impl Default for PersonaStore {
    fn default() -> Self {
        Self::new(DEFAULT_PERSONA_FILE)
    }
}

impl PersonaStore {
    /// Creates a store bound to an explicit path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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
