//! Cross-platform runtime directory and IPC socket path resolution.
//!
//! Provides standardized path resolution for Kanon runtime endpoints,
//! including `core.sock` and per-host `host_<id>.sock` endpoints.
//!
//! # Security Guarantees on Unix
//! When running on Linux/macOS, IPC domain sockets must be shielded against unauthorized
//! inspection or symlink hijacking by local multi-tenant users. If `$XDG_RUNTIME_DIR` is
//! unset (common in non-login shells, Docker containers, and minimal Alpine environments),
//! the path derivation algorithm deterministically falls back to `/tmp/kanon-run-$UID/`
//! rather than an unprotected global `/tmp` directory. Furthermore, runtime directories are
//! strictly verified against symlink attacks and enforced with POSIX `0700` permissions.

use std::path::{Path, PathBuf};

/// Default relative directory used for storing runtime sockets on non-Unix platforms.
pub const DEFAULT_RUN_DIR: &str = "./run";

/// Resolves the default runtime directory for Kanon IPC endpoints.
///
/// The node's `startup.run_dir` setting overrides this; the SDKs never call it for a host, which
/// always receives its socket path from the supervisor.
///
/// # Resolution Priority:
/// 1. On Unix:
///    - If `XDG_RUNTIME_DIR` is set and non-empty, use `$XDG_RUNTIME_DIR/kanon/run`.
///    - Otherwise, fallback to `/tmp/kanon-run-$UID/` using `libc::getuid()` to isolate
///      sockets by user ID and prevent local privilege hijacking in containerized environments.
/// 2. On non-Unix: fallback to the local relative directory `./run`.
pub fn default_run_dir() -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR")
            && !xdg_runtime.trim().is_empty()
        {
            return PathBuf::from(xdg_runtime).join("kanon").join("run");
        }

        // Fallback when XDG_RUNTIME_DIR is absent or empty (e.g. Docker, Alpine, bare cloud servers).
        // Using libc::getuid() strictly isolates sockets by effective user ID, defeating symlink attacks
        // and unauthorized socket access from other users sharing /tmp.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/kanon-run-{uid}"))
    }

    #[cfg(not(unix))]
    PathBuf::from(DEFAULT_RUN_DIR)
}

/// Resolves the socket path for the Kanon Core IPC endpoint (`core.sock`).
///
/// If `base_dir` is `None`, the default runtime directory resolved by
/// [`default_run_dir`] will be used.
pub fn core_socket_path(base_dir: Option<&Path>) -> PathBuf {
    match base_dir {
        Some(dir) => dir.join("core.sock"),
        None => default_run_dir().join("core.sock"),
    }
}

/// Resolves the socket path for a dedicated plugin host IPC endpoint (`host_<host_id>.sock`).
///
/// If `base_dir` is `None`, the default runtime directory resolved by
/// [`default_run_dir`] will be used.
pub fn host_socket_path(host_id: &str, base_dir: Option<&Path>) -> PathBuf {
    let filename = format!("host_{host_id}.sock");
    match base_dir {
        Some(dir) => dir.join(filename),
        None => default_run_dir().join(filename),
    }
}

/// Ensures that a runtime directory exists and satisfies strict permission and ownership checks.
///
/// # Security Requirements on Unix:
/// - If the directory does not exist, it is created with `0700` (`rwx------`) permissions.
/// - If the directory already exists:
///   1. Rejects symlinks: if `dir` is a symlink, returns [`std::io::ErrorKind::PermissionDenied`]
///      to stop symlink redirection attacks in shared directories like `/tmp`.
///   2. Rejects invalid file types (e.g. regular files).
///   3. Validates UID: ensures the directory owner matches the current process UID via `libc::getuid()`.
///   4. Clamps permissions: explicitly calls `chmod 0700` to revoke group/other access even if
///      the directory pre-existed with permissive permissions (e.g. `0777`).
pub fn ensure_run_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(dir, permissions)?;
        }
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let meta = std::fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Runtime directory '{}' is a symlink; refusing to use for security",
                    dir.display()
                ),
            ));
        }

        if !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Runtime path '{}' exists but is not a directory",
                    dir.display()
                ),
            ));
        }

        let current_uid = unsafe { libc::getuid() };
        if meta.uid() != current_uid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Runtime directory '{}' is owned by UID {} but expected UID {}",
                    dir.display(),
                    meta.uid(),
                    current_uid
                ),
            ));
        }

        // Enforce restrictive permissions even on pre-existing directories to revoke stale masks.
        let permissions = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, permissions)?;
    }

    Ok(())
}

/// Ensures that the parent directory of a given file path exists with secure permissions.
pub fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_run_dir(parent)?;
    }
    Ok(())
}
