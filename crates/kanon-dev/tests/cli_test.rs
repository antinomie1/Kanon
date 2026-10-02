//! Integration and end-to-end tests for the `kanon-dev` CLI toolchain.
//!
//! Validates:
//! 1. Scaffolding generation for Rust, Python, and TypeScript (`kanon-dev plugin create`)
//! 2. Static manifest and schema validation (`kanon-dev lint`)
//! 3. `.kpk` bundle distribution packager and SHA-256 integrity verification (`kanon-dev pack`)
//! 4. Offline sandbox host execution for slash commands and tool calling (`kanon-dev test`)
//! 5. The sandbox as a node: scripted messages through the pipeline, KV and the mock model

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

/// An adapter written before media capabilities existed still lints, but is told that tool media
/// no longer reaches its platform until it declares what it delivers.
#[test]
fn test_plugin_lint_warns_adapter_without_media_capabilities() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("main.py"), "").unwrap();
    let manifest = |capabilities: &str| {
        format!(
            "[plugin]\nid = \"com.example.bridge\"\nname = \"Bridge\"\nversion = \"1.0.0\"\n\
             runtime = \"python\"\nentrypoint = \"main.py\"\n\n\
             [adapter]\nplatform = \"bridge\"\ncapabilities = [{capabilities}]\n"
        )
    };
    let manifest_path = tmp.path().join("plugin.toml");

    std::fs::write(&manifest_path, manifest("\"quote_reply\"")).unwrap();
    let report = lint_plugin(&manifest_path).expect("Lint run must parse");
    assert!(report.is_valid(), "{:?}", report.errors);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("declares no send_image")),
        "{:?}",
        report.warnings
    );

    std::fs::write(&manifest_path, manifest("\"quote_reply\", \"send_image\"")).unwrap();
    let report = lint_plugin(&manifest_path).expect("Lint run must parse");
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("send_image")),
        "{:?}",
        report.warnings
    );
}

/// Names of the entries in a `.kpk` archive.
fn archive_names(bundle: &std::path::Path) -> Vec<String> {
    let mut archive = ZipArchive::new(File::open(bundle).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect()
}

#[test]
fn test_plugin_pack_bundle_and_sha256() {
    let tmp = tempdir().expect("tempdir");
    let plugin_dir = tmp.path().join("pack_test_plugin");
    create_plugin_project("pack_test", "python", Some(&plugin_dir)).unwrap();

    // What the node needs besides the scripts: the lockfile and the console pages.
    std::fs::write(plugin_dir.join("uv.lock"), "version = 1\n").unwrap();
    std::fs::create_dir_all(plugin_dir.join("pages")).unwrap();
    std::fs::write(plugin_dir.join("pages/index.html"), "<p>hi</p>").unwrap();
    // What it does not: caches, tests, notes and documentation.
    std::fs::create_dir_all(plugin_dir.join("__pycache__")).unwrap();
    std::fs::write(plugin_dir.join("__pycache__/main.pyc"), b"cache").unwrap();
    std::fs::create_dir_all(plugin_dir.join("tests")).unwrap();
    std::fs::write(plugin_dir.join("tests/test_main.py"), "").unwrap();
    std::fs::write(plugin_dir.join("notes.txt"), "todo").unwrap();

    let out_dir = tmp.path().join("dist");
    let pack_report = pack_plugin(&plugin_dir, Some(&out_dir)).expect("Packaging must succeed");

    assert_eq!(pack_report.sha256_hex.len(), 64);
    let checksum_content = std::fs::read_to_string(&pack_report.checksum_path).unwrap();
    assert_eq!(
        checksum_content,
        format!(
            "{}  org.kanon.plugin.pack_test.kpk\n",
            pack_report.sha256_hex
        )
    );

    // Exactly the manifest, the scripts, the dependency declaration and the pages; README.md
    // from the scaffold stays out too.
    let expected = [
        "main.py",
        "pages/index.html",
        "plugin.toml",
        "pyproject.toml",
        "uv.lock",
    ];
    let mut names = archive_names(&pack_report.bundle_path);
    names.sort();
    assert_eq!(names, expected);
    assert_eq!(pack_report.files, expected);
}

#[test]
fn test_plugin_pack_rust_ships_release_binary_at_entrypoint() {
    let tmp = tempdir().expect("tempdir");
    let plugin_dir = tmp.path().join("bin_plugin");
    std::fs::create_dir_all(plugin_dir.join("src")).unwrap();
    // A dependency-free crate keeps the build to a second; `[workspace]` keeps it standalone.
    std::fs::write(
        plugin_dir.join("Cargo.toml"),
        "[package]\nname = \"bin_plugin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(plugin_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"[plugin]
id = "org.kanon.test.bin_plugin"
name = "Binary Plugin"
version = "0.1.0"
runtime = "rust"
entrypoint = "target/debug/bin_plugin"
"#,
    )
    .unwrap();

    let report = pack_plugin(&plugin_dir, Some(&tmp.path().join("dist"))).expect("pack");

    // The manifest and the binary at the path the manifest launches; no sources.
    assert_eq!(report.files, ["plugin.toml", "target/debug/bin_plugin"]);
    assert!(
        plugin_dir.join("target/release/bin_plugin").is_file(),
        "the packaged binary is the release build"
    );
    let mut archive = ZipArchive::new(File::open(&report.bundle_path).unwrap()).unwrap();
    let binary = archive.by_name("target/debug/bin_plugin").unwrap();
    assert_eq!(binary.unix_mode().unwrap() & 0o755, 0o755);
}

#[tokio::test]
async fn test_sandbox_offline_non_interactive_and_command() {
    let root = find_workspace_root();
    let manifest_path = root.join("sdks/python/plugins/demo_py_plugin/plugin.toml");

    // 1. Non-interactive probe test
    let probe_opts = SandboxOptions {
        non_interactive: true,
        ..SandboxOptions::default()
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
        args: vec!["100 + 200".to_string()],
        ..SandboxOptions::default()
    };
    let cmd_res = run_sandbox(&manifest_path, cmd_opts).await;
    assert!(
        cmd_res.is_ok(),
        "Sandbox command execution failed: {:?}",
        cmd_res.err()
    );

    // 3. Direct tool call execution test
    let tool_opts = SandboxOptions {
        tool: Some("py_calc".to_string()),
        args: vec![r#"{"expr": "40 + 2"}"#.to_string()],
        ..SandboxOptions::default()
    };
    let tool_res = run_sandbox(&manifest_path, tool_opts).await;
    assert!(
        tool_res.is_ok(),
        "Sandbox tool call execution failed: {:?}",
        tool_res.err()
    );
}

/// Scripted messages run through the sandbox's real pipeline in order: commands reach the KV
/// store, the mock model calls a plugin tool on request and reports its result, and a command no
/// plugin declares makes `-c` fail.
#[test]
fn test_sandbox_scripted_conversation() {
    let root = find_workspace_root();
    let plugin = root.join("sdks/python/plugins/demo_py_plugin");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kanon-dev"))
        .arg("test")
        .arg(&plugin)
        .args(["-m", "/note add milk"])
        .args(["-m", "/note list"])
        .args(["-m", r#"!tool py_calc {"expr": "6*7"}"#])
        .args(["-m", "hello"])
        .output()
        .expect("kanon-dev runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");

    // Each reply is printed before the next message is sent.
    let mut rest = stdout.as_ref();
    for expected in [
        "you> /note add milk",
        "bot> Saved note #1.",
        "you> /note list",
        "bot> 1. milk",
        "bot> (mock model) the tool returned: {\"result\":42",
        "you> hello",
        "bot> (mock model) received:",
    ] {
        let at = rest
            .find(expected)
            .unwrap_or_else(|| panic!("'{expected}' missing or out of order in:\n{stdout}"));
        rest = &rest[at + expected.len()..];
    }

    let unknown = std::process::Command::new(env!("CARGO_BIN_EXE_kanon-dev"))
        .arg("test")
        .arg(&plugin)
        .args(["-c", "nope"])
        .output()
        .expect("kanon-dev runs");
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("'/nope' is not declared"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}
