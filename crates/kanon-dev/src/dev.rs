//! `kanon-dev dev`: reloads a plugin on a running node whenever its files change.
//!
//! The node keeps running; only the plugin's host process is restarted, through the management
//! API (`POST /api/v1/plugins/<id>/restart`). A restart re-reads `plugin.toml`, reinstalls
//! dependencies when the lockfile changed and fetches the plugin's commands, tools and hooks
//! again, so every kind of edit takes effect. Rust plugins are rebuilt first; a failed build
//! leaves the running version untouched. The same reload runs once at startup, so the node runs
//! the files on disk from the first moment, and a plugin whose host crashed is started again.
//!
//! The node must already serve the plugin from this folder: it loads plugins from its own
//! `plugins/` directory, where a symbolic link to the folder being developed works.
//!
//! Changes are found by polling file sizes and modification times. A plugin folder is small, and
//! polling needs no platform-specific watcher.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kanon_core::PluginManifest;
use serde::Deserialize;
use thiserror::Error;

use crate::build::{BuildError, build_plugin};
use crate::lint::{LintError, find_manifest_path};

/// Address of a node started with the default `startup.api_addr`.
pub const DEFAULT_NODE_URL: &str = "http://127.0.0.1:8080";

/// How often the plugin folder is scanned for changes.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long a single management API request may take. A restart includes installing changed
/// dependencies, which can take a while on a slow network.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Directories that never hold plugin sources: build output, installed environments, caches and
/// version control. Watching them would restart the plugin on its own build.
const IGNORED_DIRS: [&str; 6] = [
    "target",
    "node_modules",
    "__pycache__",
    "dist",
    "build",
    "venv",
];

/// Errors that stop `kanon-dev dev`.
#[derive(Debug, Error)]
pub enum DevError {
    /// No manifest at the given path.
    #[error("{0}")]
    Lint(#[from] LintError),
    /// The manifest could not be read.
    #[error("invalid plugin.toml: {0}")]
    Manifest(String),
    /// The node's management API could not be reached or answered unexpectedly.
    #[error("cannot reach the node at {node}: {reason}")]
    Node {
        /// Base URL that was tried.
        node: String,
        /// What went wrong.
        reason: String,
    },
    /// The node does not know the plugin.
    #[error(
        "the node at {node} does not serve '{id}'. Put this folder in the node's plugins/ directory (a symbolic link works) and restart the node or rescan plugins in the console"
    )]
    NotServed {
        /// Base URL of the node.
        node: String,
        /// Plugin identifier from the manifest.
        id: String,
    },
    /// The operator switched the plugin off on the node.
    #[error("plugin '{0}' is disabled on the node; enable it in the console first")]
    Disabled(String),
    /// The manifest's id changed while watching.
    #[error("the plugin id changed from '{from}' to '{to}'; start kanon-dev dev again")]
    IdChanged {
        /// Id the session started with.
        from: String,
        /// Id now in the manifest.
        to: String,
    },
    /// The plugin folder could not be read.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// The part of the node's plugin view this tool reads.
#[derive(Debug, Deserialize)]
struct PluginView {
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    commands: Vec<serde_json::Value>,
    #[serde(default)]
    tools: Vec<serde_json::Value>,
}

/// Response of `POST /api/v1/plugins/<id>/restart`.
#[derive(Debug, Deserialize)]
struct RestartResponse {
    host_id: String,
    plugins: Vec<PluginView>,
}

/// Size and modification time of every watched file, by path.
type Snapshot = BTreeMap<PathBuf, (u64, Option<SystemTime>)>;

/// Watches the plugin at `path` and restarts it on the node at `node` after every change.
///
/// Runs until interrupted (Ctrl+C). Returns early only when the plugin cannot be reloaded at all.
pub async fn run_dev(path: &Path, node: &str) -> Result<(), DevError> {
    let (manifest_path, root) = find_manifest_path(path)?;
    let manifest = load_manifest(&manifest_path)?;
    let id = manifest.plugin.id.clone();
    let node = node.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| DevError::Node {
            node: node.clone(),
            reason: err.to_string(),
        })?;

    let view = fetch_plugin(&client, &node, &id).await?;
    if !view.enabled {
        return Err(DevError::Disabled(id));
    }
    println!(
        "Watching {} — '{}' ({id}) on {node}. Press Ctrl+C to stop.\n",
        root.display(),
        view.name
    );

    // Whatever ran before (an older build, a crashed host), the node now runs the files as they
    // are; afterwards every change does the same.
    let mut snapshot = scan(&root)?;
    reload(&client, &node, &root, &manifest_path, &id).await?;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let mut current = scan(&root)?;
        if current == snapshot {
            continue;
        }
        // An editor's save or a `git checkout` touches files over several polls; waiting for a
        // quiet poll turns it into one reload instead of several.
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let next = scan(&root)?;
            if next == current {
                break;
            }
            current = next;
        }
        println!(
            "Changed: {}",
            describe(&changed_files(&snapshot, &current), &root)
        );
        snapshot = current;
        reload(&client, &node, &root, &manifest_path, &id).await?;
    }
}

/// Rebuilds the plugin if needed and restarts it on the node, printing the outcome.
///
/// A broken manifest, a failed build or a failed restart is reported and leaves the session
/// waiting for the next change; only a changed plugin id ends it.
async fn reload(
    client: &reqwest::Client,
    node: &str,
    root: &Path,
    manifest_path: &Path,
    id: &str,
) -> Result<(), DevError> {
    let manifest = match load_manifest(manifest_path) {
        Ok(manifest) => manifest,
        Err(err) => {
            println!("✗ {err}; waiting for the next change.\n");
            return Ok(());
        }
    };
    if manifest.plugin.id != id {
        return Err(DevError::IdChanged {
            from: id.to_string(),
            to: manifest.plugin.id,
        });
    }
    if let Err(err) = rebuild(root, &manifest).await {
        println!("✗ {err}; the running version is unchanged.\n");
        return Ok(());
    }
    match restart(client, node, id).await {
        Ok(restarted) => {
            let (commands, tools) = restarted.plugins.iter().fold((0, 0), |(c, t), plugin| {
                (c + plugin.commands.len(), t + plugin.tools.len())
            });
            println!(
                "✓ Restarted {} ({commands} commands, {tools} tools).\n",
                restarted.host_id
            );
        }
        // The plugin's own output (a traceback, a panic) is in the node's log, not here.
        Err(reason) => {
            println!("✗ Restart failed: {reason}\n  The plugin's output is in the node's log.\n")
        }
    }
    Ok(())
}

/// Reads and parses the manifest.
fn load_manifest(path: &Path) -> Result<PluginManifest, DevError> {
    PluginManifest::load_from_file(path).map_err(|err| DevError::Manifest(err.to_string()))
}

/// Builds a Rust plugin where the node will launch it from.
///
/// The node runs the manifest's entrypoint, so the binary cargo produced must be that file; a
/// plugin inside a Cargo workspace builds elsewhere and is reported instead of restarted with a
/// stale binary.
async fn rebuild(root: &Path, manifest: &PluginManifest) -> Result<(), String> {
    let Some(executable) = build_plugin(root, manifest)
        .await
        .map_err(|err: BuildError| err.to_string())?
    else {
        return Ok(());
    };
    let entrypoint = root.join(&manifest.plugin.entrypoint);
    let same = match (executable.canonicalize(), entrypoint.canonicalize()) {
        (Ok(built), Ok(launched)) => built == launched,
        _ => false,
    };
    if same {
        Ok(())
    } else {
        Err(format!(
            "cargo built {}, but the node launches {}; give the plugin its own [workspace] or point `entrypoint` at the built binary",
            executable.display(),
            entrypoint.display()
        ))
    }
}

/// Looks the plugin up on the node.
async fn fetch_plugin(
    client: &reqwest::Client,
    node: &str,
    id: &str,
) -> Result<PluginView, DevError> {
    let unreachable = |reason: String| DevError::Node {
        node: node.to_string(),
        reason,
    };
    let response = client
        .get(format!("{node}/api/v1/plugins/{id}"))
        .send()
        .await
        .map_err(|err| {
            unreachable(format!(
                "{err}. Start `kanon` first (its address is `startup.api_addr` in data/system.json) or pass --node"
            ))
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(DevError::NotServed {
            node: node.to_string(),
            id: id.to_string(),
        });
    }
    if !response.status().is_success() {
        return Err(unreachable(api_error(response).await));
    }
    response
        .json()
        .await
        .map_err(|err| unreachable(format!("unexpected plugin description: {err}")))
}

/// Asks the node to restart the plugin's host.
async fn restart(
    client: &reqwest::Client,
    node: &str,
    id: &str,
) -> Result<RestartResponse, String> {
    let response = client
        .post(format!("{node}/api/v1/plugins/{id}/restart"))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(api_error(response).await);
    }
    response
        .json()
        .await
        .map_err(|err| format!("unexpected restart response: {err}"))
}

/// The message of a management API error (`{"error": {"code", "message"}}`), or the raw body.
async fn api_error(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("HTTP {status}: {body}"))
}

/// Records every watched file under `root`.
fn scan(root: &Path) -> std::io::Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Hidden entries are editor state, environments (`.venv`) or version control.
            if name.starts_with('.') {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !IGNORED_DIRS.contains(&name.as_ref()) {
                    pending.push(entry.path());
                }
            } else if let Ok(metadata) = entry.metadata() {
                // A file deleted between listing and reading simply drops out of the snapshot.
                snapshot.insert(entry.path(), (metadata.len(), metadata.modified().ok()));
            }
        }
    }
    Ok(snapshot)
}

/// Files added, removed or modified between two snapshots.
fn changed_files(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let mut changed: Vec<PathBuf> = after
        .iter()
        .filter(|(path, stamp)| before.get(*path) != Some(stamp))
        .map(|(path, _)| path.clone())
        .collect();
    changed.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .cloned(),
    );
    changed
}

/// A one-line summary of changed files, relative to the plugin folder.
fn describe(changed: &[PathBuf], root: &Path) -> String {
    let mut names: Vec<String> = changed
        .iter()
        .take(3)
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    if changed.len() > 3 {
        names.push(format!("and {} more", changed.len() - 3));
    }
    names.join(", ")
}
