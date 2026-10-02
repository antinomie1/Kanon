//! `GET /api/v1/plugins/market`: plugins offered by the operator's market indexes.
//!
//! The index URLs come from `plugin_market.indexes` in `data/system.json` and are read on every
//! request (concurrently, each bounded by [`INDEX_FETCH_TIMEOUT`] and [`MAX_INDEX_BYTES`]), so an
//! edited configuration or a republished index shows up without a restart. A source that fails
//! is reported in `sources[].error` while the others still answer: one unreachable mirror must not
//! empty the whole market.
//!
//! Installing an entry is not a separate endpoint: the console sends the entry's `download_url`
//! (or `repository`) to `POST /api/v1/plugins/install`, which applies every install-time check.

use std::collections::HashMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::future::join_all;

use crate::error::ApiError;
use crate::plugin_market::{
    MarketPlugin, MarketResponse, MarketSource, UNCONFIGURED_HINT, parse_index,
};
use crate::plugin_sources::{INDEX_FETCH_TIMEOUT, MAX_INDEX_BYTES, download};
use crate::state::ApiState;

/// Registers the plugin market route.
pub fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/plugins/market", get(get_market))
}

/// Reads every configured index and merges their plugins.
async fn get_market(State(state): State<ApiState>) -> Result<Json<MarketResponse>, ApiError> {
    let config = state.system_config().load_plugin_market().map_err(|err| {
        ApiError::Internal(format!(
            "Could not read the plugin market settings in system.json: {err}"
        ))
    })?;
    if config.indexes.is_empty() {
        return Ok(Json(MarketResponse {
            configured: false,
            hint: Some(UNCONFIGURED_HINT.to_string()),
            sources: Vec::new(),
            plugins: Vec::new(),
        }));
    }

    let fetched = join_all(config.indexes.iter().map(|url| async move {
        let result = match download(url, MAX_INDEX_BYTES, INDEX_FETCH_TIMEOUT).await {
            Ok(bytes) => parse_index(&bytes),
            Err(err) => Err(err.to_string()),
        };
        (url.clone(), result)
    }))
    .await;

    let installed = installed_versions(&state).await;
    let mut sources = Vec::with_capacity(fetched.len());
    let mut plugins: Vec<MarketPlugin> = Vec::new();
    for (url, result) in fetched {
        match result {
            Ok(index) => {
                let mut warnings = index.warnings;
                let mut contributed = 0;
                for entry in index.entries {
                    // Sources are in priority order, so the first listing of an id wins and a
                    // later one is reported rather than silently dropped.
                    if let Some(earlier) = plugins.iter().find(|known| known.entry.id == entry.id) {
                        warnings.push(format!(
                            "'{}' is also listed by {}, which takes precedence",
                            entry.id, earlier.source
                        ));
                        continue;
                    }
                    let compatibility = entry
                        .kanon_version
                        .as_deref()
                        .map_or(Ok(()), kanon_core::check_kanon_requirement);
                    plugins.push(MarketPlugin {
                        installed_version: installed.get(&entry.id).cloned(),
                        compatible: compatibility.is_ok(),
                        incompatible_reason: compatibility.err(),
                        source: url.clone(),
                        entry,
                    });
                    contributed += 1;
                }
                sources.push(MarketSource {
                    url,
                    name: index.name,
                    plugins: contributed,
                    error: None,
                    warnings,
                });
            }
            Err(error) => {
                tracing::warn!(url = %url, error = %error, "Plugin market index could not be read");
                sources.push(MarketSource {
                    url,
                    name: None,
                    plugins: 0,
                    error: Some(error),
                    warnings: Vec::new(),
                });
            }
        }
    }

    Ok(Json(MarketResponse {
        configured: true,
        hint: None,
        sources,
        plugins,
    }))
}

/// Versions of the plugins installed on this node, keyed by id.
///
/// A running host's reported version wins over the manifest on disk: it is what actually runs.
async fn installed_versions(state: &ApiState) -> HashMap<String, String> {
    let mut versions = HashMap::new();
    for host in state.supervisor().get_all_hosts().await {
        for meta in host.metas() {
            if !meta.version.is_empty() {
                versions.insert(meta.id, meta.version);
            }
        }
    }
    for plugin in state.plugins_on_disk() {
        if plugin.manifest_path.is_file() {
            versions
                .entry(plugin.manifest.plugin.id)
                .or_insert(plugin.manifest.plugin.version);
        }
    }
    versions
}
