//! Static manifest validator and linter for `plugin.toml`.
//!
//! Validates metadata schema, naming conventions, duplicate commands/tools,
//! parameter JSON Schema conformity, priority ranges, and entrypoint existence.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use kanon_core::manifest::PluginManifest;

/// Errors occurring during plugin linting.
#[derive(Debug, Error)]
pub enum LintError {
    /// File I/O failure accessing plugin manifest or files.
    #[error("I/O error reading manifest at '{path}': {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// TOML parsing failure.
    #[error("TOML syntax error in '{path}': {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// Manifest file not found at the specified path.
    #[error(
        "Manifest file not found: '{0}' (expected 'plugin.toml' file or directory containing it)"
    )]
    ManifestNotFound(PathBuf),
}

/// Detailed outcome of a static plugin linting run.
#[derive(Debug, Clone)]
pub struct LintReport {
    /// Path to the analyzed manifest file.
    pub manifest_path: PathBuf,
    /// Root directory of the plugin.
    pub plugin_root: PathBuf,
    /// Declared plugin identifier, if parsed.
    pub plugin_id: Option<String>,
    /// Declared plugin display name, if parsed.
    pub plugin_name: Option<String>,
    /// Declared runtime environment, if parsed.
    pub runtime: Option<String>,
    /// Validation errors that violate specification requirements.
    pub errors: Vec<String>,
    /// Validation warnings that indicate potential issues or non-fatal deviations.
    pub warnings: Vec<String>,
}

impl LintReport {
    /// Returns `true` if no validation errors were found.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Resolves the absolute path to `plugin.toml` from a given file or directory path.
pub fn find_manifest_path(path: &Path) -> Result<(PathBuf, PathBuf), LintError> {
    if path.is_file() {
        let parent = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok((path.to_path_buf(), parent))
    } else if path.is_dir() {
        let candidate = path.join("plugin.toml");
        if candidate.is_file() {
            Ok((candidate, path.to_path_buf()))
        } else {
            Err(LintError::ManifestNotFound(candidate))
        }
    } else {
        Err(LintError::ManifestNotFound(path.to_path_buf()))
    }
}

/// Performs comprehensive static analysis and schema validation on a `plugin.toml` manifest.
pub fn lint_plugin(path: &Path) -> Result<LintReport, LintError> {
    let (manifest_path, plugin_root) = find_manifest_path(path)?;

    let raw_toml = fs::read_to_string(&manifest_path).map_err(|e| LintError::Io {
        path: manifest_path.clone(),
        source: e,
    })?;

    let manifest: PluginManifest = toml::from_str(&raw_toml).map_err(|e| LintError::Toml {
        path: manifest_path.clone(),
        source: e,
    })?;

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let plugin = &manifest.plugin;
    let plugin_id = Some(plugin.id.clone());
    let plugin_name = Some(plugin.name.clone());
    let runtime = Some(plugin.runtime.clone());

    // 1. Validate Plugin ID format (reverse domain convention, e.g. org.kanon.plugin.name)
    if plugin.id.trim().is_empty() {
        errors.push("Plugin 'id' must not be empty".to_string());
    } else {
        if !plugin.id.contains('.') {
            errors.push(format!(
                "Plugin id '{}' does not follow reverse domain notation (e.g. 'org.kanon.plugin.demo')",
                plugin.id
            ));
        }
        for c in plugin.id.chars() {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '.' && c != '_' && c != '-' {
                errors.push(format!(
                    "Plugin id '{}' contains invalid character '{}'. Only lowercase ASCII, digits, '.', '_', and '-' are permitted",
                    plugin.id, c
                ));
                break;
            }
        }
    }

    // 2. Validate Plugin Name and Version
    if plugin.name.trim().is_empty() {
        errors.push("Plugin 'name' must not be empty".to_string());
    }

    if plugin.version.trim().is_empty() {
        errors.push("Plugin 'version' must not be empty".to_string());
    } else {
        let parts: Vec<&str> = plugin.version.split('.').collect();
        if parts.len() < 3 || parts.iter().any(|p| p.parse::<u64>().is_err()) {
            errors.push(format!(
                "Plugin version '{}' is not a valid Semantic Version (expected X.Y.Z)",
                plugin.version
            ));
        }
    }

    // 3. Validate Runtime Target
    let runtime_norm = plugin.runtime.to_lowercase();
    match runtime_norm.as_str() {
        "rust" | "python" | "typescript" | "ts" => {}
        other => {
            errors.push(format!(
                "Unsupported runtime '{}'. Supported runtimes: 'rust', 'python', 'typescript'",
                other
            ));
        }
    }

    // 4. Validate Priority
    if let Some(priority) = plugin.priority
        && !(1..=1000).contains(&priority)
    {
        errors.push(format!(
            "Plugin priority {} is outside valid range [1, 1000]",
            priority
        ));
    }

    // 5. Validate Entrypoint Existence
    if plugin.entrypoint.trim().is_empty() {
        errors.push("Plugin 'entrypoint' must not be empty".to_string());
    } else {
        let entrypoint_path = plugin_root.join(&plugin.entrypoint);
        if runtime_norm == "rust" {
            if !entrypoint_path.exists() {
                warnings.push(format!(
                    "Rust entrypoint binary '{}' not found on disk. Ensure project has been compiled with 'cargo build'",
                    plugin.entrypoint
                ));
            }
        } else if !entrypoint_path.exists() {
            errors.push(format!(
                "Entrypoint script '{}' does not exist at '{}'",
                plugin.entrypoint,
                entrypoint_path.display()
            ));
        }
    }

    // 6. Validate Command Definitions
    let mut command_names = HashSet::new();
    for cmd in &manifest.commands {
        if cmd.name.trim().is_empty() {
            errors.push("Command name must not be empty".to_string());
            continue;
        }

        if cmd.name.starts_with('/') {
            errors.push(format!(
                "Command name '{}' must not include a leading slash (use '{}' instead)",
                cmd.name,
                cmd.name.trim_start_matches('/')
            ));
        }

        if cmd.name.chars().any(char::is_whitespace) {
            errors.push(format!(
                "Command name '{}' must not contain whitespace",
                cmd.name
            ));
        }

        if !command_names.insert(cmd.name.clone()) {
            errors.push(format!(
                "Duplicate command declaration: command '{}' is defined multiple times",
                cmd.name
            ));
        }

        if let Some(priority) = cmd.priority
            && !(1..=1000).contains(&priority)
        {
            errors.push(format!(
                "Command '{}' priority {} is outside valid range [1, 1000]",
                cmd.name, priority
            ));
        }
    }

    // 7. Validate Tool Definitions
    let mut tool_names = HashSet::new();
    for tool in &manifest.tools {
        if tool.name.trim().is_empty() {
            errors.push("Tool name must not be empty".to_string());
            continue;
        }

        for c in tool.name.chars() {
            if !c.is_alphanumeric() && c != '_' && c != '-' {
                errors.push(format!(
                    "Tool name '{}' contains invalid character '{}'. Only alphanumeric characters, '_', and '-' are allowed",
                    tool.name, c
                ));
                break;
            }
        }

        if !tool_names.insert(tool.name.clone()) {
            errors.push(format!(
                "Duplicate tool declaration: tool '{}' is defined multiple times",
                tool.name
            ));
        }

        if let Some(ref params) = tool.parameters {
            if !params.is_object() {
                errors.push(format!(
                    "Tool '{}' parameters schema must be a JSON Schema Object",
                    tool.name
                ));
            } else if let Some(schema_type) = params.get("type") {
                if schema_type != "object" {
                    errors.push(format!(
                        "Tool '{}' parameters schema must declare 'type': 'object'",
                        tool.name
                    ));
                }
            } else {
                warnings.push(format!(
                    "Tool '{}' parameters schema does not explicitly declare 'type': 'object'",
                    tool.name
                ));
            }
        }
    }

    // 8. Validate Platform Adapter Section
    if let Some(ref adapter) = manifest.adapter {
        if adapter.platform.trim().is_empty() {
            errors.push("Adapter 'platform' identifier must not be empty".to_string());
        }
        for c in adapter.platform.chars() {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' && c != '-' {
                errors.push(format!(
                    "Adapter platform '{}' contains invalid character '{}'. Use lowercase alphanumeric, '_', or '-'",
                    adapter.platform, c
                ));
                break;
            }
        }
        if adapter.sends_no_media() {
            warnings.push(format!(
                "Adapter '{}' declares no send_image/send_voice/send_video/send_file capability, so \
                 tool-produced media is left out of its replies. Add the kinds the platform \
                 delivers to [adapter] capabilities, e.g. capabilities = [\"send_image\", \"send_file\"]",
                adapter.platform
            ));
        }
    }

    Ok(LintReport {
        manifest_path,
        plugin_root,
        plugin_id,
        plugin_name,
        runtime,
        errors,
        warnings,
    })
}
