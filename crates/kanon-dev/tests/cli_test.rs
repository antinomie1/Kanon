//! Integration and end-to-end tests for the `kanon-dev` CLI toolchain.
//!
//! Validates:
//! 1. Scaffolding generation for Rust, Python, and TypeScript (`kanon-dev plugin create`)
//! 2. Static manifest and schema validation (`kanon-dev lint`)
//! 3. `.kpk` bundle distribution packager and SHA-256 integrity verification (`kanon-dev pack`)
//! 4. Offline sandbox host execution for slash commands and tool calling (`kanon-dev test`)

use std::fs::File;
use std::path::PathBuf;
use tempfile::tempdir;
use zip::ZipArchive;

use kanon_dev::{SandboxOptions, create_plugin_project, lint_plugin, pack_plugin, run_sandbox};

fn find_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn test_plugin_scaffold_rust() {
    let tmp = tempdir().expect("tempdir");
    let out_dir = tmp.path().join("my_rust_plugin");

    let result = create_plugin_project("my_rust_plugin", "rust", Some(&out_dir));
    assert!(
        result.is_ok(),
        "Failed to scaffold Rust plugin: {:?}",
        result.err()
    );

    assert!(out_dir.join("plugin.toml").exists());
    assert!(out_dir.join("Cargo.toml").exists());
    assert!(out_dir.join("src/main.rs").exists());
    assert!(out_dir.join("README.md").exists());

    let plugin_toml = std::fs::read_to_string(out_dir.join("plugin.toml")).unwrap();
    assert!(plugin_toml.contains("id = \"org.kanon.plugin.my_rust_plugin\""));
    assert!(plugin_toml.contains("runtime = \"rust\""));
    assert!(plugin_toml.contains("[[commands]]"));
    assert!(plugin_toml.contains("[[tools]]"));

    let cargo_toml = std::fs::read_to_string(out_dir.join("Cargo.toml")).unwrap();
    assert!(cargo_toml.contains("name = \"my_rust_plugin\""));
    assert!(cargo_toml.contains("edition = \"2024\""));
}

#[test]
fn test_plugin_scaffold_python() {
    let tmp = tempdir().expect("tempdir");
    let out_dir = tmp.path().join("my_py_plugin");

    let result = create_plugin_project("my_py_plugin", "python", Some(&out_dir));
    assert!(
        result.is_ok(),
        "Failed to scaffold Python plugin: {:?}",
        result.err()
    );

    assert!(out_dir.join("plugin.toml").exists());
    assert!(out_dir.join("pyproject.toml").exists());
    assert!(out_dir.join("main.py").exists());
    assert!(out_dir.join("README.md").exists());

    let plugin_toml = std::fs::read_to_string(out_dir.join("plugin.toml")).unwrap();
    assert!(plugin_toml.contains("id = \"org.kanon.plugin.my_py_plugin\""));
    assert!(plugin_toml.contains("runtime = \"python\""));

    let main_py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
    assert!(main_py.contains("class MyPyPluginPlugin(Plugin):"));
    assert!(main_py.contains("@command"));
    assert!(main_py.contains("@tool"));
}

/// Inside a Kanon checkout the scaffold points the SDK dependency at that checkout, relative to
/// the plugin, because the SDKs are not published to a registry.
#[test]
fn test_plugin_scaffold_links_sdk_checkout() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("sdks/python")).unwrap();
    std::fs::create_dir_all(tmp.path().join("sdks/typescript")).unwrap();
    std::fs::write(tmp.path().join("sdks/python/pyproject.toml"), "").unwrap();
    std::fs::write(tmp.path().join("sdks/typescript/package.json"), "{}").unwrap();

    let py_dir = tmp.path().join("plugins/linked_py");
    create_plugin_project("linked_py", "python", Some(&py_dir)).unwrap();
    let pyproject = std::fs::read_to_string(py_dir.join("pyproject.toml")).unwrap();
    assert!(pyproject.contains(r#"dependencies = ["kanon-python-host"]"#));
    assert!(
        pyproject
            .contains(r#"kanon-python-host = { path = "../../sdks/python", editable = true }"#)
    );

    let ts_dir = tmp.path().join("plugins/linked_ts");
    create_plugin_project("linked_ts", "ts", Some(&ts_dir)).unwrap();
    let package_json = std::fs::read_to_string(ts_dir.join("package.json")).unwrap();
    assert!(package_json.contains(r#""@kanon/sdk-and-host": "file:../../sdks/typescript""#));
}

#[test]
fn test_plugin_scaffold_typescript() {
    let tmp = tempdir().expect("tempdir");
    let out_dir = tmp.path().join("my_ts_plugin");

    let result = create_plugin_project("my_ts_plugin", "ts", Some(&out_dir));
    assert!(
        result.is_ok(),
        "Failed to scaffold TypeScript plugin: {:?}",
        result.err()
    );

    assert!(out_dir.join("plugin.toml").exists());
    assert!(out_dir.join("package.json").exists());
    assert!(out_dir.join("tsconfig.json").exists());
    assert!(out_dir.join("index.ts").exists());
    assert!(out_dir.join("README.md").exists());

    let plugin_toml = std::fs::read_to_string(out_dir.join("plugin.toml")).unwrap();
    assert!(plugin_toml.contains("id = \"org.kanon.plugin.my_ts_plugin\""));
    assert!(plugin_toml.contains("runtime = \"typescript\""));

    let index_ts = std::fs::read_to_string(out_dir.join("index.ts")).unwrap();
    assert!(index_ts.contains("export default class MyTsPluginPlugin extends Plugin"));
    assert!(index_ts.contains("@Command"));
    assert!(index_ts.contains("@Tool"));
}

#[test]
fn test_plugin_lint_demo_plugins() {
    let root = find_workspace_root();

    // 1. Python demo plugin
    let py_dir = root.join("sdks/python/plugins/demo_py_plugin");
    let py_report = lint_plugin(&py_dir).expect("Failed to lint Python demo plugin");
    assert!(
        py_report.is_valid(),
        "Python demo plugin lint failed: {:?}",
        py_report.errors
    );
    assert_eq!(
        py_report.plugin_id.as_deref(),
        Some("org.kanon.plugin.demo_py")
    );

    // 2. TypeScript demo plugin
    let ts_dir = root.join("sdks/typescript/plugins/demo_ts_plugin");
    let ts_report = lint_plugin(&ts_dir).expect("Failed to lint TypeScript demo plugin");
    assert!(
        ts_report.is_valid(),
        "TypeScript demo plugin lint failed: {:?}",
        ts_report.errors
    );
    assert_eq!(
        ts_report.plugin_id.as_deref(),
        Some("org.kanon.plugin.demo_ts")
    );

    // 3. Rust demo plugin
    let rust_dir = root.join("sdks/rust/plugins/demo_rust_plugin");
    let rust_report = lint_plugin(&rust_dir).expect("Failed to lint Rust demo plugin");
    assert!(
        rust_report.is_valid(),
        "Rust demo plugin lint failed: {:?}",
        rust_report.errors
    );
    assert_eq!(
        rust_report.plugin_id.as_deref(),
        Some("org.kanon.plugin.demo_rust")
    );
}

#[test]
fn test_plugin_lint_catches_invalid_manifest() {
    let tmp = tempdir().expect("tempdir");
    let bad_toml = r#"
[plugin]
id = "INVALID_ID_WITHOUT_DOT"
name = ""
version = "v1-beta"
runtime = "golang"
entrypoint = "non_existent_script.py"
priority = 9999

[[commands]]
name = "/leading_slash"
priority = 0

[[commands]]
name = "duplicate"

[[commands]]
name = "duplicate"

[[tools]]
name = "bad tool space"
parameters = "not an object"
"#;

    let manifest_path = tmp.path().join("plugin.toml");
    std::fs::write(&manifest_path, bad_toml).unwrap();

    let report = lint_plugin(&manifest_path).expect("Lint run must parse");
    assert!(!report.is_valid(), "Expected validation errors");

    // Check specific caught violations
    let err_str = report.errors.join("\n");
    assert!(
        err_str.contains("reverse domain notation"),
        "Must catch bad ID"
    );
    assert!(
        err_str.contains("Plugin 'name' must not be empty"),
        "Must catch empty name"
    );
    assert!(
        err_str.contains("not a valid Semantic Version"),
        "Must catch bad version"
    );
    assert!(
        err_str.contains("Unsupported runtime 'golang'"),
        "Must catch unsupported runtime"
    );
    assert!(
        err_str.contains("Plugin priority 9999 is outside valid range"),
        "Must catch bad priority"
    );
    assert!(
        err_str.contains("must not include a leading slash"),
        "Must catch slash in command"
    );
    assert!(
        err_str.contains("Duplicate command declaration: command 'duplicate'"),
        "Must catch duplicate command"
    );
    assert!(
        err_str.contains("bad tool space"),
        "Must catch space in tool name"
    );
    assert!(
        err_str.contains("parameters schema must be a JSON Schema Object"),
        "Must catch non-object schema"
    );
}

#[test]
fn test_plugin_pack_bundle_and_sha256() {
    let tmp = tempdir().expect("tempdir");
    let plugin_dir = tmp.path().join("pack_test_plugin");

    // Scaffold a valid Python plugin
    create_plugin_project("pack_test", "python", Some(&plugin_dir)).unwrap();

    // Create a dummy file that should be excluded
    let cache_dir = plugin_dir.join("__pycache__");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("test.pyc"), b"dummy cache").unwrap();

    // Pack the plugin into out_dir
    let out_dir = tmp.path().join("dist");
    let pack_report = pack_plugin(&plugin_dir, Some(&out_dir)).expect("Packaging must succeed");

    assert!(pack_report.bundle_path.exists());
    assert!(pack_report.checksum_path.exists());
    assert!(pack_report.bundle_size_bytes > 0);
    assert_eq!(pack_report.sha256_hex.len(), 64); // 256-bit hex = 64 chars

    // Check checksum file content: `<sha256>  <filename>`
    let checksum_content = std::fs::read_to_string(&pack_report.checksum_path).unwrap();
    assert!(checksum_content.starts_with(&pack_report.sha256_hex));
    assert!(checksum_content.contains("org.kanon.plugin.pack_test.kpk"));

    // Verify ZIP archive contents
    let file = File::open(&pack_report.bundle_path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();

    let mut archived_names = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).unwrap();
        archived_names.push(entry.name().to_string());
    }

    assert!(archived_names.contains(&"plugin.toml".to_string()));
    assert!(archived_names.contains(&"pyproject.toml".to_string()));
    assert!(archived_names.contains(&"main.py".to_string()));
    assert!(archived_names.contains(&"README.md".to_string()));

    // Excluded files must NOT be in the archive
    assert!(
        !archived_names.iter().any(|n| n.contains("__pycache__")),
        "ZIP archive must exclude __pycache__ directory"
    );
}

#[tokio::test]
async fn test_sandbox_offline_non_interactive_and_command() {
    let root = find_workspace_root();
    let manifest_path = root.join("sdks/python/plugins/demo_py_plugin/plugin.toml");

    // 1. Non-interactive probe test
    let probe_opts = SandboxOptions {
        command: None,
        tool: None,
        args: vec![],
        non_interactive: true,
    };
    let probe_res = run_sandbox(&manifest_path, probe_opts).await;
    assert!(
        probe_res.is_ok(),
        "Sandbox non-interactive probe failed: {:?}",
        probe_res.err()
    );

    // 2. Direct command execution test
    let cmd_opts = SandboxOptions {
        command: Some("pycalc".to_string()),
        tool: None,
        args: vec!["100 + 200".to_string()],
        non_interactive: false,
    };
    let cmd_res = run_sandbox(&manifest_path, cmd_opts).await;
    assert!(
        cmd_res.is_ok(),
        "Sandbox command execution failed: {:?}",
        cmd_res.err()
    );

    // 3. Direct tool call execution test
    let tool_opts = SandboxOptions {
        command: None,
        tool: Some("py_calc".to_string()),
        args: vec![r#"{"expr": "40 + 2"}"#.to_string()],
        non_interactive: false,
    };
    let tool_res = run_sandbox(&manifest_path, tool_opts).await;
    assert!(
        tool_res.is_ok(),
        "Sandbox tool call execution failed: {:?}",
        tool_res.err()
    );
}
