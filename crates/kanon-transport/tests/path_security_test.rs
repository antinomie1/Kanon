//! Security and permission enforcement integration tests for Kanon transport paths.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use kanon_transport::path::{
    core_socket_path, default_run_dir, ensure_parent_dir, ensure_run_dir, host_socket_path,
};

static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard to safely set or remove an environment variable and guarantee restoration on drop.
struct EnvVarGuard {
    var_name: &'static str,
    original_value: Option<String>,
}

impl EnvVarGuard {
    fn set(var_name: &'static str, val: &str) -> Self {
        let original_value = std::env::var(var_name).ok();
        unsafe {
            std::env::set_var(var_name, val);
        }
        Self {
            var_name,
            original_value,
        }
    }

    fn remove(var_name: &'static str) -> Self {
        let original_value = std::env::var(var_name).ok();
        unsafe {
            std::env::remove_var(var_name);
        }
        Self {
            var_name,
            original_value,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(ref val) = self.original_value {
                std::env::set_var(self.var_name, val);
            } else {
                std::env::remove_var(self.var_name);
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn default_run_dir_falls_back_to_uid_isolated_tmp_when_xdg_empty() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::remove("XDG_RUNTIME_DIR");

    let dir = default_run_dir();
    let uid = unsafe { libc::getuid() };
    assert_eq!(dir, PathBuf::from(format!("/tmp/kanon-run-{uid}")));
}

#[test]
#[cfg(unix)]
fn default_run_dir_uses_xdg_runtime_dir_when_present() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let xdg = "/tmp/fake-xdg-runtime-1000";
    let _env = EnvVarGuard::set("XDG_RUNTIME_DIR", xdg);

    let dir = default_run_dir();
    assert_eq!(dir, PathBuf::from(xdg).join("kanon").join("run"));
}

#[test]
fn socket_paths_append_expected_file_names() {
    let base = PathBuf::from("/tmp/custom-run");
    assert_eq!(
        core_socket_path(Some(&base)),
        PathBuf::from("/tmp/custom-run/core.sock")
    );
    assert_eq!(
        host_socket_path("alpha", Some(&base)),
        PathBuf::from("/tmp/custom-run/host_alpha.sock")
    );
}

#[test]
#[cfg(unix)]
fn ensure_run_dir_creates_with_0700_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("sub_run_dir");

    assert!(!target.exists());
    ensure_run_dir(&target).unwrap();

    assert!(target.is_dir());
    let meta = std::fs::metadata(&target).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "Directory permissions must be strictly 0700 (rwx------)"
    );
}

#[test]
#[cfg(unix)]
fn ensure_run_dir_tightens_permissive_preexisting_directory_to_0700() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("preexisting_permissive");

    std::fs::create_dir_all(&target).unwrap();
    // Intentionally create with insecure 0777 permissions
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).unwrap();
    let initial_mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(initial_mode, 0o777);

    // ensure_run_dir must tighten it back to 0700
    ensure_run_dir(&target).unwrap();
    let final_mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        final_mode, 0o700,
        "Pre-existing directory permissions must be clamped to 0700"
    );
}

#[test]
#[cfg(unix)]
fn ensure_run_dir_rejects_symlink_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let real_dir = tmp.path().join("real_target");
    std::fs::create_dir_all(&real_dir).unwrap();

    let symlink_path = tmp.path().join("symlinked_dir");
    std::os::unix::fs::symlink(&real_dir, &symlink_path).unwrap();

    let err = ensure_run_dir(&symlink_path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("symlink"));
}

#[test]
#[cfg(unix)]
fn ensure_parent_dir_ensures_parent_has_secure_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("secure_run").join("core.sock");

    ensure_parent_dir(&file_path).unwrap();
    let parent = file_path.parent().unwrap();
    assert!(parent.is_dir());
    let mode = std::fs::metadata(parent).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}
