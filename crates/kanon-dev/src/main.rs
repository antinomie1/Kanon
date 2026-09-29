//! Kanon official developer toolchain CLI (`kanon-dev`).
//!
//! Provides unified management commands across the entire plugin lifecycle:
//! - `plugin create <name> --lang <rust|python|ts>`: Project scaffolding
//! - `lint [path]`: Static manifest and schema validation
//! - `test [path]`: Offline terminal sandbox for commands and tool calling
//! - `pack [path]`: Standard `.kpk` bundle distribution packager

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use kanon_dev::{SandboxOptions, create_plugin_project, lint_plugin, pack_plugin, run_sandbox};

#[derive(Parser)]
#[command(
    name = "kanon-dev",
    version,
    about = "Official developer toolchain and CLI for Kanon plugins"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Plugin lifecycle management commands
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Create a new plugin from template (shorthand for `plugin create`)
    Create {
        /// Name of the new plugin
        name: String,
        /// Programming language: rust, python, typescript (ts)
        #[arg(long, default_value = "python")]
        lang: String,
        /// Target output directory
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Statically validate a plugin manifest (shorthand for `plugin lint`)
    Lint {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
    },
    /// Run offline sandbox environment for command and tool testing
    Test {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Specific command to execute (e.g. /calc or calc)
        #[arg(short, long)]
        command: Option<String>,
        /// Specific tool to call
        #[arg(short, long)]
        tool: Option<String>,
        /// Arguments for the command or JSON arguments for the tool
        #[arg(short, long)]
        args: Vec<String>,
        /// Run in non-interactive probe mode
        #[arg(long)]
        non_interactive: bool,
    },
    /// Pack a plugin into a .kpk distribution archive with SHA-256 checksum
    Pack {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Destination directory for the archive
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Start dev server with hot reload
    Dev,
}

#[derive(Subcommand)]
enum PluginAction {
    /// Create a new plugin from template
    Create {
        /// Name of the new plugin
        name: String,
        /// Programming language: rust, python, typescript (ts)
        #[arg(long, default_value = "python")]
        lang: String,
        /// Target output directory
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Statically validate a plugin manifest
    Lint {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
    },
    /// Pack a plugin into a .kpk distribution archive
    Pack {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Destination directory for the archive
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run offline sandbox tests
    Test {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Specific command to execute
        #[arg(short, long)]
        command: Option<String>,
        /// Specific tool to call
        #[arg(short, long)]
        tool: Option<String>,
        /// Arguments for command or tool
        #[arg(short, long)]
        args: Vec<String>,
        /// Non-interactive execution
        #[arg(long)]
        non_interactive: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Plugin { action } => handle_plugin_action(action).await,
        Commands::Create { name, lang, output } => handle_create(&name, &lang, output.as_deref()),
        Commands::Lint { path } => handle_lint(path.as_deref()),
        Commands::Test {
            path,
            command,
            tool,
            args,
            non_interactive,
        } => handle_test(path.as_deref(), command, tool, args, non_interactive).await,
        Commands::Pack { path, output } => handle_pack(path.as_deref(), output.as_deref()),
        Commands::Dev => {
            println!("Starting Kanon dev server with file watcher and hot reload...");
            println!("(Press Ctrl+C to stop)");
            ExitCode::SUCCESS
        }
    }
}

async fn handle_plugin_action(action: PluginAction) -> ExitCode {
    match action {
        PluginAction::Create { name, lang, output } => {
            handle_create(&name, &lang, output.as_deref())
        }
        PluginAction::Lint { path } => handle_lint(path.as_deref()),
        PluginAction::Pack { path, output } => handle_pack(path.as_deref(), output.as_deref()),
        PluginAction::Test {
            path,
            command,
            tool,
            args,
            non_interactive,
        } => handle_test(path.as_deref(), command, tool, args, non_interactive).await,
    }
}

fn handle_create(name: &str, lang: &str, output: Option<&std::path::Path>) -> ExitCode {
    match create_plugin_project(name, lang, output) {
        Ok(dir) => {
            println!(
                "✓ Successfully created {} plugin in '{}'",
                lang,
                dir.display()
            );
            println!("  Next steps:");
            println!("    cd {}", dir.display());
            if lang == "rust" {
                println!("    cargo build");
            } else if lang == "python" || lang == "py" {
                println!("    uv sync");
            } else {
                println!("    npm install");
            }
            println!("    kanon-dev test .");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error creating plugin project: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn handle_lint(path: Option<&std::path::Path>) -> ExitCode {
    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    match lint_plugin(target) {
        Ok(report) => {
            println!(
                "Linting plugin manifest: {}",
                report.manifest_path.display()
            );
            if let Some(ref id) = report.plugin_id {
                println!(
                    "  Plugin: {} ({})",
                    report.plugin_name.as_deref().unwrap_or_default(),
                    id
                );
            }
            if let Some(ref runtime) = report.runtime {
                println!("  Runtime: {}", runtime);
            }

            for warn in &report.warnings {
                println!("  [WARN] {}", warn);
            }

            if report.is_valid() {
                println!(
                    "✓ Manifest validation PASSED with 0 errors ({} warnings)",
                    report.warnings.len()
                );
                ExitCode::SUCCESS
            } else {
                for err in &report.errors {
                    eprintln!("  [ERROR] {}", err);
                }
                eprintln!(
                    "✗ Manifest validation FAILED with {} error(s)",
                    report.errors.len()
                );
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("Error during linting: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn handle_pack(path: Option<&std::path::Path>, output: Option<&std::path::Path>) -> ExitCode {
    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    match pack_plugin(target, output) {
        Ok(report) => {
            println!("✓ Successfully packed plugin into distribution bundle:");
            println!("  Bundle:   {}", report.bundle_path.display());
            println!("  Checksum: {}", report.checksum_path.display());
            println!("  SHA-256:  {}", report.sha256_hex);
            println!("  Files:    {}", report.file_count);
            println!(
                "  Size:     {:.2} KB",
                report.bundle_size_bytes as f64 / 1024.0
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error packaging plugin: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn handle_test(
    path: Option<&std::path::Path>,
    command: Option<String>,
    tool: Option<String>,
    args: Vec<String>,
    non_interactive: bool,
) -> ExitCode {
    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    let opts = SandboxOptions {
        command,
        tool,
        args,
        non_interactive,
    };

    match run_sandbox(target, opts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Sandbox error: {}", e);
            ExitCode::FAILURE
        }
    }
}
