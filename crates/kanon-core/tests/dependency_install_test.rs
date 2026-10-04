//! Installing plugin dependencies before launch, with stand-in tools so no network is needed.
//!
//! The stand-ins are shell scripts that record how they were called (in `calls.log` in the
//! plugin directory, their working directory) and create the environment the real tool would.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kanon_core::DependencyInstaller;

/// Tests write and then execute scripts; a script being written while another test forks can
/// fail to execute (`ETXTBSY`), so the tests take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Writes an executable stand-in tool into `bin`.
fn tool(bin: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `uv` that records its arguments and creates the environment where it is told to.
///
/// Like the real one it would resolve a relative `UV_PROJECT_ENVIRONMENT` against the project
/// rather than the working directory, so it refuses one outright.
fn fake_uv(bin: &Path) {
    tool(
        bin,
        "uv",
        r#"echo "uv $*" >> calls.log
case "$UV_PROJECT_ENVIRONMENT" in
  /*) ;;
  *) echo "relative environment: $UV_PROJECT_ENVIRONMENT" >&2; exit 3 ;;
esac
mkdir -p "$UV_PROJECT_ENVIRONMENT/bin"
ln -sf /bin/sh "$UV_PROJECT_ENVIRONMENT/bin/python""#,
    );
}

fn calls(plugin: &Path) -> Vec<String> {
    std::fs::read_to_string(plugin.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn python_plugin(root: &Path) -> PathBuf {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("pyproject.toml"), "[project]\nname = \"p\"\n").unwrap();
    std::fs::write(plugin.join("uv.lock"), "version = 1\n").unwrap();
    plugin
}

#[tokio::test]
async fn a_python_environment_is_created_once_and_refreshed_when_the_project_changes() {
    let _turn = SERIAL.lock().await;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fake_uv(&bin);
    let plugin = python_plugin(root.path());
    let installer = DependencyInstaller::new().with_search_path(vec![bin]);

    let python = installer.prepare_python(&plugin).await.expect("installed");
    assert_eq!(python, plugin.join(".venv/bin/python"));
    assert_eq!(
        calls(&plugin),
        ["uv sync --locked"],
        "the lockfile is honoured"
    );

    installer.prepare_python(&plugin).await.expect("up to date");
    assert_eq!(
        calls(&plugin).len(),
        1,
        "an up-to-date environment is left alone"
    );

    // Editing the project (a new dependency) installs again at the next launch.
    std::fs::File::options()
        .write(true)
        .open(plugin.join("pyproject.toml"))
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    installer.prepare_python(&plugin).await.expect("refreshed");
    assert_eq!(calls(&plugin).len(), 2);
}

/// The node addresses plugins through its relative `./plugins`; the environment must still be
/// created inside the plugin folder.
#[tokio::test]
async fn a_relative_plugin_directory_gets_its_environment_inside_it() {
    let _turn = SERIAL.lock().await;
    // Relative to the test's working directory, as `./plugins/<plugin>` is to the node's.
    let dir = tempfile::tempdir_in(".").unwrap();
    let root = Path::new(".").join(dir.path().file_name().unwrap());
    let bin = dir.path().join("bin");
    fake_uv(&bin);
    let plugin = python_plugin(&root);
    let installer = DependencyInstaller::new().with_search_path(vec![bin]);

    let python = installer.prepare_python(&plugin).await.expect("installed");
    assert_eq!(python, plugin.join(".venv/bin/python"));
    assert!(python.is_file());
}

#[tokio::test]
async fn a_failed_install_reports_the_end_of_the_tools_output_and_is_retried() {
    let _turn = SERIAL.lock().await;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    tool(
        &bin,
        "uv",
        r#"echo "uv $*" >> calls.log
(head -c 1048576 /dev/zero | tr '\000' o; echo stdout-diagnostic) &
head -c 1048576 /dev/zero | tr '\000' e >&2
for i in $(seq 1 30); do echo "resolver line $i" >&2; done
wait
exit 3"#,
    );
    let plugin = python_plugin(root.path());
    let installer = DependencyInstaller::new()
        .with_search_path(vec![bin.clone()])
        .with_timeout(Duration::from_secs(10));

    let err = installer.prepare_python(&plugin).await.unwrap_err();
    assert!(err.contains("`uv sync --locked` failed"), "{err}");
    assert!(err.contains("resolver line 30"), "{err}");
    assert!(
        !err.contains("stdout-diagnostic"),
        "stderr takes precedence: {err}"
    );
    assert!(
        !err.contains("resolver line 5\n"),
        "only the tail is kept: {err}"
    );

    installer.prepare_python(&plugin).await.unwrap_err();
    assert_eq!(
        calls(&plugin).len(),
        2,
        "a failure is not remembered as installed"
    );

    // A line-count limit alone cannot bound this single long stdout line; keep its final bytes.
    tool(
        &bin,
        "uv",
        r#"printf discarded-prefix
head -c 1048576 /dev/zero | tr '\000' x
printf final-stdout-diagnostic
exit 4"#,
    );
    let err = installer.prepare_python(&plugin).await.unwrap_err();
    assert!(err.contains("`uv sync --locked` failed"));
    assert!(err.ends_with("final-stdout-diagnostic"));
    assert!(!err.contains("discarded-prefix"));
    assert!(err.len() <= 64 * 1024 + 200, "retained {} bytes", err.len());
}

#[tokio::test]
async fn without_the_tool_an_existing_environment_is_used_and_a_missing_one_reported() {
    let _turn = SERIAL.lock().await;
    let root = tempfile::tempdir().unwrap();
    let plugin = python_plugin(root.path());
    let installer = DependencyInstaller::new().with_search_path(vec![root.path().join("empty")]);

    let err = installer.prepare_python(&plugin).await.unwrap_err();
    assert!(err.contains("`uv` is not installed"), "{err}");

    // An environment the developer built by other means is still the plugin's own.
    std::fs::create_dir_all(plugin.join(".venv/bin")).unwrap();
    std::fs::write(plugin.join(".venv/bin/python"), "").unwrap();
    installer
        .prepare_python(&plugin)
        .await
        .expect("existing environment used");
}

#[tokio::test]
async fn node_dependencies_are_installed_by_the_tool_that_wrote_the_lockfile() {
    let _turn = SERIAL.lock().await;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    for name in ["npm", "bun"] {
        tool(
            &bin,
            name,
            &format!("echo \"{name} $*\" >> calls.log\nmkdir -p node_modules"),
        );
    }
    let installer = DependencyInstaller::new().with_search_path(vec![bin]);

    let plugin = root.path().join("npm-plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("package.json"),
        r#"{"dependencies":{"left-pad":"1.3.0"}}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("package-lock.json"), "{}").unwrap();
    installer.prepare_node(&plugin).await.expect("installed");
    installer.prepare_node(&plugin).await.expect("up to date");
    assert_eq!(calls(&plugin), ["npm ci"]);

    // Nothing to install, nothing run.
    let plain = root.path().join("plain-plugin");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("package.json"), r#"{"name":"plain"}"#).unwrap();
    installer
        .prepare_node(&plain)
        .await
        .expect("no dependencies");
    assert!(calls(&plain).is_empty());
}

/// Prevents a failing regression from leaving its simulated lifecycle script running.
struct InstallerChildren {
    parent: i32,
    descendant: i32,
}

impl Drop for InstallerChildren {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.parent, libc::SIGKILL);
            libc::kill(self.parent, libc::SIGKILL);
            libc::kill(self.descendant, libc::SIGKILL);
        }
    }
}

#[tokio::test]
async fn timed_out_and_cancelled_installs_stop_their_lifecycle_children() {
    let _turn = SERIAL.lock().await;
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        tool(
            &bin,
            "uv",
            r#"
echo $$ > installer.pid
sh -c 'sleep 1; echo survived > unexpected-write' &
echo $! > descendant.pid
wait
"#,
        );
        let plugin = python_plugin(root.path());
        let installer = DependencyInstaller::new()
            .with_search_path(vec![bin])
            .with_timeout(Duration::from_millis(300));
        let installing_dir = plugin.clone();
        let installing =
            tokio::spawn(async move { installer.prepare_python(&installing_dir).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !plugin.join("descendant.pid").is_file() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let _children = InstallerChildren {
            parent: std::fs::read_to_string(plugin.join("installer.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
            descendant: std::fs::read_to_string(plugin.join("descendant.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        };
        if cancel {
            installing.abort();
            assert!(installing.await.unwrap_err().is_cancelled());
        } else {
            let err = installing.await.unwrap().unwrap_err();
            assert!(err.contains("did not finish"), "{err}");
        }
        // The old behavior killed uv/npm but left its lifecycle script writing into the same
        // directory after the next installer was allowed to acquire that directory's lock.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            !plugin.join("unexpected-write").exists(),
            "installer child survived (cancel={cancel})"
        );
    }
}
