//! Plugin configuration store with JSON Schema validation and atomic persistence.
//!
//! Configuration lives in the core's control plane, not inside plugin processes: the plugin
//! only ever receives a pushed snapshot through `ReloadPluginConfig` and keeps it in memory
//! (see the read-cache rule in the persistence spec). Values are persisted under the plugin's
//! isolated data directory (`./data/plugins/<id>/config.json`) so restarts are deterministic.
//!
//! # Validation scope
//! The declared `[config_schema]` is a JSON Schema object. This store validates the subset a
//! management console can actually violate — object-ness, required keys, declared JSON types,
//! enum membership and explicit `additionalProperties = false` — instead of pretending to be a
//! full JSON Schema engine. Anything outside that subset is left for the plugin to judge and
//! report through its reload response.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::ApiError;

/// File name used for persisted plugin configuration.
const CONFIG_FILE_NAME: &str = "config.json";

/// Exact previous file state, retained until a running host accepts a saved candidate.
pub(crate) struct PluginConfigBackup {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl PluginConfigBackup {
    /// Restores the previous file, including absence for a plugin configured for the first time.
    pub(crate) fn restore(self) -> Result<(), ApiError> {
        match self.previous {
            Some(bytes) => write_atomic(&self.path, &bytes),
            None => {
                std::fs::remove_file(self.path)?;
                Ok(())
            }
        }
    }
}

/// Uses a unique file in the destination directory so concurrent callers never share a staging file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let dir = path
        .parent()
        .ok_or_else(|| ApiError::Internal("Configuration path has no parent directory".into()))?;
    std::fs::create_dir_all(dir)?;
    let mut pending = tempfile::NamedTempFile::new_in(dir)?;
    pending.write_all(bytes)?;
    pending.as_file().sync_all()?;
    pending.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Persistent store for per-plugin configuration values.
#[derive(Debug, Clone)]
pub struct PluginConfigStore {
    base_dir: PathBuf,
}

impl Default for PluginConfigStore {
    fn default() -> Self {
        Self::new(kanon_storage::PluginDataDir::DEFAULT_BASE)
    }
}

impl PluginConfigStore {
    /// Creates a store rooted at the given base data directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Returns the configured base data directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Resolves the persistence path for a plugin's configuration file.
    ///
    /// The plugin identifier is validated first: it becomes a directory name, so accepting
    /// separators or `..` would let a crafted identifier escape the sandbox base directory.
    pub fn config_path(&self, plugin_id: &str) -> Result<PathBuf, ApiError> {
        validate_plugin_id(plugin_id)?;
        Ok(self.base_dir.join(plugin_id).join(CONFIG_FILE_NAME))
    }

    /// Loads the persisted configuration values for a plugin.
    ///
    /// Returns an empty object when the plugin has never been configured; the caller merges
    /// schema defaults on top so the console always renders meaningful initial values.
    pub fn load(&self, plugin_id: &str) -> Result<Value, ApiError> {
        let path = self.config_path(plugin_id)?;
        if !path.exists() {
            return Ok(Value::Object(Map::new()));
        }

        let raw = std::fs::read_to_string(&path)?;
        let parsed: Value = serde_json::from_str(&raw).map_err(|err| {
            ApiError::Internal(format!(
                "Persisted configuration for plugin '{plugin_id}' is not valid JSON: {err}"
            ))
        })?;

        match parsed {
            Value::Object(map) => Ok(Value::Object(map)),
            _ => Err(ApiError::Internal(format!(
                "Persisted configuration for plugin '{plugin_id}' must be a JSON object"
            ))),
        }
    }

    /// Persists configuration values for a plugin using a write-then-rename commit.
    ///
    /// The temporary file plus `rename` sequence guarantees the commit point is atomic: a
    /// crash mid-write leaves the previous configuration intact instead of a truncated file.
    pub fn store(&self, plugin_id: &str, values: &Value) -> Result<(), ApiError> {
        let path = self.config_path(plugin_id)?;
        let serialized = serde_json::to_string_pretty(values).map_err(|err| {
            ApiError::Internal(format!("Failed to serialize configuration: {err}"))
        })?;
        write_atomic(&path, serialized.as_bytes())
    }

    /// Saves a candidate and keeps the exact previous file for a rejected hot reload.
    ///
    /// The caller must hold the supervisor's per-plugin configuration guard through acceptance
    /// or restoration. A filesystem failure here occurs before the host sees the candidate.
    pub(crate) fn replace(
        &self,
        plugin_id: &str,
        values: &Value,
    ) -> Result<PluginConfigBackup, ApiError> {
        let path = self.config_path(plugin_id)?;
        let previous = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        self.store(plugin_id, values)?;
        Ok(PluginConfigBackup { path, previous })
    }

    /// Merges schema-declared defaults underneath the provided values.
    ///
    /// Explicitly persisted values always win; defaults only fill gaps so a freshly
    /// discovered plugin reports a usable configuration immediately.
    pub fn apply_defaults(schema: Option<&Value>, values: &Value) -> Value {
        let mut merged = declared_defaults(schema);
        if let (Value::Object(target), Value::Object(provided)) = (&mut merged, values) {
            for (key, value) in provided {
                target.insert(key.clone(), value.clone());
            }
        }
        merged
    }

    /// Validates configuration values against a plugin's declared JSON Schema.
    ///
    /// Returns a human-readable reason on the first violation encountered.
    pub fn validate(schema: Option<&Value>, values: &Value) -> Result<(), String> {
        let Some(schema) = schema else {
            // No schema declared: any JSON object is acceptable, but it must still be an object
            // because plugin configuration is passed as a `google.protobuf.Struct`.
            return match values {
                Value::Object(_) => Ok(()),
                _ => Err("configuration must be a JSON object".to_string()),
            };
        };

        let object = match values {
            Value::Object(map) => map,
            _ => return Err("configuration must be a JSON object".to_string()),
        };

        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for entry in required {
                if let Some(name) = entry.as_str()
                    && !object.contains_key(name)
                {
                    return Err(format!("missing required property '{name}'"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(properties) = properties {
            for (key, value) in object {
                let Some(property_schema) = properties.get(key) else {
                    continue;
                };
                validate_property(key, property_schema, value)?;
            }
        }

        // Honour strict schemas that opt out of additional properties entirely.
        let strict = schema
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .map(|allowed| !allowed)
            .unwrap_or(false);
        if strict && let Some(properties) = properties {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("unknown property '{key}'"));
                }
            }
        }

        Ok(())
    }
}

/// Validates a single property value against its declared schema.
fn validate_property(key: &str, property_schema: &Value, value: &Value) -> Result<(), String> {
    if let Some(expected) = property_schema.get("type").and_then(Value::as_str)
        && !matches_json_type(expected, value)
    {
        return Err(format!(
            "property '{key}' must be of type '{expected}' but received {}",
            json_type_name(value)
        ));
    }

    if let Some(allowed) = property_schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Err(format!(
            "property '{key}' must be one of {}",
            serde_json::to_string(allowed).unwrap_or_else(|_| "[]".to_string())
        ));
    }

    Ok(())
}

/// Returns `true` when a JSON value satisfies a JSON Schema type name.
///
/// `integer` additionally accepts floating point values with no fractional part, matching
/// how consoles serialize whole numbers coming from HTML number inputs.
fn matches_json_type(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => match value {
            Value::Number(number) => {
                number.is_i64()
                    || number.is_u64()
                    || number.as_f64().is_some_and(|float| float.fract() == 0.0)
            }
            _ => false,
        },
        "null" => value.is_null(),
        // Unknown type names are not enforced here: judging custom vocabularies is the
        // plugin's responsibility, and rejecting valid configuration would be worse.
        _ => true,
    }
}

/// Returns the JSON type name of a value, for error messages.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Collects `default` values declared on top-level schema properties.
fn declared_defaults(schema: Option<&Value>) -> Value {
    let mut defaults = Map::new();

    if let Some(properties) = schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    {
        for (key, property) in properties {
            if let Some(default) = property.get("default") {
                defaults.insert(key.clone(), default.clone());
            }
        }
    }

    Value::Object(defaults)
}

/// Ensures a plugin identifier conforms to canonical security constraints.
fn validate_plugin_id(plugin_id: &str) -> Result<(), ApiError> {
    kanon_storage::PluginId::validate(plugin_id).map_err(|err| {
        ApiError::BadRequest(format!("Invalid plugin identifier '{plugin_id}': {err}"))
    })
}
