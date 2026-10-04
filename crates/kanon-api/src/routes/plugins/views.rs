//! Plugin catalog views built from live metadata and manifests.

use super::*;

/// Builds the host view for a supervised process.
pub(super) fn host_view(
    host: &ManagedHost,
    pid: Option<u32>,
    plugins: Vec<PluginView>,
) -> HostView {
    HostView {
        host_id: host.host_id.clone(),
        status: "running".to_string(),
        runtime: host
            .manifest()
            .map(|manifest| manifest.plugin.runtime.clone()),
        socket_path: host.socket_path.to_string_lossy().to_string(),
        priority: host.priority,
        plugin_ids: host.metas().iter().map(|meta| meta.id.clone()).collect(),
        restartable: host.launch_spec().is_some(),
        pid,
        plugins,
    }
}

/// Builds plugin views for a host, merging live metadata with the static manifest.
pub(crate) fn plugin_views_for(host: &ManagedHost) -> Result<Vec<PluginView>, ApiError> {
    let manifest = host.manifest();

    let mut views: Vec<PluginView> = host
        .metas()
        .iter()
        .map(|meta| plugin_view_from_meta(meta, host, manifest))
        .collect::<Result<_, _>>()?;

    // A host that declares a manifest but has not reported its plugin (or reports it later)
    // still appears in the catalog as `declared`, so consoles never hide a configured plugin.
    if let Some(manifest) = manifest
        && !views.iter().any(|view| view.id == manifest.plugin.id)
    {
        views.push(plugin_view_from_manifest(host, manifest));
    }

    Ok(views)
}

/// Builds a plugin view from live process metadata.
pub(super) fn plugin_view_from_meta(
    meta: &PluginMeta,
    host: &ManagedHost,
    manifest: Option<&kanon_core::PluginManifest>,
) -> Result<PluginView, ApiError> {
    let fallback = manifest.filter(|m| m.plugin.id == meta.id);

    Ok(PluginView {
        id: meta.id.clone(),
        name: non_empty_or(meta.name.clone(), || {
            fallback.map(|m| m.plugin.name.clone()).unwrap_or_default()
        }),
        version: non_empty_or(meta.version.clone(), || {
            fallback
                .map(|m| m.plugin.version.clone())
                .unwrap_or_default()
        }),
        author: non_empty_or(meta.author.clone(), || {
            fallback
                .and_then(|m| m.plugin.author.clone())
                .unwrap_or_default()
        }),
        description: non_empty_or(meta.description.clone(), || {
            fallback
                .and_then(|m| m.plugin.description.clone())
                .unwrap_or_default()
        }),
        host_id: host.host_id.clone(),
        runtime: fallback.map(|m| m.plugin.runtime.clone()),
        priority: host.priority,
        status: "running".to_string(),
        enabled: true,
        health: None,
        commands: meta
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone(),
                usage: command.usage.clone(),
                priority: command.priority,
            })
            .collect(),
        tools: meta
            .tools
            .iter()
            .map(|tool| {
                Ok(ToolView {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool
                        .parameters
                        .clone()
                        .map(prost_struct_to_json)
                        .transpose()?
                        .unwrap_or_else(|| json!({ "type": "object" })),
                })
            })
            .collect::<Result<_, ApiError>>()?,
        serves_http: meta.serves_http,
        ..PluginView::default()
    })
}

/// Builds a plugin view from the static manifest alone.
pub(super) fn plugin_view_from_manifest(
    host: &ManagedHost,
    manifest: &kanon_core::PluginManifest,
) -> PluginView {
    PluginView {
        id: manifest.plugin.id.clone(),
        name: manifest.plugin.name.clone(),
        version: manifest.plugin.version.clone(),
        author: manifest.plugin.author.clone().unwrap_or_default(),
        description: manifest.plugin.description.clone().unwrap_or_default(),
        host_id: host.host_id.clone(),
        runtime: Some(manifest.plugin.runtime.clone()),
        priority: host.priority,
        status: "declared".to_string(),
        enabled: true,
        health: None,
        commands: manifest
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone().unwrap_or_default(),
                usage: command.usage.clone().unwrap_or_default(),
                priority: command.priority.unwrap_or(500),
            })
            .collect(),
        tools: manifest
            .tools
            .iter()
            .map(|tool| ToolView {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                parameters: tool
                    .parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
            .collect(),
        ..PluginView::default()
    }
}

/// Builds a plugin view from a manifest with an explicit lifecycle status.
pub(crate) fn plugin_view_from_manifest_with_status(
    manifest: &kanon_core::PluginManifest,
    status: &str,
) -> PluginView {
    let host_id = format!("host_{}", manifest.plugin.id.replace('.', "_"));
    PluginView {
        id: manifest.plugin.id.clone(),
        name: manifest.plugin.name.clone(),
        version: manifest.plugin.version.clone(),
        author: manifest.plugin.author.clone().unwrap_or_default(),
        description: manifest.plugin.description.clone().unwrap_or_default(),
        host_id,
        runtime: Some(manifest.plugin.runtime.clone()),
        priority: manifest.plugin.priority.unwrap_or(500),
        status: status.to_string(),
        enabled: true,
        health: None,
        commands: manifest
            .commands
            .iter()
            .map(|command| CommandView {
                name: command.name.clone(),
                description: command.description.clone().unwrap_or_default(),
                usage: command.usage.clone().unwrap_or_default(),
                priority: command.priority.unwrap_or(500),
            })
            .collect(),
        tools: manifest
            .tools
            .iter()
            .map(|tool| ToolView {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                parameters: tool
                    .parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
            .collect(),
        ..PluginView::default()
    }
}

/// Returns `value` when non-empty, otherwise the fallback.
pub(super) fn non_empty_or(value: String, fallback: impl FnOnce() -> String) -> String {
    if value.is_empty() { fallback() } else { value }
}
