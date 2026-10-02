//! Building a plugin before it runs.
//!
//! Only Rust plugins have a build step: Python and TypeScript plugins run from source, and their
//! dependencies are installed by the supervisor with the ecosystem's own tool when they start.
//! The build is plain `cargo build` in the plugin folder, so it uses the plugin's own toolchain,
//! lockfile and workspace exactly as the developer's terminal would.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use kanon_core::PluginManifest;
use thiserror::Error;

/// Errors raised while building a plugin.
#[derive(Debug, Error)]
pub enum BuildError {
    /// Cargo could not be started at all.
    #[error("could not run cargo (is the Rust toolchain installed?): {0}")]
    Spawn(std::io::Error),
    /// Cargo ran and reported a failure; its diagnostics were already printed.
    #[error("cargo build failed ({0})")]
    Failed(std::process::ExitStatus),
    /// The build succeeded but produced no binary named like the manifest's entrypoint.
    #[error(
        "cargo built no binary named '{name}' (the manifest's entrypoint); check `entrypoint` in plugin.toml and the [[bin]] or package name in Cargo.toml"
    )]
    NoExecutable {
        /// File name the entrypoint asks for.
        name: String,
    },
}

/// Builds the plugin when its runtime has a build step, returning the executable to launch.
///
/// Returns `None` for runtimes that run from source. The entrypoint decides the profile:
/// `release` anywhere in its path selects `--release`.
pub async fn build_plugin(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<Option<PathBuf>, BuildError> {
    if manifest.plugin.runtime != "rust" {
        return Ok(None);
    }
    let entrypoint = Path::new(&manifest.plugin.entrypoint);
    let release = entrypoint
        .components()
        .any(|component| component.as_os_str() == "release");
    let (root, name) = (root.to_path_buf(), binary_name(entrypoint));
    // Cargo runs for seconds to minutes; it must not hold a runtime worker meanwhile.
    tokio::task::spawn_blocking(move || cargo_build(&root, release, &name))
        .await
        .map_err(|err| BuildError::Spawn(std::io::Error::other(err)))?
        .map(Some)
}

/// Builds the release binary of a Rust plugin, which is what a package ships.
pub fn build_release(root: &Path, manifest: &PluginManifest) -> Result<PathBuf, BuildError> {
    let name = binary_name(Path::new(&manifest.plugin.entrypoint));
    cargo_build(root, true, &name)
}

/// The binary an entrypoint names, without extension so `target/debug/foo` also finds
/// `foo.exe` on Windows.
fn binary_name(entrypoint: &Path) -> String {
    entrypoint
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Runs `cargo build` in `root` and returns the binary called `wanted`.
///
/// The binary's location is taken from cargo's own report rather than from the entrypoint path:
/// a plugin inside a Cargo workspace builds into the workspace's `target/`, not its own.
fn cargo_build(root: &Path, release: bool, wanted: &str) -> Result<PathBuf, BuildError> {
    let mut command = std::process::Command::new("cargo");
    command
        .arg("build")
        // Machine-readable artifacts on stdout; the usual human diagnostics still go to stderr.
        .arg("--message-format=json-render-diagnostics")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if release {
        command.arg("--release");
    }
    let output = command.output().map_err(BuildError::Spawn)?;
    if !output.status.success() {
        return Err(BuildError::Failed(output.status));
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .find(|executable| {
            executable
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == wanted)
        })
        .ok_or_else(|| BuildError::NoExecutable {
            name: wanted.to_string(),
        })
}
