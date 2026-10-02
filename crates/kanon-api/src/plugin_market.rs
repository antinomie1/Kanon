//! Plugin market: JSON indexes listing installable plugins.
//!
//! A market is nothing more than a JSON document at an `https` URL; the operator lists the
//! indexes to read under `plugin_market.indexes` in `data/system.json`. A node ships with none
//! configured, so it never contacts a third party unless the operator chose one.
//!
//! # Index format
//! ```json
//! {
//!   "name": "Example market",
//!   "plugins": [
//!     {
//!       "id": "org.example.weather",
//!       "name": "Weather",
//!       "description": "Weather commands and a weather tool",
//!       "author": "Example",
//!       "version": "1.2.0",
//!       "download_url": "https://example.org/weather-1.2.0.kpk",
//!       "repository": "https://github.com/example/kanon-weather.git",
//!       "kanon_version": ">=0.1, <0.3",
//!       "platforms": ["qq"],
//!       "homepage": "https://example.org/weather"
//!     }
//!   ]
//! }
//! ```
//! `id`, `name` and `version` are required, plus at least one of `download_url` (a `.kpk` /
//! `.zip` package, preferred when both exist) and `repository` (a Git URL). Unknown fields are
//! ignored so an index can grow without breaking older nodes.
//!
//! Entries are parsed one by one: a malformed entry is reported as a warning of its source and
//! the rest of the index stays usable, while a document that is not an index at all fails the
//! whole source. Nothing is dropped without a message.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugin_sources::{check_git_url, check_remote_url};

/// One plugin offered by a market index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketEntry {
    /// Plugin identifier; must equal the `[plugin] id` of the package it points to.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Author attribution.
    #[serde(default)]
    pub author: String,
    /// Version the download or repository provides.
    pub version: String,
    /// Git repository the plugin can be cloned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Direct link to a `.kpk` / `.zip` package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    /// Node versions the plugin supports, as a semver requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kanon_version: Option<String>,
    /// Platforms the plugin was written for; empty means every platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Project homepage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// Root of a market index document.
#[derive(Debug, Deserialize)]
struct IndexDocument {
    /// Optional display name of the market.
    #[serde(default)]
    name: Option<String>,
    /// Raw entries, parsed individually so one bad entry cannot hide the others.
    plugins: Vec<Value>,
}

/// A parsed index: its usable entries plus the problems found in the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedIndex {
    /// Display name declared by the index, if any.
    pub name: Option<String>,
    /// Valid entries in document order, first occurrence of each id only.
    pub entries: Vec<MarketEntry>,
    /// One sentence per skipped entry.
    pub warnings: Vec<String>,
}

/// Parses and validates a market index document.
///
/// Fails only when the document as a whole is not an index (not JSON, or no `plugins` array).
pub fn parse_index(bytes: &[u8]) -> Result<ParsedIndex, String> {
    let document: IndexDocument =
        serde_json::from_slice(bytes).map_err(|err| format!("not a plugin market index: {err}"))?;

    let mut parsed = ParsedIndex {
        name: document
            .name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        ..ParsedIndex::default()
    };

    for (position, raw) in document.plugins.into_iter().enumerate() {
        let label = raw
            .get("id")
            .and_then(Value::as_str)
            .map(|id| format!("entry {} ('{id}')", position + 1))
            .unwrap_or_else(|| format!("entry {}", position + 1));

        let entry = match serde_json::from_value::<MarketEntry>(raw) {
            Ok(entry) => entry,
            Err(err) => {
                parsed.warnings.push(format!("{label} skipped: {err}"));
                continue;
            }
        };
        if let Err(reason) = validate_entry(&entry) {
            parsed.warnings.push(format!("{label} skipped: {reason}"));
            continue;
        }
        if parsed.entries.iter().any(|known| known.id == entry.id) {
            parsed
                .warnings
                .push(format!("{label} skipped: the id is listed more than once"));
            continue;
        }
        parsed.entries.push(entry);
    }

    Ok(parsed)
}

/// Checks the fields an installer relies on, so the console never offers a button that is
/// certain to fail.
fn validate_entry(entry: &MarketEntry) -> Result<(), String> {
    kanon_storage::PluginId::validate(&entry.id)
        .map_err(|err| format!("invalid plugin id: {err}"))?;
    if entry.name.trim().is_empty() {
        return Err("'name' is empty".to_string());
    }
    if entry.version.trim().is_empty() {
        return Err("'version' is empty".to_string());
    }
    match (&entry.download_url, &entry.repository) {
        (None, None) => {
            return Err("it has neither 'download_url' nor 'repository'".to_string());
        }
        (download_url, repository) => {
            if let Some(url) = download_url {
                check_remote_url(url).map_err(|reason| format!("'download_url': {reason}"))?;
            }
            if let Some(url) = repository {
                check_git_url(url).map_err(|reason| format!("'repository': {reason}"))?;
            }
        }
    }
    if let Some(requirement) = &entry.kanon_version {
        semver::VersionReq::parse(requirement)
            .map_err(|err| format!("'kanon_version' '{requirement}' is invalid: {err}"))?;
    }
    Ok(())
}

/// Result of reading one configured index, as reported to the console.
#[derive(Debug, Clone, Serialize)]
pub struct MarketSource {
    /// Index URL as configured.
    pub url: String,
    /// Display name the index declares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Number of plugins this source contributed.
    pub plugins: usize,
    /// Why the source could not be read at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Entries of this source that were skipped, one sentence each.
    pub warnings: Vec<String>,
}

/// A market entry annotated for this node.
#[derive(Debug, Clone, Serialize)]
pub struct MarketPlugin {
    /// The entry as the index lists it.
    #[serde(flatten)]
    pub entry: MarketEntry,
    /// URL of the index that listed it.
    pub source: String,
    /// Version installed on this node, when the plugin is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Whether the entry's `kanon_version` admits this node.
    pub compatible: bool,
    /// Why it does not, when `compatible` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incompatible_reason: Option<String>,
}

/// Response of `GET /api/v1/plugins/market`.
#[derive(Debug, Clone, Serialize)]
pub struct MarketResponse {
    /// Whether any index is configured.
    pub configured: bool,
    /// Setup guidance when nothing is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// One entry per configured index, in configuration order.
    pub sources: Vec<MarketSource>,
    /// Plugins from every readable source; an id listed by several sources is taken from the
    /// first one.
    pub plugins: Vec<MarketPlugin>,
}

/// Guidance returned when no index is configured.
pub const UNCONFIGURED_HINT: &str = "No plugin market is configured. List index URLs under \
     \"plugin_market\": { \"indexes\": [\"https://…/index.json\"] } in data/system.json; \
     the market is read on the next visit, no restart needed.";
