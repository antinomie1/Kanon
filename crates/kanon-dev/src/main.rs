//! Kanon official developer toolchain CLI (`kanon-dev`).
//!
//! Provides unified management commands across the entire plugin lifecycle:
//! - `plugin create <name> --lang <rust|python|ts>`: Project scaffolding
//! - `lint [path]`: Static manifest and schema validation
//! - `test [path]`: Offline sandbox node: real pipeline, KV and conversations, mock model
//! - `dev [path]`: Rebuild and restart the plugin on a running node after every change
//! - `pack [path]`: Standard `.kpk` bundle distribution packager

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use kanon_dev::{
    DEFAULT_NODE_URL, SandboxOptions, create_plugin_project, lint_plugin, pack_plugin, run_dev,
    run_sandbox,
};

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
    /// Run the plugin in an offline sandbox node (real pipeline, KV and conversations, mock model)
    Test(TestArgs),
    /// Pack a plugin into a .kpk distribution archive with SHA-256 checksum
    Pack {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Destination directory for the archive
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Rebuild and restart the plugin on a running node whenever its files change
    Dev {
        /// Path to plugin directory or plugin.toml
        path: Option<PathBuf>,
        /// Management API address of the node serving the plugin
        #[arg(long, default_value = DEFAULT_NODE_URL)]
        node: String,
    },
}

/// Options of the sandbox (`test` and `plugin test`).
#[derive(Args)]
struct TestArgs {
    /// Path to plugin directory or plugin.toml
    path: Option<PathBuf>,
    /// Run one command through the pipeline (e.g. /calc or calc) and fail unless it succeeds
    #[arg(short, long)]
    command: Option<String>,
    /// Call one tool directly, without the model
    #[arg(short, long)]
    tool: Option<String>,
    /// Arguments for the command, or JSON arguments for the tool
    #[arg(short, long)]
    args: Vec<String>,
    /// Send a message as the user; repeat for a conversation (e.g. -m "/note add milk" -m "!tool dice {}")
    #[arg(short, long = "message")]
    messages: Vec<String>,
    /// Start the plugin, print what it declares and exit
    #[arg(long)]
    non_interactive: bool,
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
    /// Run the plugin in an offline sandbox node
    Test(TestArgs),
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Plugin { action } => handle_plugin_action(action).await,
        Commands::Create { name, lang, output } => handle_create(&name, &lang, output.as_deref()),
        Commands::Lint { path } => handle_lint(path.as_deref()),
        Commands::Test(args) => handle_test(args).await,
        Commands::Pack { path, output } => handle_pack(path.as_deref(), output.as_deref()),
        Commands::Dev { path, node } => handle_dev(path.as_deref(), &node).await,
    }
}

async fn handle_plugin_action(action: PluginAction) -> ExitCode {
    match action {
        PluginAction::Create { name, lang, output } => {
            handle_create(&name, &lang, output.as_deref())
        }
        PluginAction::Lint { path } => handle_lint(path.as_deref()),
        PluginAction::Pack { path, output } => handle_pack(path.as_deref(), output.as_deref()),
        PluginAction::Test(args) => handle_test(args).await,
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
            // The sandbox builds Rust plugins and installs dependencies itself, so trying the new
            // plugin is a single command.
            println!("  Next steps:");
            println!("    cd {}", dir.display());
            println!("    kanon-dev test .        # chat with it in the offline sandbox");
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

async fn handle_test(args: TestArgs) -> ExitCode {
    let target = args.path.unwrap_or_else(|| PathBuf::from("."));
    let opts = SandboxOptions {
        command: args.command,
        tool: args.tool,
        args: args.args,
        messages: args.messages,
        non_interactive: args.non_interactive,
    };

    match run_sandbox(&target, opts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Sandbox error: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn handle_dev(path: Option<&std::path::Path>, node: &str) -> ExitCode {
    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    match run_dev(target, node).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Dev error: {}", e);
            ExitCode::FAILURE
        }
    }
}
