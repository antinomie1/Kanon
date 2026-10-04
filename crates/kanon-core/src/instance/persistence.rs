//! Instance catalog migration and atomic persistence.

use super::*;

impl InstanceRegistry {
    /// Reads the catalog document, returning `None` when the file does not exist.
    pub(super) fn read_document(
        path: &Path,
    ) -> Result<Option<InstanceCatalogDocument>, InstanceError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(InstanceError::Io(format!(
                    "failed to read {}: {err}",
                    path.display()
                )));
            }
        };
        let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
            InstanceError::Io(format!("failed to parse {}: {err}", path.display()))
        })?;
        // The first simulation preview stored guidance inside its timing policy. Migrate it
        // at the file boundary: its dormant assistant-mode default must not opt old assistants
        // into new behavior. New writes only contain the independent top-level switch.
        if let Some(instances) = value
            .get_mut("instances")
            .and_then(serde_json::Value::as_array_mut)
        {
            for instance in instances {
                let legacy = instance
                    .get_mut("simulation")
                    .and_then(serde_json::Value::as_object_mut)
                    .and_then(|policy| policy.remove("behavior_prompt"));
                if let Some(legacy) = legacy {
                    let enabled = legacy.as_bool().ok_or_else(|| {
                        InstanceError::Invalid("simulation.behavior_prompt must be boolean".into())
                    })?;
                    if instance.get("conversation_rules").is_none() {
                        instance["conversation_rules"] = serde_json::Value::Bool(
                            enabled && instance["conversation_mode"] == "simulation",
                        );
                    }
                }
            }
        }
        let document: InstanceCatalogDocument = serde_json::from_value(value).map_err(|err| {
            InstanceError::Io(format!("failed to parse {}: {err}", path.display()))
        })?;

        if document.version != 1 {
            return Err(InstanceError::Invalid(format!(
                "{} has schema version {}, expected 1",
                path.display(),
                document.version
            )));
        }
        Ok(Some(document))
    }

    /// Atomically writes the catalog so a crash cannot truncate it.
    ///
    /// An in-memory catalog has no path to write to: that is its documented mode, not a failure.
    pub(super) fn persist(
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
