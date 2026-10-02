//! Tests for plugin directory scanning, manifest discovery, and priority sorting.

use std::fs;

use kanon_core::manifest::{PluginManifest, PluginScanner, PluginSection};
use kanon_core::supervisor::Supervisor;

#[test]
fn scanner_returns_empty_on_missing_or_empty_dir() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let missing_path = temp_dir.path().join("does_not_exist");

    let res = PluginScanner::scan(&missing_path).expect("scan missing");
    assert!(res.is_empty());

    let empty_path = temp_dir.path().join("empty_plugins");
    fs::create_dir_all(&empty_path).expect("create empty");
    let res = PluginScanner::scan(&empty_path).expect("scan empty");
    assert!(res.is_empty());
}

#[test]
fn scanner_discovers_and_sorts_by_priority() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let base = temp_dir.path();

    // Plugin A: priority 600
    let dir_a = base.join("plugin_a");
    fs::create_dir_all(&dir_a).expect("create dir a");
    fs::write(
        dir_a.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.a"
name = "Plugin A"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin_a"
priority = 600
"#,
    )
    .expect("write a");

    // Plugin B: priority 100
    let dir_b = base.join("plugin_b");
    fs::create_dir_all(&dir_b).expect("create dir b");
    fs::write(
        dir_b.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.b"
name = "Plugin B"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin_b"
priority = 100
"#,
    )
    .expect("write b");

    // Plugin C: default priority (500)
    let dir_c = base.join("plugin_c");
    fs::create_dir_all(&dir_c).expect("create dir c");
    fs::write(
        dir_c.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.c"
name = "Plugin C"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin_c"
"#,
    )
    .expect("write c");

    let discovered = PluginScanner::scan(base).expect("scan");
    assert_eq!(discovered.len(), 3);

    // B (100) -> C (500) -> A (600)
    assert_eq!(discovered[0].manifest.plugin.id, "org.kanon.test.b");
    assert_eq!(discovered[1].manifest.plugin.id, "org.kanon.test.c");
    assert_eq!(discovered[2].manifest.plugin.id, "org.kanon.test.a");
}

#[test]
fn scanner_skips_invalid_manifests_gracefully() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let base = temp_dir.path();

    // Valid plugin
    let valid_dir = base.join("valid");
    fs::create_dir_all(&valid_dir).expect("create valid");
    fs::write(
        valid_dir.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.valid"
name = "Valid Plugin"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin_valid"
"#,
    )
    .expect("write valid");

    // Broken plugin (invalid TOML syntax)
    let broken_dir = base.join("broken");
    fs::create_dir_all(&broken_dir).expect("create broken");
    fs::write(
        broken_dir.join("plugin.toml"),
        "this is not valid toml = [[ broken",
    )
    .expect("write broken");

    let discovered = PluginScanner::scan(base).expect("scan");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].manifest.plugin.id, "org.kanon.test.valid");
}

#[test]
fn scanner_ignores_hidden_and_build_dirs() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let base = temp_dir.path();

    let target_dir = base.join("target").join("dummy");
    fs::create_dir_all(&target_dir).expect("create target");
    fs::write(
        target_dir.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.target"
name = "Ignored Target"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin"
"#,
    )
    .expect("write target");

    let hidden_dir = base.join(".git").join("dummy");
    fs::create_dir_all(&hidden_dir).expect("create hidden");
    fs::write(
        hidden_dir.join("plugin.toml"),
        r#"
[plugin]
id = "org.kanon.test.hidden"
name = "Ignored Hidden"
version = "1.0.0"
runtime = "rust"
entrypoint = "bin"
"#,
    )
    .expect("write hidden");

    let discovered = PluginScanner::scan(base).expect("scan");
    assert_eq!(discovered.len(), 0);
}

#[tokio::test]
async fn supervisor_tracks_unavailable_plugins() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let supervisor = Supervisor::new(Some(temp_dir.path().to_path_buf()), None);

    let manifest_path = temp_dir.path().join("plugin.toml");
    let manifest_content = r#"
[plugin]
id = "org.kanon.test.py"
name = "Python Plugin"
version = "1.0.0"
runtime = "python"
entrypoint = "main.py"
"#;
    fs::write(&manifest_path, manifest_content).expect("write manifest");

    let manifest = PluginManifest::load_from_file(&manifest_path).expect("parse manifest");

    supervisor
        .record_unavailable_plugin(
            manifest,
            manifest_path.clone(),
            "Python 3 not found".to_string(),
        )
        .await;

    let unavail = supervisor.get_unavailable_plugins().await;
    assert_eq!(unavail.len(), 1);
    assert_eq!(unavail[0].manifest.plugin.id, "org.kanon.test.py");
    assert_eq!(unavail[0].status, "RuntimeUnavailable");
    assert_eq!(unavail[0].reason, "Python 3 not found");

    supervisor
        .remove_unavailable_plugin("org.kanon.test.py")
        .await;
    let empty = supervisor.get_unavailable_plugins().await;
    assert!(empty.is_empty());
}

#[tokio::test]
async fn a_plugin_built_for_another_node_version_is_refused_with_the_reason() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let supervisor = Supervisor::new(Some(temp_dir.path().to_path_buf()), None);
    let manifest_path = temp_dir.path().join("plugin.toml");
    fs::write(
        &manifest_path,
        r#"
[plugin]
id = "future"
name = "Future Plugin"
version = "1.0.0"
runtime = "rust"
entrypoint = "missing-binary"
kanon_version = ">=99"
"#,
    )
    .expect("write manifest");

    let err = supervisor
        .spawn_from_manifest(&manifest_path, None)
        .await
        .expect_err("refused before launch");
    assert!(err.to_string().contains("requires Kanon >=99"), "{err}");
    let unavailable = supervisor.get_unavailable_plugins().await;
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].reason.contains(kanon_core::KANON_VERSION));
}

#[test]
fn kanon_version_requirements_are_semver() {
    let section = |requirement: Option<&str>| PluginSection {
        id: "p".to_string(),
        name: "P".to_string(),
        version: "1.0.0".to_string(),
        author: None,
        description: None,
        runtime: "rust".to_string(),
        entrypoint: "p".to_string(),
        isolated: None,
        priority: None,
        kanon_version: requirement.map(str::to_string),
        platforms: Vec::new(),
        homepage: None,
        repository: None,
    };
    let current = semver::Version::parse(kanon_core::KANON_VERSION).unwrap();
    let range = format!(
        ">={}.{}, <{}",
        current.major,
        current.minor,
        current.major + 1
    );
    assert!(kanon_core::check_kanon_version(&section(None)).is_ok());
    assert!(kanon_core::check_kanon_version(&section(Some(&range))).is_ok());
    assert!(kanon_core::check_kanon_version(&section(Some("<0.0.1"))).is_err());
    assert!(
        kanon_core::check_kanon_version(&section(Some("latest please"))).is_err(),
        "a malformed requirement is not ignored"
    );
}
