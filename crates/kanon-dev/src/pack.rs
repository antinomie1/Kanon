//! Plugin distribution packager generating `.kpk` bundles.
//!
//! A package carries only what the node needs to run the plugin:
//!
//! - `plugin.toml`, the manifest;
//! - the code: a Rust plugin's release binary (built here and stored at its `entrypoint`), or a
//!   Python / TypeScript plugin's scripts together with the dependency declaration and lockfile
//!   from which the node installs the plugin's environment;
//! - `pages/` and `i18n/`, which the node serves to the console.
//!
//! Everything else (the sources of a compiled plugin, tests, documentation, editor and build
//! state) stays out. The bundle's SHA-256 is saved next to it in `<bundle>.kpk.sha256`.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use kanon_core::PluginManifest;
use thiserror::Error;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::build::{BuildError, build_release};
use crate::lint::{LintError, LintReport, lint_plugin};

/// Folders the node reads from a plugin besides its code, packaged whole.
const ASSET_DIRS: [&str; 2] = ["pages", "i18n"];

/// Folders never packaged: environments and caches the node rebuilds, build output and tests.
/// Hidden entries (`.git`, `.venv`, editor state) are skipped as well.
const SKIPPED_DIRS: [&str; 5] = ["target", "node_modules", "__pycache__", "venv", "tests"];

/// Errors occurring during plugin bundle packaging.
#[derive(Debug, Error)]
pub enum PackError {
    /// File I/O failure while reading files or creating archives.
    #[error("I/O error during packaging: {0}")]
    Io(#[from] std::io::Error),
    /// Manifest lint validation failed.
    #[error(
        "Plugin manifest validation failed with {0} error(s). Run 'kanon-dev lint' for details."
    )]
    ValidationFailed(usize),
    /// Underlying linting failure.
    #[error("Linting failure: {0}")]
    Lint(#[from] LintError),
    /// The manifest passed lint but could not be loaded.
    #[error("invalid plugin.toml: {0}")]
    Manifest(String),
    /// The release build of a Rust plugin failed.
    #[error("{0}")]
    Build(#[from] BuildError),
    /// ZIP compression failure.
    #[error("ZIP packaging error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// Detailed outcome of a successful plugin packaging operation.
#[derive(Debug, Clone)]
pub struct PackReport {
    /// Path to the generated `.kpk` bundle file.
    pub bundle_path: PathBuf,
    /// Path to the accompanying `.sha256` checksum file.
    pub checksum_path: PathBuf,
    /// Computed hex-encoded SHA-256 digest string.
    pub sha256_hex: String,
    /// Paths inside the archive, sorted.
    pub files: Vec<String>,
    /// Size of the generated archive in bytes.
    pub bundle_size_bytes: u64,
    /// Associated lint report.
    pub lint_report: LintReport,
}

/// Packages a plugin project into a standardized `.kpk` distribution archive.
///
/// A Rust plugin is built with `cargo build --release` first; its binary goes into the archive at
/// the manifest's `entrypoint`, so the installed plugin launches exactly as the manifest says.
pub fn pack_plugin(plugin_path: &Path, output_dir: Option<&Path>) -> Result<PackReport, PackError> {
    // 1. Run static validation first; refuse to pack invalid manifests
    let lint_report = lint_plugin(plugin_path)?;
    if !lint_report.is_valid() {
        return Err(PackError::ValidationFailed(lint_report.errors.len()));
    }
    let manifest = PluginManifest::load_from_file(&lint_report.manifest_path)
        .map_err(|err| PackError::Manifest(err.to_string()))?;
    let plugin_root = &lint_report.plugin_root;

    // 2. Decide the contents: archive path -> file on disk, sorted so the archive is reproducible
    let entrypoint = archive_path(Path::new(manifest.plugin.entrypoint.trim()));
    let mut contents = Vec::new();
    collect_files(
        plugin_root,
        plugin_root,
        &manifest.plugin.runtime,
        &mut contents,
    )?;
    if manifest.plugin.runtime == "rust" {
        let binary = build_release(plugin_root, &manifest)?;
        contents.push((entrypoint.clone(), binary));
    }
    contents.sort();

    // 3. Resolve destination directory and archive filename
    let out_dir = output_dir.unwrap_or(plugin_root);
    fs::create_dir_all(out_dir)?;
    let archive_name = format!("{}.kpk", manifest.plugin.id);
    let bundle_path = out_dir.join(&archive_name);
    let checksum_path = out_dir.join(format!("{archive_name}.sha256"));

    // 4. Build the ZIP archive
    let mut zip = ZipWriter::new(File::create(&bundle_path)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, source) in &contents {
        // The node may run the entrypoint directly (a Rust binary), so it stays executable.
        let mode = if *name == entrypoint { 0o755 } else { 0o644 };
        zip.start_file(name.as_str(), options.unix_permissions(mode))?;
        std::io::copy(&mut File::open(source)?, &mut zip)?;
    }
    zip.finish()?;

    // 5. Compute the SHA-256 digest of the bundle
    let mut bundle_bytes = Vec::new();
    File::open(&bundle_path)?.read_to_end(&mut bundle_bytes)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bundle_bytes);
    let sha256_hex: String = digest
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    // 6. Write the checksum file in the standard format: `<sha256>  <filename>`
    fs::write(&checksum_path, format!("{sha256_hex}  {archive_name}\n"))?;

    Ok(PackReport {
        bundle_path,
        checksum_path,
        sha256_hex,
        files: contents.into_iter().map(|(name, _)| name).collect(),
        bundle_size_bytes: bundle_bytes.len() as u64,
        lint_report,
    })
}

/// Collects the packaged files below `dir` as (archive path, file on disk).
fn collect_files(
    root: &Path,
    dir: &Path,
    runtime: &str,
    contents: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let name = relative
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                collect_files(root, &path, runtime, contents)?;
            }
        } else if path.is_file() && is_packaged(relative, runtime) {
            contents.push((archive_path(relative), path));
        }
    }
    Ok(())
}

/// Whether a file (relative to the plugin root) is something the node needs.
fn is_packaged(relative: &Path, runtime: &str) -> bool {
    let (dependency_files, script_extensions): (&[&str], &[&str]) = match runtime {
        "python" => (&["pyproject.toml", "uv.lock"], &["py"]),
        "typescript" => (
            &["package.json", "bun.lock", "bun.lockb", "package-lock.json"],
            &["ts", "mts", "cts", "js", "mjs", "cjs"],
        ),
        // A compiled plugin ships its binary, which is added after the build.
        _ => (&[], &[]),
    };
    let in_asset_dir = relative
        .components()
        .next()
        .is_some_and(|first| ASSET_DIRS.iter().any(|dir| first.as_os_str() == *dir));
    let is_script = relative
        .extension()
        .is_some_and(|ext| script_extensions.iter().any(|known| ext == *known));
    relative == Path::new("plugin.toml")
        || in_asset_dir
        || dependency_files
            .iter()
            .any(|name| relative == Path::new(name))
        || is_script
}

/// A relative path as a ZIP entry name: `/`-separated, without `.` components.
fn archive_path(relative: &Path) -> String {
    relative
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
