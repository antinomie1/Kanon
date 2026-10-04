//! Plugin configuration read, validation and durable reload transactions.

use super::*;

/// Returns the current configuration and declaration schema for a plugin.
pub(super) async fn get_config(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
) -> Result<Json<PluginConfigView>, ApiError> {
    let transaction = state.supervisor().lock_plugin_config(&plugin_id).await;
    let host = state
        .supervisor()
        .find_host_for_plugin(&plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let schema = host
        .manifest()
        .and_then(|manifest| manifest.config_schema.clone());

    let store = state.config_store();
    let stored = store.load(&plugin_id)?;
    let persisted = stored.as_object().is_some_and(|map| !map.is_empty());
    let values = PluginConfigStore::apply_defaults(schema.as_ref(), &stored);
    let version = transaction.version();

    Ok(Json(PluginConfigView {
        plugin_id,
        values,
        schema: schema.unwrap_or(Value::Null),
        persisted,
        version,
    }))
}

/// Completes a configuration commit even if its HTTP caller disconnects during the host RPC.
pub(super) async fn put_config(
    State(state): State<ApiState>,
    AxumPath(plugin_id): AxumPath<String>,
    Json(body): Json<UpdateConfigRequest>,
) -> Result<Json<UpdateConfigResponse>, ApiError> {
    // Reserve the transaction before detaching it. A caller cancelled while queued has made
    // no change, and shutdown can discover every detached commit through its held guard.
    let transaction = state.supervisor().lock_plugin_config(&plugin_id).await;
    tokio::spawn(async move {
        let result = commit_plugin_config(&state, &plugin_id, body, transaction).await;
        // A disconnected caller no longer consumes the response, so transaction failures must
        // be logged by the task that owns the lock and rollback, not just by IntoResponse.
        if let Err(error) = &result {
            tracing::error!(plugin_id = %plugin_id, %error, "Plugin configuration commit failed");
        }
        result.map(Json)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("Configuration task failed: {error}")))?
}

/// Saves and applies one candidate under the same lock used by readers and lifecycle routes.
pub(super) async fn commit_plugin_config(
    state: &ApiState,
    plugin_id: &str,
    body: UpdateConfigRequest,
    mut transaction: kanon_core::supervisor::PluginConfigGuard,
) -> Result<UpdateConfigResponse, ApiError> {
    if !body.values.is_object() {
        return Err(ApiError::BadRequest(
            "Field 'values' must be a JSON object".to_string(),
        ));
    }

    transaction.check_version(body.version)?;
    let host = state
        .supervisor()
        .find_host_for_plugin(plugin_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Plugin '{plugin_id}' is not loaded by any active host"
            ))
        })?;

    let schema = host
        .manifest()
        .and_then(|manifest| manifest.config_schema.clone());

    PluginConfigStore::validate(schema.as_ref(), &body.values)
        .map_err(|reason| ApiError::BadRequest(format!("Invalid configuration: {reason}")))?;

    // Persist before changing the host: a full disk or read-only directory must not change live
    // behavior. Retain the exact previous file until the host accepts, including its absence.
    let store = state.config_store().clone();
    let values = body.values.clone();
    let persist_id = plugin_id.to_string();
    let backup = tokio::task::spawn_blocking(move || store.replace(&persist_id, &values))
        .await
        .map_err(|err| ApiError::Internal(format!("Persistence task failed: {err}")))??;

    let applied_version = match transaction.reload(&host, &body.values).await {
        Ok(version) => version,
        Err(error) => {
            let restored = tokio::task::spawn_blocking(move || backup.restore())
                .await
                .map_err(|err| ApiError::Internal(format!("Restoration task failed: {err}")))
                .and_then(std::convert::identity);
            let rejected = matches!(
                &error,
                kanon_core::SupervisorError::ConfigReloadRejected { .. }
            );
            if !rejected || restored.is_err() {
                // A lost RPC response cannot prove which values the host accepted. Restoring a
                // file alone is insufficient: remove this host until a restart reads that file.
                let stopped = state.supervisor().stop_host(&host.host_id).await;
                if let Err(restore_error) = restored {
                    return Err(ApiError::Internal(format!(
                        "{error}; previous configuration could not be restored: {restore_error}; \
                         host stop result: {stopped:?}. Inspect the saved configuration before restarting"
                    )));
                }
                stopped?;
                return Err(ApiError::Upstream(format!(
                    "{error}; the previous configuration was restored and the host was removed \
                     from routing because its applied configuration could not be confirmed"
                )));
            }
            return Err(error.into());
        }
    };

    state
        .observability()
        .events
        .publish(TraceEvent::PluginConfigUpdated {
            plugin_id: plugin_id.to_string(),
            host_id: host.host_id.clone(),
        });

    tracing::info!(
        plugin_id = %plugin_id,
        host_id = %host.host_id,
        version = applied_version,
        "Plugin configuration updated and hot reloaded"
    );

    Ok(UpdateConfigResponse {
        plugin_id: plugin_id.to_string(),
        host_id: host.host_id.clone(),
        values: body.values,
        reloaded: true,
        version: applied_version,
    })
}
