//! Project scaffolding and plugin template generation.
//!
//! Generates standard production-ready plugin projects for Rust, Python, and TypeScript,
//! complete with `plugin.toml` manifest, dependency definitions, and starter implementations.

use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors arising during plugin project scaffolding.
#[derive(Debug, Error)]
pub enum ScaffoldError {
    /// Target directory creation or file writing failure.
    #[error("I/O error during project creation: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid or unsupported programming language.
    #[error(
        "Unsupported plugin language '{0}'. Supported languages: rust, python, typescript (ts)"
    )]
    UnsupportedLanguage(String),
    /// Invalid plugin name.
    #[error(
        "Invalid plugin name '{0}'. Must be a valid identifier containing only alphanumeric characters, underscores, and hyphens"
    )]
    InvalidPluginName(String),
    /// Target directory already exists and is not empty.
    #[error("Target directory '{0}' already exists and is not empty")]
    DirectoryNotEmpty(PathBuf),
}

/// Validates whether a plugin name conforms to standard identifier syntax.
fn validate_plugin_name(name: &str) -> Result<(), ScaffoldError> {
    if name.is_empty() {
        return Err(ScaffoldError::InvalidPluginName(name.to_string()));
    }
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '_' && c != '-' {
            return Err(ScaffoldError::InvalidPluginName(name.to_string()));
        }
    }
    Ok(())
}

/// Converts a name to snake_case for plugin IDs and package identifiers.
fn to_snake_case(s: &str) -> String {
    s.replace('-', "_").to_lowercase()
}

/// Converts a name to PascalCase for class or struct definitions.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' || c == '-' {
            capitalize = true;
        } else if capitalize {
            result.extend(c.to_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Generates a starter plugin project for the given language.
///
/// Returns the path to the initialized project directory.
pub fn create_plugin_project(
    name: &str,
    lang: &str,
    output_dir: Option<&Path>,
) -> Result<PathBuf, ScaffoldError> {
    validate_plugin_name(name)?;

    let target_dir = match output_dir {
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from(name),
    };

    if target_dir.exists() {
        let entries = fs::read_dir(&target_dir)?;
        if entries.count() > 0 {
            return Err(ScaffoldError::DirectoryNotEmpty(target_dir));
        }
    } else {
        fs::create_dir_all(&target_dir)?;
    }

    let normalized_lang = lang.to_lowercase();
    match normalized_lang.as_str() {
        "rust" => scaffold_rust_plugin(name, &target_dir)?,
        "python" | "py" => scaffold_python_plugin(name, &target_dir)?,
        "typescript" | "ts" => scaffold_typescript_plugin(name, &target_dir)?,
        _ => return Err(ScaffoldError::UnsupportedLanguage(lang.to_string())),
    }

    Ok(target_dir)
}

/// Finds a Kanon SDK checkout above `dir` and returns its path relative to `dir`.
///
/// The SDKs are not published to a registry yet, so a plugin created inside a Kanon checkout
/// points its SDK dependency at that checkout. Outside one there is nothing to point at: the
/// dependency is left as a plain registry name and the package manager reports it explicitly.
fn sdk_path(dir: &Path, sdk: &str, marker: &str) -> Option<String> {
    let dir = dir.canonicalize().ok()?;
    dir.ancestors()
        .enumerate()
        .find(|(_, ancestor)| ancestor.join(sdk).join(marker).is_file())
        .map(|(depth, _)| format!("{}{sdk}", "../".repeat(depth)))
}

/// Generates a starter Rust plugin project with `Cargo.toml`, `plugin.toml`, and `src/main.rs`.
fn scaffold_rust_plugin(name: &str, dir: &Path) -> Result<(), ScaffoldError> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let plugin_toml = format!(
        r#"[plugin]
id = "org.kanon.plugin.{snake_name}"
name = "{pascal_name} Plugin"
version = "0.1.0"
author = "Kanon Dev"
description = "Kanon plugin written in Rust"
runtime = "rust"
entrypoint = "target/debug/{snake_name}"
isolated = false
priority = 500

[[commands]]
name = "{snake_name}_echo"
description = "Echoes the message back"
usage = "/{snake_name}_echo <message>"

[[tools]]
name = "{snake_name}_add"
description = "Adds two numbers."
parameters = {{ type = "object", properties = {{ a = {{ type = "number", description = "First number." }}, b = {{ type = "number", description = "Second number." }} }}, required = ["a", "b"] }}
"#
    );

    let sdk_dependency = sdk_path(dir, "sdks/rust/kanon-sdk", "Cargo.toml")
        .map(|path| format!("{{ path = \"{path}\" }}"))
        .unwrap_or_else(|| "\"0.1.0\"".to_string());
    // The empty `[workspace]` makes the plugin its own workspace, so it also builds when created
    // inside another one (such as a Kanon checkout), and `target/` stays next to `plugin.toml`
    // where the entrypoint expects it.
    let cargo_toml = format!(
        r#"[package]
name = "{snake_name}"
version = "0.1.0"
edition = "2024"
description = "Kanon plugin in Rust: {pascal_name}"

[dependencies]
kanon-sdk = {sdk_dependency}
tokio = {{ version = "1.40", features = ["full"] }}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
schemars = "1"

[workspace]
"#
    );

    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir)?;

    let main_rs = format!(
        r#"//! {pascal_name} plugin for Kanon.
//!
//! See docs/PLUGIN_GUIDE.md in the Kanon repository for commands, tools, events and core calls.

use kanon_sdk::prelude::*;

/// Arguments of `{snake_name}_add`. The schema the model sees is generated from this struct;
/// the doc comments describe the parameters.
#[derive(Deserialize, JsonSchema)]
struct AddArgs {{
    /// First number.
    a: f64,
    /// Second number.
    b: f64,
}}

fn plugin() -> Router {{
    Router::new("org.kanon.plugin.{snake_name}", "{pascal_name} Plugin", "0.1.0")
        .author("Kanon Dev")
        .description("Kanon plugin written in Rust")
        .command(
            CommandSpec::new("{snake_name}_echo")
                .description("Echoes the message back")
                .usage("/{snake_name}_echo <message>"),
            |event| async move {{
                // Returning text replies with it; raw_args is the text after the command name.
                let text = match event.raw_args() {{
                    "" => "Say something!",
                    text => text,
                }};
                Ok(format!("[{pascal_name}] {{text}}"))
            }},
        )
        .tool(
            ToolSpec::typed::<AddArgs>("{snake_name}_add").description("Adds two numbers."),
            |args, _event| async move {{
                // Arguments the model got wrong are reported back to it before this runs.
                Ok(args.a + args.b)
            }},
        )
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    KanonHost::new(plugin()).run().await?;
    Ok(())
}}
"#
    );

    let readme_md = format!(
        "# {pascal_name} Plugin (Rust)\n\nA Kanon microkernel plugin built with Rust 2024.\n"
    );

    fs::write(dir.join("plugin.toml"), plugin_toml)?;
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;
    fs::write(src_dir.join("main.rs"), main_rs)?;
    fs::write(dir.join("README.md"), readme_md)?;

    Ok(())
}

/// Generates a starter Python plugin project with `pyproject.toml`, `plugin.toml`, and `main.py`.
fn scaffold_python_plugin(name: &str, dir: &Path) -> Result<(), ScaffoldError> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let plugin_toml = format!(
        r#"[plugin]
id = "org.kanon.plugin.{snake_name}"
name = "{pascal_name} Plugin"
version = "0.1.0"
author = "Kanon Dev"
description = "Kanon plugin written in Python"
runtime = "python"
entrypoint = "main.py"
isolated = false
priority = 500

[[commands]]
name = "{snake_name}_echo"
description = "Echoes the message back"
usage = "/{snake_name}_echo <message>"

[[tools]]
name = "{snake_name}_add"
description = "Adds two numbers."
parameters = {{ type = "object", properties = {{ a = {{ type = "number", description = "First number." }}, b = {{ type = "number", description = "Second number." }} }}, required = ["a", "b"] }}
"#
    );

    // No `[build-system]`: the host loads the plugin from its directory, so uv only has to
    // install the dependencies into `.venv`, never build the plugin itself.
    let sdk_source = sdk_path(dir, "sdks/python", "pyproject.toml")
        .map(|path| {
            format!("\n[tool.uv.sources]\nkanon-python-host = {{ path = \"{path}\", editable = true }}\n")
        })
        .unwrap_or_default();
    let pyproject_toml = format!(
        r#"[project]
name = "{snake_name}"
version = "0.1.0"
description = "Kanon plugin in Python: {pascal_name}"
requires-python = ">=3.10"
dependencies = ["kanon-python-host"]
{sdk_source}"#
    );

    let main_py = format!(
        r#""""{pascal_name} plugin for Kanon.

See docs/PLUGIN_GUIDE.md in the Kanon repository for commands, tools, events and core calls.
"""

from kanon_sdk import CommandEvent, Plugin, PluginContext, command, tool


class {pascal_name}Plugin(Plugin):
    id = "org.kanon.plugin.{snake_name}"
    name = "{pascal_name} Plugin"
    version = "0.1.0"
    author = "Kanon Dev"
    description = "Kanon plugin written in Python"
    priority = 500

    async def on_load(self, ctx: PluginContext) -> None:
        print(f"{pascal_name} Plugin loaded with data dir: {{ctx.data_dir}}", flush=True)

    @command(
        "{snake_name}_echo",
        description="Echoes the message back",
        usage="/{snake_name}_echo <message>",
    )
    async def echo(self, event: CommandEvent) -> str:
        # Returning a string replies with it; raw_args is the text after the command name.
        return f"[{pascal_name}] {{event.raw_args or 'Say something!'}}"

    @tool
    async def {snake_name}_add(self, a: float, b: float) -> float:
        """Adds two numbers.

        Args:
            a: First number.
            b: Second number.
        """
        # The schema the model sees is inferred from the signature and this docstring.
        return a + b
"#
    );

    let readme_md = format!(
        "# {pascal_name} Plugin (Python)\n\nA Kanon microkernel plugin built with Python and `uv`.\n"
    );

    fs::write(dir.join("plugin.toml"), plugin_toml)?;
    fs::write(dir.join("pyproject.toml"), pyproject_toml)?;
    fs::write(dir.join("main.py"), main_py)?;
    fs::write(dir.join("README.md"), readme_md)?;

    Ok(())
}

/// Generates a starter TypeScript plugin project with `package.json`, `tsconfig.json`, `plugin.toml`, and `index.ts`.
fn scaffold_typescript_plugin(name: &str, dir: &Path) -> Result<(), ScaffoldError> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let plugin_toml = format!(
        r#"[plugin]
id = "org.kanon.plugin.{snake_name}"
name = "{pascal_name} Plugin"
version = "0.1.0"
author = "Kanon Dev"
description = "Kanon plugin written in TypeScript"
runtime = "typescript"
entrypoint = "index.ts"
isolated = false
priority = 500

[[commands]]
name = "{snake_name}_echo"
description = "Echoes the message back"
usage = "/{snake_name}_echo <message>"

[[tools]]
name = "{snake_name}_add"
description = "Adds two numbers."
parameters = {{ type = "object", properties = {{ a = {{ type = "number", description = "First number." }}, b = {{ type = "number", description = "Second number." }} }}, required = ["a", "b"] }}
"#
    );

    let sdk_version = sdk_path(dir, "sdks/typescript", "package.json")
        .map(|path| format!("file:{path}"))
        .unwrap_or_else(|| "^0.1.0".to_string());
    let package_json = format!(
        r#"{{
  "name": "{snake_name}",
  "version": "0.1.0",
  "type": "module",
  "description": "Kanon plugin in TypeScript: {pascal_name}",
  "main": "index.ts",
  "scripts": {{
    "build": "tsc"
  }},
  "dependencies": {{
    "@kanon/sdk-and-host": "{sdk_version}"
  }},
  "devDependencies": {{
    "typescript": "^5.0.0"
  }}
}}
"#
    );

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "experimentalDecorators": true,
    "emitDecoratorMetadata": true,
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true
  },
  "include": ["**/*.ts"]
}
"#;

    let index_ts = format!(
        r#"/**
 * {pascal_name} plugin for Kanon.
 *
 * See docs/PLUGIN_GUIDE.md in the Kanon repository for commands, tools, events and core calls.
 */

import {{ Command, CommandEvent, Plugin, PluginContext, Tool, s }} from "@kanon/sdk-and-host";

export default class {pascal_name}Plugin extends Plugin {{
  id = "org.kanon.plugin.{snake_name}";
  name = "{pascal_name} Plugin";
  version = "0.1.0";
  author = "Kanon Dev";
  description = "Kanon plugin written in TypeScript";
  priority = 500;

  async onLoad(ctx: PluginContext): Promise<void> {{
    console.log(`{pascal_name} Plugin loaded with data dir: ${{ctx.dataDir}}`);
  }}

  @Command("{snake_name}_echo", {{
    description: "Echoes the message back",
    usage: "/{snake_name}_echo <message>",
  }})
  async echo(event: CommandEvent): Promise<string> {{
    // Returning a string replies with it; rawArgs is the text after the command name.
    return `[{pascal_name}] ${{event.rawArgs || "Say something!"}}`;
  }}

  @Tool("{snake_name}_add", {{
    description: "Adds two numbers.",
    args: {{ a: s.number("First number."), b: s.number("Second number.") }},
  }})
  async add({{ a, b }}: {{ a: number; b: number }}): Promise<number> {{
    // The schema the model sees is built from `args`; missing or unknown arguments are reported
    // to the model before this runs.
    return a + b;
  }}
}}
"#
    );

    let readme_md = format!(
        "# {pascal_name} Plugin (TypeScript)\n\nA Kanon microkernel plugin built with TypeScript.\n"
    );

    fs::write(dir.join("plugin.toml"), plugin_toml)?;
    fs::write(dir.join("package.json"), package_json)?;
    fs::write(dir.join("tsconfig.json"), tsconfig_json)?;
    fs::write(dir.join("index.ts"), index_ts)?;
    fs::write(dir.join("README.md"), readme_md)?;

    Ok(())
}
