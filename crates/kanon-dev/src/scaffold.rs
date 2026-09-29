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
description = "High-performance Kanon plugin written in Rust"
runtime = "rust"
entrypoint = "target/debug/{snake_name}"
isolated = false
priority = 500

[[commands]]
name = "{snake_name}_echo"
description = "Echoes the provided message with prefix"
usage = "/{snake_name}_echo <message>"
priority = 500

[[tools]]
name = "{snake_name}_calc"
description = "Performs mathematical calculations"
parameters = {{ type = "object", properties = {{ expr = {{ type = "string", description = "Mathematical expression" }} }} }}
"#
    );

    let cargo_toml = format!(
        r#"[package]
name = "{snake_name}"
version = "0.1.0"
edition = "2024"
description = "Kanon Rust Plugin: {pascal_name}"

[dependencies]
kanon-sdk = "0.1.0"
tokio = {{ version = "1.40", features = ["full"] }}
serde_json = "1.0"
"#
    );

    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir)?;

    let main_rs = format!(
        r#"//! Starter implementation for {pascal_name} plugin.

use kanon_sdk::prelude::message_segment::Segment;
use kanon_sdk::prelude::*;

struct {pascal_name}Plugin;

#[async_trait]
impl Plugin for {pascal_name}Plugin {{
    fn meta(&self) -> PluginMeta {{
        PluginMeta {{
            id: "org.kanon.plugin.{snake_name}".to_string(),
            name: "{pascal_name} Plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "Kanon Dev".to_string(),
            description: "High-performance Kanon plugin written in Rust".to_string(),
            commands: vec![CommandMeta {{
                name: "{snake_name}_echo".to_string(),
                description: "Echoes the provided message with prefix".to_string(),
                usage: "/{snake_name}_echo <message>".to_string(),
                priority: 500,
            }}],
            tools: vec![ToolMeta {{
                name: "{snake_name}_calc".to_string(),
                description: "Performs mathematical calculations".to_string(),
                parameters: None,
            }}],
        }}
    }}

    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {{
        println!("{pascal_name} Plugin loaded with data dir: {{:?}}", ctx.data_dir);
        Ok(())
    }}

    async fn on_pre_filter(&self, _req: PipelineEventRequest) -> PluginResult<Option<PreFilterResult>> {{
        Ok(None)
    }}

    async fn on_execute_command(&self, req: CommandExecuteRequest) -> PluginResult<CommandExecuteResponse> {{
        let text = format!("[{pascal_name}] Echo: {{}}", req.args.join(" "));
        let reply = MessageSegment {{
            segment: Some(Segment::Text(TextSegment {{ content: text }})),
        }};
        Ok(CommandExecuteResponse {{
            success: true,
            replies: vec![reply],
            error_message: String::new(),
        }})
    }}

    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {{
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("result".to_string(), prost_types::Value {{
            kind: Some(prost_types::value::Kind::StringValue("Sample computation from Rust".to_string())),
        }});
        Ok(ToolCallResponse {{
            call_id: req.call_id,
            success: true,
            error_message: String::new(),
            payload: Some(tool_call_response::Payload::StructuredResult(prost_types::Struct {{ fields }})),
        }})
    }}
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    KanonHost::new({pascal_name}Plugin).run().await?;
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
description = "Echoes the provided message with prefix"
usage = "/{snake_name}_echo <message>"
priority = 500

[[tools]]
name = "{snake_name}_calc"
description = "Performs mathematical calculations"
parameters = {{ type = "object", properties = {{ expr = {{ type = "string", description = "Mathematical expression" }} }} }}
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
        r#""""{pascal_name} Plugin for Kanon Microkernel."""

from typing import Any, Dict, List, Optional
from kanon_sdk import Plugin, PluginContext, MessageSegment, command, tool
from kanon_sdk.proto import pb

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
        name="{snake_name}_echo",
        description="Echoes the provided message with prefix",
        usage="/{snake_name}_echo <message>",
        priority=500,
    )
    async def handle_echo(self, req: pb.CommandExecuteRequest, args: List[str]) -> pb.CommandExecuteResponse:
        reply_text = f"[{pascal_name}] Echo: {{' '.join(args)}}"
        return pb.CommandExecuteResponse(
            success=True,
            replies=[MessageSegment.text(reply_text)],
            error_message="",
        )

    @tool(
        name="{snake_name}_calc",
        description="Performs mathematical calculations",
        parameters={{
            "type": "object",
            "properties": {{
                "expr": {{"type": "string", "description": "Mathematical expression"}},
            }},
        }},
    )
    async def handle_calc(self, params: Dict[str, Any]) -> Dict[str, Any]:
        return {{
            "result": 42.0,
            "summary": "Sample calculation from Python plugin tool",
        }}
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
description = "Echoes the provided message with prefix"
usage = "/{snake_name}_echo <message>"
priority = 500

[[tools]]
name = "{snake_name}_calc"
description = "Performs mathematical calculations"
parameters = {{ type = "object", properties = {{ expr = {{ type = "string", description = "Mathematical expression" }} }} }}
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
        r#"import {{
  Plugin,
  PluginContext,
  Command,
  Tool,
  MessageSegment,
}} from "@kanon/sdk-and-host";

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
    description: "Echoes the provided message with prefix",
    usage: "/{snake_name}_echo <message>",
    priority: 500,
  }})
  async handleEcho(req: any, args: string[]): Promise<any> {{
    const message = args && args.length > 0 ? args.join(" ") : "(empty)";
    return {{
      success: true,
      replies: [
        MessageSegment.text(`[{pascal_name}] Echo: ${{message}}`),
      ],
      error_message: "",
    }};
  }}

  @Tool({{
    name: "{snake_name}_calc",
    description: "Performs mathematical calculations",
    parameters: {{
      type: "object",
      properties: {{
        expr: {{ type: "string", description: "Mathematical expression" }},
      }},
    }},
  }})
  async handleCalc(params: any): Promise<any> {{
    return {{
      result: 42,
      summary: "Sample calculation from TypeScript plugin tool",
    }};
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
