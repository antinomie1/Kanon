//! Persisted global enable/disable state for plugins, skills and MCP servers.
//!
//! # Why one store with sections
//! All three item kinds share the same operator intent ("run this on my node or not") and the same
//! persistence concerns: the state must survive restarts, must not rewrite files inside the user's
//! plugin/skill directories, and an identifier absent from the store means *enabled*, so a freshly
//! added item runs without an explicit opt-in.
//!
//! Keeping one document with named sections also keeps the read-modify-write path in a single
//! place, so toggling a skill can never discard a plugin's state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Default location of the toggle document, relative to the node working directory.
pub const DEFAULT_TOGGLE_STATE: &str = "./data/toggles.json";

/// Section holding plugin identifiers.
pub const PLUGIN_SECTION: &str = "plugins";
/// Section holding skill identifiers.
pub const SKILL_SECTION: &str = "skills";
/// Section holding MCP server identifiers.
pub const MCP_SECTION: &str = "mcp";

/// Document persisted at the toggle path.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToggleDocument {
    /// Schema version, so a future format change can be migrated explicitly.
    #[serde(default = "default_version")]
    version: u32,
    /// Plugin identifier -> enabled flag.
    #[serde(default)]
    plugins: HashMap<String, bool>,
    /// Skill identifier -> enabled flag.
    #[serde(default)]
    skills: HashMap<String, bool>,
    /// MCP server identifier -> enabled flag.
    #[serde(default)]
    mcp: HashMap<String, bool>,
    /// Unrecognized keys are carried through verbatim, so writing this document never discards
    /// settings another (possibly newer) component stored beside it.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl Default for ToggleDocument {
    fn default() -> Self {
        Self {
            version: default_version(),
            plugins: HashMap::new(),
            skills: HashMap::new(),
            mcp: HashMap::new(),
            other: serde_json::Map::new(),
        }
    }
}

/// Current schema version of the toggle document.
fn default_version() -> u32 {
    1
}

/// Thread-safe toggle state for every toggleable item kind, optionally persisted.
///
/// A store without a path is *in-memory*: it behaves identically within the process but writes
/// nothing, which is what tests and embedded cores use.
#[derive(Debug, Default)]
pub struct ToggleStore {
    /// Path of the persisted document; `None` for an in-memory store.
    path: Option<PathBuf>,
    /// Section -> identifier -> enabled flag, for everything the operator has toggled.
    sections: RwLock<HashMap<String, HashMap<String, bool>>>,
}

impl ToggleStore {
    /// Creates a store that lives only for this process.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            sections: RwLock::new(HashMap::new()),
        }
    }

    /// Opens (or creates) the store at `path`.
    ///
    /// A missing file means "nothing has been toggled yet" (everything enabled); a malformed file
    /// is an error, because silently enabling an item the operator disabled is exactly the
    /// surprise this store exists to prevent.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let document = read_document(&path)?.unwrap_or_default();

        Ok(Self {
            path: Some(path),
            sections: RwLock::new(HashMap::from([
                (PLUGIN_SECTION.to_string(), document.plugins),
                (SKILL_SECTION.to_string(), document.skills),
                (MCP_SECTION.to_string(), document.mcp),
            ])),
        })
    }

    /// Path of the persisted document, or `None` for an in-memory store.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Returns whether `id` is enabled in `section` (absent identifiers are enabled).
    pub async fn is_enabled(&self, section: &str, id: &str) -> bool {
        self.sections
            .read()
            .await
            .get(section)
            .and_then(|entries| entries.get(id))
            .copied()
            .unwrap_or(true)
    }

    /// Returns every identifier the operator explicitly disabled in `section`.
    pub async fn disabled_ids(&self, section: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .sections
            .read()
            .await
            .get(section)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, enabled)| !**enabled)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// Sets the state of one item and persists it.
    ///
    /// Returns `true` when the state actually changed, so callers can skip side effects (stopping
    /// a host, reconnecting a client) for an idempotent request.
    pub async fn set_enabled(
        &self,
        section: &str,
        id: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let mut sections = self.sections.write().await;
        // Staged on a copy and swapped in only after the write succeeds, so a failed write never
        // leaves the node honouring a toggle the file does not record.
        let mut next = sections.clone();
        let entries = next.entry(section.to_string()).or_default();
        // Absence means enabled, so an enabled item is represented by *no* entry. Persisting an
        // explicit `true` would accumulate dead keys for every item ever toggled and make the file
        // claim state the operator never expressed.
        if entries.get(id).copied().unwrap_or(true) == enabled {
            return Ok(false);
        }

        if enabled {
            entries.remove(id);
        } else {
            entries.insert(id.to_string(), enabled);
        }
        self.persist(&next)?;
        *sections = next;
        Ok(true)
    }

    /// Atomically writes the document.
    fn persist(&self, sections: &HashMap<String, HashMap<String, bool>>) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }

        // Read-modify-write so unrelated keys survive a toggle.
        let mut document = read_document(path)?.unwrap_or_default();
        document.version = default_version();
        document.plugins = section_of(sections, PLUGIN_SECTION);
        document.skills = section_of(sections, SKILL_SECTION);
        document.mcp = section_of(sections, MCP_SECTION);

        let payload = serde_json::to_string_pretty(&document)
            .map_err(|err| format!("failed to serialize toggle state: {err}"))?;

        let temp_path = path.with_extension("json.tmp");
        std::fs::write(&temp_path, payload)
            .map_err(|err| format!("failed to write {}: {err}", temp_path.display()))?;
        std::fs::rename(&temp_path, path).map_err(|err| {
            format!(
                "failed to move {} into place at {}: {err}",
                temp_path.display(),
                path.display()
            )
        })
    }
}

/// Copies one section out of the in-memory map for serialization.
fn section_of(
    sections: &HashMap<String, HashMap<String, bool>>,
    name: &str,
) -> HashMap<String, bool> {
    sections.get(name).cloned().unwrap_or_default()
}

/// Reads the toggle document, returning `None` when the file does not exist.
fn read_document(path: &Path) -> Result<Option<ToggleDocument>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    let document: ToggleDocument = serde_json::from_str(&raw)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;

    Ok(Some(document))
}
