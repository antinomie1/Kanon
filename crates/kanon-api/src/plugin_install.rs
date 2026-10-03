//! Plugin installation from a folder, a package (uploaded or downloaded) or a Git repository.
//!
//! Every source ends in the same place — a folder holding `plugin.toml` — and goes through one
//! installer, so the checks below apply no matter where the plugin came from:
//!
//! 1. **Manifest**: it must parse, carry a valid id, name an entrypoint inside the plugin folder
//!    and admit this node's version (`kanon_version`).
//! 2. **Id collisions**: a plugin id lives in exactly one folder, `plugins/<id>/`. Installing an id
//!    that another folder already declares, or that a host this node did not start from that
//!    folder already serves, is refused (`409`): two copies would fight over one id. Installing
//!    over the same id in `plugins/<id>/` is an upgrade and needs the caller's explicit `replace`.
//! 3. **Staged replacement**: the new files are copied into a hidden staging folder first; only
//!    when that succeeded is the running host stopped and the folder swapped in by rename. A failed
//!    download, extraction or copy therefore leaves the installed version untouched and running.
//!
//! Packages are bounded: at most [`MAX_PACKAGE_BYTES`] compressed, [`MAX_UNPACKED_BYTES`]
//! extracted and [`MAX_PACKAGE_ENTRIES`] entries, so a small "zip bomb" cannot fill the disk.
//! A Git checkout is held to the same extracted-size limit before it is copied.
//!
//! Environments are never copied: `.venv`, `node_modules` and other hidden or build folders are
//! skipped, and the supervisor installs the plugin's dependencies with its ecosystem's own tool
//! when it launches the plugin (see `kanon_core::DependencyInstaller`). A failed install reports
//! the plugin `RuntimeUnavailable`, exactly as when it is copied in by hand.

use std::path::{Component, Path, PathBuf};

use kanon_core::{
    DiscoveredPlugin, LaunchSpec, ManagedHost, PluginManifest, PluginScanner, SupervisorError,
};
use kanon_storage::PluginId;

use crate::error::ApiError;
use crate::observability::TraceEvent;
use crate::plugin_sources::{
    self, FetchError, GIT_CLONE_TIMEOUT, GitError, PACKAGE_DOWNLOAD_TIMEOUT,
};
use crate::routes::plugins::{
    InstallPluginResponse, plugin_view_from_manifest_with_status, plugin_views_for,
};
use crate::state::ApiState;

pub use crate::plugin_sources::MAX_PACKAGE_BYTES;

/// Largest total size a package may extract to (or a Git checkout may have).
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Most entries a package may contain.
pub const MAX_PACKAGE_ENTRIES: usize = 10_000;

/// Where the plugin to install comes from.
#[derive(Debug, Clone)]
pub enum InstallSource {
    /// A folder on this node (or its `plugin.toml`).
    Path(PathBuf),
    /// Bytes of a `.kpk` / `.zip` package.
    Archive(Vec<u8>),
    /// An `https` URL of a `.kpk` / `.zip` package.
    Url(String),
    /// A Git repository, optionally at a branch or tag.
    Git {
        /// Repository URL.
        url: String,
        /// Branch or tag; the remote's default branch when absent.
        git_ref: Option<String>,
    },
}

/// Installs a plugin from `source`.
///
/// `replace` must be set to overwrite a plugin already installed under the same id; without it
/// such an install is refused with `409` so an upgrade is always a deliberate act.
pub async fn install(
    state: &ApiState,
    source: InstallSource,
    replace: bool,
) -> Result<InstallPluginResponse, ApiError> {
    match source {
        InstallSource::Path(path) => install_from_dir(state, &path, replace).await,
        InstallSource::Archive(bytes) => install_from_archive(state, bytes, replace).await,
        InstallSource::Url(url) => {
            let bytes = plugin_sources::download(&url, MAX_PACKAGE_BYTES, PACKAGE_DOWNLOAD_TIMEOUT)
                .await
                .map_err(|err| fetch_error(&url, err))?;
            tracing::info!(url = %url, bytes = bytes.len(), "Plugin package downloaded");
            install_from_archive(state, bytes, replace).await
        }
        InstallSource::Git { url, git_ref } => {
            let checkout = tempfile::tempdir()?;
            let repo = checkout.path().join("repo");
            plugin_sources::git_clone(&url, git_ref.as_deref(), &repo, GIT_CLONE_TIMEOUT)
                .await
                .map_err(git_error)?;
            let size = {
                let repo = repo.clone();
                tokio::task::spawn_blocking(move || tree_size(&repo))
                    .await
                    .map_err(|err| ApiError::Internal(format!("Size check failed: {err}")))??
            };
            if size > MAX_UNPACKED_BYTES {
                return Err(ApiError::BadRequest(format!(
                    "The repository checkout is {size} bytes; plugins are limited to {MAX_UNPACKED_BYTES} bytes"
                )));
            }
            tracing::info!(url = %url, git_ref = ?git_ref, "Plugin repository cloned");
            install_from_dir(state, &repo, replace).await
        }
    }
}

/// Maps a download failure onto the API taxonomy: the caller's URL is a `400`, the remote's
/// misbehaviour a `502`, a stalled transfer a `504`.
fn fetch_error(url: &str, err: FetchError) -> ApiError {
    match err {
        FetchError::InvalidUrl(reason) => ApiError::BadRequest(reason),
        FetchError::TooLarge { limit } => ApiError::BadRequest(format!(
            "The package at {url} is larger than the {limit}-byte limit"
        )),
        FetchError::Timeout(_) => ApiError::Timeout(format!("Downloading {url}: {err}")),
        FetchError::Status(_) | FetchError::Transport(_) => {
            ApiError::Upstream(format!("Downloading {url}: {err}"))
        }
    }
}

/// Maps a clone failure onto the API taxonomy.
fn git_error(err: GitError) -> ApiError {
    match err {
        GitError::Missing => ApiError::Unavailable(err.to_string()),
        GitError::InvalidUrl(_) | GitError::InvalidRef(_) => ApiError::BadRequest(err.to_string()),
        GitError::Timeout(_) => ApiError::Timeout(err.to_string()),
        GitError::Failed(_) => ApiError::Upstream(err.to_string()),
    }
}

/// Extracts a package into a temporary folder and installs the plugin it contains.
async fn install_from_archive(
    state: &ApiState,
    bytes: Vec<u8>,
    replace: bool,
) -> Result<InstallPluginResponse, ApiError> {
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "The package is {} bytes; packages are limited to {MAX_PACKAGE_BYTES} bytes",
            bytes.len()
        )));
    }
    let temp_dir = tempfile::tempdir()?;
    let extract_into = temp_dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || extract_archive(&bytes, &extract_into))
        .await
        .map_err(|err| ApiError::Internal(format!("Extraction task failed: {err}")))??;

    let source_dir = if temp_dir.path().join("plugin.toml").is_file() {
        temp_dir.path().to_path_buf()
    } else if let Some(manifest_path) = PluginScanner::find_manifest_in_dir(temp_dir.path()) {
        manifest_path
            .parent()
            .unwrap_or(temp_dir.path())
            .to_path_buf()
    } else {
        return Err(ApiError::BadRequest(
            "The package does not contain a plugin.toml file".to_string(),
        ));
    };

    install_from_dir(state, &source_dir, replace).await
}

/// Unpacks a ZIP container under the entry-count and extracted-size limits.
///
/// Entry names are taken through `enclosed_name`, which rejects absolute paths and `..`, so no
/// entry can be written outside `into`. Permission bits are reduced to read/write/execute: a
/// package can mark its entrypoint executable but never setuid.
fn extract_archive(bytes: &[u8], into: &Path) -> Result<(), ApiError> {
    if bytes.len() < 22 {
        return Err(ApiError::BadRequest(
            "The package is too small to be a ZIP archive".to_string(),
        ));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|err| {
        ApiError::BadRequest(format!("The package is not a valid ZIP archive: {err}"))
    })?;
    if zip.len() > MAX_PACKAGE_ENTRIES {
        return Err(ApiError::BadRequest(format!(
            "The package has {} entries; packages are limited to {MAX_PACKAGE_ENTRIES}",
            zip.len()
        )));
    }

    let mut total: u64 = 0;
    for index in 0..zip.len() {
        let mut file = zip
            .by_index(index)
            .map_err(|err| ApiError::BadRequest(format!("Corrupt package entry: {err}")))?;
        let Some(enclosed) = file.enclosed_name() else {
            return Err(ApiError::BadRequest(format!(
                "The package entry '{}' points outside the plugin folder",
                file.name()
            )));
        };
        let out_path = into.join(enclosed);

        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        // `take` bounds the copy by what is left of the budget (+1 to detect the overflow), so the
        // declared size in the archive header — which an attacker controls — is never trusted.
        let budget = MAX_UNPACKED_BYTES - total;
        let copied = std::io::copy(&mut std::io::Read::take(&mut file, budget + 1), &mut out)?;
        total += copied;
        if total > MAX_UNPACKED_BYTES {
            return Err(ApiError::BadRequest(format!(
                "The package extracts to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }

        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &out_path,
                std::fs::Permissions::from_mode((mode & 0o755) | 0o600),
            )?;
        }
    }
    Ok(())
}

/// Installs the plugin whose manifest is in (or is) `source_path`.
async fn install_from_dir(
    state: &ApiState,
    source_path: &Path,
    replace: bool,
) -> Result<InstallPluginResponse, ApiError> {
    let manifest_file = locate_manifest(source_path)?;
    let manifest = PluginManifest::load_from_file(&manifest_file)
        .map_err(|err| ApiError::BadRequest(format!("Invalid plugin manifest: {err}")))?;
    validate_manifest(&manifest)?;

    let plugin_id = manifest.plugin.id.clone();
    // Replacement must not switch the registered host halfway through a configuration commit.
    let _configuration = state.supervisor().lock_plugin_config(&plugin_id).await;
    let source_dir = manifest_file.parent().unwrap_or(source_path).to_path_buf();
    let plugins_root = state.plugins_dir().to_path_buf();
    std::fs::create_dir_all(&plugins_root)?;

    // A stale snapshot entry whose folder was deleted by hand no longer claims anything.
    let known = state
        .plugins_on_disk()
        .into_iter()
        .find(|plugin| plugin.manifest.plugin.id == plugin_id && plugin.manifest_path.is_file());
    // An installed plugin keeps its folder whatever it is called (a hand-copied `demo_weather/`
    // need not match the id): reinstalling from that folder registers it in place, and a
    // replacement lands there too, so one id never ends up in two folders.
    let dest_dir = match &known {
        Some(plugin)
            if plugin
                .plugin_dir
                .parent()
                .is_some_and(|parent| same_directory(parent, &plugins_root)) =>
        {
            plugin.plugin_dir.clone()
        }
        _ => plugins_root.join(&plugin_id),
    };
    let in_place = same_directory(&source_dir, &dest_dir);

    let running = state.supervisor().find_host_for_plugin(&plugin_id).await;
    check_collisions(
        &manifest,
        known.as_ref(),
        &dest_dir,
        in_place,
        replace,
        running.as_deref(),
    )?;

    // Copy first, while the installed version keeps running: everything that can fail slowly
    // (a large copy, a full disk) happens before anything is stopped or replaced.
    let staged = if in_place {
        None
    } else {
        let staging = tempfile::Builder::new()
            .prefix(".kanon-install-")
            .tempdir_in(&plugins_root)?;
        let (from, to) = (source_dir.clone(), staging.path().to_path_buf());
        let entrypoint = manifest.plugin.entrypoint.trim().to_string();
        tokio::task::spawn_blocking(move || {
            copy_dir_recursive(&from, &to)?;
            copy_entrypoint(&from, &to, &entrypoint)
        })
        .await
        .map_err(|err| ApiError::Internal(format!("Copy task failed: {err}")))?
        .map_err(|err| ApiError::Internal(format!("Failed to copy the plugin files: {err}")))?;
        Some(staging)
    };

    // Point of no return: the old host stops so its files can be replaced.
    if let Some(host) = &running {
        state
            .supervisor()
            .stop_host(&host.host_id)
            .await
            .map_err(|err| {
                ApiError::Internal(format!(
                    "Could not stop the running host '{}' before replacing it: {err}",
                    host.host_id
                ))
            })?;
    }
    if let Some(staging) = staged {
        swap_into_place(staging, &dest_dir, &plugins_root)?;
    }

    ensure_entrypoint_executable(&dest_dir, &manifest.plugin.entrypoint);

    let target_manifest_path = dest_dir.join("plugin.toml");
    // The files are in place, so the node knows the plugin from here on, even if its host fails
    // to start below: the operator can then fix the cause and enable it from the console.
    state.record_installed_plugin(DiscoveredPlugin {
        manifest: manifest.clone(),
        manifest_path: target_manifest_path.clone(),
        plugin_dir: dest_dir.clone(),
    });

    // Reinstalling a plugin the operator switched off must not switch it back on behind their back.
    if !state
        .plugin_state()
        .is_enabled(kanon_core::PLUGIN_SECTION, &plugin_id)
        .await
    {
        let declared = plugin_view_from_manifest_with_status(&manifest, "disabled");
        return Ok(response(
            &manifest,
            declared.commands,
            declared.tools,
            "disabled",
            "Plugin installed; it stays off because it is disabled on this node",
        ));
    }

    match state
        .supervisor()
        .spawn_from_manifest(&target_manifest_path, None)
        .await
    {
        Ok(host) => {
            state
                .observability()
                .events
                .publish(TraceEvent::PluginInstalled {
                    plugin_id: plugin_id.clone(),
                    host_id: host.host_id.clone(),
                });
            let (commands, tools) = plugin_views_for(&host)
                .into_iter()
                .find(|view| view.id == plugin_id)
                .map(|view| (view.commands, view.tools))
                .unwrap_or_else(|| {
                    let declared = plugin_view_from_manifest_with_status(&manifest, "running");
                    (declared.commands, declared.tools)
                });
            tracing::info!(
                plugin_id = %plugin_id,
                host_id = %host.host_id,
                "Plugin installed and host spawned successfully"
            );
            Ok(response(
                &manifest,
                commands,
                tools,
                "running",
                "Plugin installed and launched successfully",
            ))
        }
        Err(SupervisorError::RuntimeUnavailable { runtime, reason }) => {
            tracing::warn!(
                plugin_id = %plugin_id,
                runtime = %runtime,
                reason = %reason,
                "Plugin installed but runtime is unavailable"
            );
            let declared = plugin_view_from_manifest_with_status(&manifest, "RuntimeUnavailable");
            let mut view = response(
                &manifest,
                declared.commands,
                declared.tools,
                "RuntimeUnavailable",
                &format!("Plugin installed but runtime is unavailable: {reason}"),
            );
            view.runtime = runtime;
            Ok(view)
        }
        Err(err) => Err(ApiError::BadRequest(format!(
            "Plugin files were installed but its host failed to launch: {err}"
        ))),
    }
}

/// Builds the installation response.
fn response(
    manifest: &PluginManifest,
    commands: Vec<crate::routes::plugins::CommandView>,
    tools: Vec<crate::routes::plugins::ToolView>,
    status: &str,
    message: &str,
) -> InstallPluginResponse {
    InstallPluginResponse {
        plugin_id: manifest.plugin.id.clone(),
        name: manifest.plugin.name.clone(),
        version: manifest.plugin.version.clone(),
        runtime: manifest.plugin.runtime.clone(),
        commands,
        tools,
        status: status.to_string(),
        message: Some(message.to_string()),
    }
}

/// Finds the manifest of the plugin at `source_path` (a folder or a `plugin.toml`).
fn locate_manifest(source_path: &Path) -> Result<PathBuf, ApiError> {
    if !source_path.exists() {
        return Err(ApiError::BadRequest(format!(
            "Source path does not exist: {}",
            source_path.display()
        )));
    }
    if source_path.is_file()
        && source_path
            .file_name()
            .is_some_and(|name| name == "plugin.toml")
    {
        return Ok(source_path.to_path_buf());
    }
    if !source_path.is_dir() {
        return Err(ApiError::BadRequest(format!(
            "Source path is neither a directory nor a plugin.toml file: {}",
            source_path.display()
        )));
    }
    let candidate = source_path.join("plugin.toml");
    if candidate.is_file() {
        return Ok(candidate);
    }
    PluginScanner::find_manifest_in_dir(source_path).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "No plugin.toml found in directory: {}",
            source_path.display()
        ))
    })
}

/// Install-time manifest checks beyond what parsing already enforces.
fn validate_manifest(manifest: &PluginManifest) -> Result<(), ApiError> {
    PluginId::validate(&manifest.plugin.id)
        .map_err(|err| ApiError::BadRequest(format!("Invalid plugin identifier: {err}")))?;

    // The entrypoint is executed by the node: it must be a file the package itself provides,
    // never `/bin/sh` or `../../something` outside the plugin folder.
    let entrypoint = manifest.plugin.entrypoint.trim();
    let path = Path::new(entrypoint);
    let inside = !entrypoint.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(ApiError::BadRequest(format!(
            "Invalid plugin manifest: entrypoint '{entrypoint}' must be a relative path inside the plugin folder"
        )));
    }

    kanon_core::check_kanon_version(&manifest.plugin)
        .map_err(|err| ApiError::BadRequest(err.to_string()))
}

/// Refuses installs that would leave two folders, or two hosts, claiming one plugin id.
///
/// `known` is the installed copy of the plugin, if any; it only differs from `dest_dir` when it
/// lives outside the plugins directory, which the installer never writes to.
fn check_collisions(
    manifest: &PluginManifest,
    known: Option<&DiscoveredPlugin>,
    dest_dir: &Path,
    in_place: bool,
    replace: bool,
    running: Option<&ManagedHost>,
) -> Result<(), ApiError> {
    let plugin_id = &manifest.plugin.id;

    if let Some(known) = known
        && !same_directory(&known.plugin_dir, dest_dir)
    {
        return Err(ApiError::Conflict(format!(
            "Plugin '{plugin_id}' is already installed in {}; remove that folder before installing another copy",
            known.plugin_dir.display()
        )));
    }

    if let Some(host) = running {
        let from_dest = match host.launch_spec() {
            Some(LaunchSpec::Manifest { manifest_path, .. }) => manifest_path
                .parent()
                .is_some_and(|dir| same_directory(dir, dest_dir)),
            _ => false,
        };
        if !from_dest {
            return Err(ApiError::Conflict(format!(
                "Plugin '{plugin_id}' is already served by host '{}', which this node did not start from {}",
                host.host_id,
                dest_dir.display()
            )));
        }
    }

    if !in_place && !replace && dest_dir.exists() {
        let installed = known
            .map(|plugin| format!(" (version {})", plugin.manifest.plugin.version))
            .unwrap_or_default();
        return Err(ApiError::Conflict(format!(
            "Plugin '{plugin_id}' is already installed{installed}; install again with \"replace\" to replace it with version {}",
            manifest.plugin.version
        )));
    }

    Ok(())
}

/// Moves a fully staged plugin folder to `dest_dir`, keeping the old one until the swap succeeded.
fn swap_into_place(
    staging: tempfile::TempDir,
    dest_dir: &Path,
    plugins_root: &Path,
) -> Result<(), ApiError> {
    if !dest_dir.exists() {
        std::fs::rename(staging.path(), dest_dir).map_err(|err| {
            ApiError::Internal(format!(
                "Failed to move the plugin into {}: {err}",
                dest_dir.display()
            ))
        })?;
        return Ok(());
    }

    // The previous version is parked in a hidden folder (the scanner skips dot-folders) and only
    // deleted — by the guard's drop — once the new version is in place.
    let parking = tempfile::Builder::new()
        .prefix(".kanon-replaced-")
        .tempdir_in(plugins_root)?;
    let previous = parking.path().join("previous");
    std::fs::rename(dest_dir, &previous).map_err(|err| {
        ApiError::Internal(format!(
            "Failed to move the installed version out of {}: {err}",
            dest_dir.display()
        ))
    })?;
    if let Err(err) = std::fs::rename(staging.path(), dest_dir) {
        // Put the old version back so a failed upgrade leaves a working plugin behind.
        let restored = std::fs::rename(&previous, dest_dir);
        return Err(ApiError::Internal(format!(
            "Failed to move the new version into {}: {err}{}",
            dest_dir.display(),
            match restored {
                Ok(()) => "; the previous version was restored".to_string(),
                Err(restore) => format!(
                    "; restoring the previous version also failed ({restore}), it is kept in {}",
                    parking.keep().join("previous").display()
                ),
            }
        )));
    }
    Ok(())
}

/// Whether two paths name the same existing directory.
fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Total size of the regular files below `dir`, ignoring `.git`; symlinks are not followed.
fn tree_size(dir: &Path) -> Result<u64, ApiError> {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(entry.path());
                }
            } else if file_type.is_file() {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// Recursively copies a plugin folder, skipping development and cache directories.
///
/// Hidden entries (`.git`, `.venv`, editor files), `node_modules`, `target` and Python caches are
/// environment, not plugin: they are rebuilt in place with the language's own tools. Symlinks are
/// not copied (following one could pull a file from anywhere on the node into the plugin); each
/// one is logged so a plugin that relied on one is easy to diagnose.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "venv"
            || name == "__pycache__"
        {
            continue;
        }

        let src_child = entry.path();
        let dst_child = dst.join(&file_name);
        if file_type.is_dir() {
            copy_dir_recursive(&src_child, &dst_child)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_child, &dst_child)?;
        } else {
            tracing::warn!(
                path = %src_child.display(),
                "Skipping a symlink or special file while installing a plugin"
            );
        }
    }
    Ok(())
}

/// Copies the entrypoint when [`copy_dir_recursive`] left it out.
///
/// A Rust plugin's binary lives under `target/`, which the copy skips as build output; the binary
/// is the plugin itself, though, so it is copied on its own. Like the folder copy, it never
/// follows a symlink. The path was checked to stay inside the plugin folder by
/// [`validate_manifest`].
fn copy_entrypoint(src: &Path, dst: &Path, entrypoint: &str) -> Result<(), std::io::Error> {
    let (from, to) = (src.join(entrypoint), dst.join(entrypoint));
    let is_file = std::fs::symlink_metadata(&from).is_ok_and(|metadata| metadata.is_file());
    if is_file && !to.exists() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&from, &to)?;
    }
    Ok(())
}

/// Sets the executable bits on the declared entrypoint, which a ZIP or a copy may have lost.
fn ensure_entrypoint_executable(plugin_dir: &Path, entrypoint: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exec_path = plugin_dir.join(entrypoint);
        if let Ok(metadata) = std::fs::metadata(&exec_path)
            && metadata.is_file()
        {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o755);
            if let Err(err) = std::fs::set_permissions(&exec_path, permissions) {
                tracing::warn!(
                    path = %exec_path.display(),
                    error = %err,
                    "Could not mark the plugin entrypoint executable"
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (plugin_dir, entrypoint);
    }
}
