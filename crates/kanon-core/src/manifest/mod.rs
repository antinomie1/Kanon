//! Plugin static manifest (`plugin.toml`) schema and parser.
//!
//! Parses static metadata, runtime target requirements, command definitions,
//! and tool schemas from plugin configuration files.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Error types occurring during plugin manifest parsing.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// File I/O failure while accessing the manifest file.
    #[error("Failed to read manifest file: {0}")]
    Io(#[from] std::io::Error),
    /// Deserialization failure due to invalid TOML syntax or schema violation.
    #[error("Failed to parse TOML manifest: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Metadata section `[plugin]` in `plugin.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSection {
    /// Unique identifier for the plugin (e.g., `org.kanon.plugin.weather`).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semantic version string.
    pub version: String,
    /// Author or organization attribution.
    pub author: Option<String>,
    /// Brief explanation of the plugin functionality.
    pub description: Option<String>,
    /// Runtime platform ("rust", "python", "typescript").
    pub runtime: String,
    /// Entrypoint executable path or script relative to plugin root.
    pub entrypoint: String,
    /// Whether this plugin must run in a dedicated sub-process.
    pub isolated: Option<bool>,
    /// Execution priority for pipeline scheduling (1..=1000, lower executes first, default 500).
    pub priority: Option<i32>,
}

/// Command metadata declared under `[[commands]]` in `plugin.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDefinition {
    /// Command trigger word (e.g. `rustcalc`).
    pub name: String,
    /// Short help text describing the command purpose.
    pub description: Option<String>,
    /// Usage example syntax.
    pub usage: Option<String>,
    /// Dispatch priority (lower numbers execute first, default 500).
    pub priority: Option<i32>,
}

/// Static tool declaration under `[[tools]]` in `plugin.toml`.
///
/// Mirrors the runtime `ToolMeta` reported over gRPC so that the management console
/// can render the tool catalog without spawning a plugin host sub-process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinitionEntry {
    /// Tool name exposed to the model for function calling.
    pub name: String,
    /// Natural language explanation of the tool purpose.
    pub description: Option<String>,
    /// JSON Schema object describing accepted parameters.
    pub parameters: Option<serde_json::Value>,
}

/// Platform adapter capability declared under `[adapter]` in `plugin.toml`.
///
/// Declaring this section is what turns a plugin into a platform adapter: the core routes every
/// outbound message whose `platform` matches to this plugin's host via `OnDeliverMessage`, and the
/// plugin pushes inbound messages back through `BotApiService.IngestEvent`.
///
/// The declaration lives in the manifest rather than in `PluginMeta` on purpose: the core must
/// know which host owns a platform *before* receiving traffic, and a static file cannot disagree
/// with itself after a hot reload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSection {
    /// Platform identifier served by this plugin (matches `PipelineEventRequest.platform`).
    pub platform: String,
    /// Human-readable name shown in the management console.
    pub display_name: Option<String>,
}

/// Complete representation of a parsed `plugin.toml` manifest.
///
/// Unknown sections are rejected rather than ignored. In particular, a Python or TypeScript
/// plugin declares its packages in its own `pyproject.toml` / `package.json`; a stale
/// `[dependencies]` section here must fail loudly instead of looking like it still does something.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Core plugin metadata.
    pub plugin: PluginSection,
    /// JSON Schema of the user-facing configuration object (`[config_schema]`).
    ///
    /// Retained verbatim so the headless core can hand the schema to the WebUI
    /// for form rendering without launching the plugin sub-process.
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// Optional platform adapter capability declaration.
    #[serde(default)]
    pub adapter: Option<AdapterSection>,
    /// List of statically declared commands.
    #[serde(default)]
    pub commands: Vec<CommandDefinition>,
    /// List of statically declared tools.
    #[serde(default)]
    pub tools: Vec<ToolDefinitionEntry>,
}

impl PluginManifest {
    /// Loads and parses a `plugin.toml` manifest from a given file path.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let manifest: Self = toml::from_str(&content)?;
        Ok(manifest)
    }
}

pub mod scanner;
pub use scanner::{DiscoveredPlugin, PluginScanner};
