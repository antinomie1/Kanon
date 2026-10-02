//! Installing a plugin's dependencies with its ecosystem's own tool, before the plugin starts.
//!
//! Kanon is not a package manager: it never resolves, downloads or locks packages itself, and
//! every plugin keeps its own environment inside its directory (`<plugin>/.venv`,
//! `<plugin>/node_modules`), so two plugins can never disagree about a package version. What the
//! supervisor does is run the native tool the plugin's author already uses, in the plugin
//! directory, when the environment is missing or older than the files that describe it:
//!
//! | Runtime | Described by | Command |
//! | --- | --- | --- |
//! | Python | `pyproject.toml`, `uv.lock` | `uv sync` (`--locked` with a lockfile) |
//! | TypeScript | `package.json` + lockfile | `bun install --frozen-lockfile` (`bun.lock`), `npm ci` (`package-lock.json`), otherwise `bun install` or `npm install` |
//!
//! After a successful run a marker file is written into the environment; its modification time
//! is what "older than its description" is measured against, so editing `pyproject.toml` or
//! `package.json` re-installs at the next launch while an up-to-date plugin costs a few `stat`s.
//!
//! Failures are explicit: the plugin is reported `RuntimeUnavailable` with the tool's last lines
//! of output, and nothing falls back to a shared or system environment. The one leniency: an
//! environment that exists but cannot be refreshed because the tool is not installed is used as
//! it is, with a warning, since it is still the plugin's own environment.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Marker written into an environment after the tool finished successfully.
const MARKER: &str = ".kanon-installed";

/// Lines of tool output kept in a failure report.
const OUTPUT_TAIL_LINES: usize = 20;

/// How long one install may take before it is killed.
pub const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs plugins' native dependency tools; see the module documentation.
pub struct DependencyInstaller {
    /// Directories searched for `uv`, `bun` and `npm`; `None` searches `PATH`.
    search_path: Option<Vec<PathBuf>>,
    timeout: Duration,
    /// One lock per plugin directory: a restart racing a first launch must not run two installs
    /// into the same environment. Plugins are few, so entries are never removed.
    locks: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for DependencyInstaller {
    fn default() -> Self {
        Self {
            search_path: None,
            timeout: DEFAULT_INSTALL_TIMEOUT,
            locks: Mutex::new(HashMap::new()),
        }
    }
}

impl std::fmt::Debug for DependencyInstaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DependencyInstaller")
            .field("search_path", &self.search_path)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl DependencyInstaller {
    /// An installer that finds the tools on `PATH` and allows [`DEFAULT_INSTALL_TIMEOUT`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks for the tools only in `dirs` instead of `PATH`.
    pub fn with_search_path(mut self, dirs: Vec<PathBuf>) -> Self {
        self.search_path = Some(dirs);
        self
    }

    /// Kills an install that runs longer than `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Makes the Python plugin's `.venv` match its `pyproject.toml` and returns its interpreter.
    pub async fn prepare_python(&self, plugin_dir: &Path) -> Result<PathBuf, String> {
        let _installing = self.lock(plugin_dir).await;
        let env = plugin_dir.join(".venv");
        let lockfile = plugin_dir.join("uv.lock");
        let inputs = [plugin_dir.join("pyproject.toml"), lockfile.clone()];
        if plugin_python(plugin_dir).is_none() || is_stale(&env, &inputs) {
            match self.find_tool("uv") {
                Some(uv) => {
                    let mut args = vec!["sync"];
                    if lockfile.is_file() {
                        args.push("--locked");
                    }
                    // uv resolves a relative UV_PROJECT_ENVIRONMENT against the project, not
                    // the working directory: with the node's default `./plugins` the
                    // environment would land in `<plugin>/plugins/<plugin>/.venv`. Absolute it
                    // is, without resolving symbolic links, so a linked plugin folder keeps its
                    // environment beside its sources.
                    let project_env = std::path::absolute(&env).map_err(|err| {
                        format!(
                            "cannot resolve the environment path '{}': {err}",
                            env.display()
                        )
                    })?;
                    let mut command = tokio::process::Command::new(&uv);
                    // The environment must be exactly `<plugin>/.venv`, whatever the node's own
                    // environment says: an inherited UV_PROJECT_ENVIRONMENT would install
                    // elsewhere, and an active VIRTUAL_ENV only makes uv warn.
                    command
                        .args(&args)
                        .env("UV_PROJECT_ENVIRONMENT", &project_env)
                        .env_remove("VIRTUAL_ENV");
                    self.run(plugin_dir, &format!("uv {}", args.join(" ")), command)
                        .await?;
                    mark_installed(&env);
                }
                None if plugin_python(plugin_dir).is_some() => tracing::warn!(
                    plugin_dir = %plugin_dir.display(),
                    "Python environment may be out of date, but `uv` is not installed to refresh it; \
                     starting with the existing .venv"
                ),
                None => {
                    return Err(format!(
                        "Python environment '{}' not found and `uv` is not installed to create it; \
                         install uv or run `uv sync` in the plugin directory",
                        env.display()
                    ));
                }
            }
        }
        plugin_python(plugin_dir).ok_or_else(|| {
            format!(
                "Python environment '{}' has no interpreter after `uv sync`",
                env.display()
            )
        })
    }

    /// Makes the TypeScript plugin's `node_modules` match its `package.json`.
    ///
    /// A plugin without `package.json`, or without `dependencies` in it, needs nothing.
    pub async fn prepare_node(&self, plugin_dir: &Path) -> Result<(), String> {
        if !declares_node_dependencies(plugin_dir)? {
            return Ok(());
        }
        let _installing = self.lock(plugin_dir).await;
        let modules = plugin_dir.join("node_modules");
        let bun_lock = ["bun.lock", "bun.lockb"]
            .into_iter()
            .map(|name| plugin_dir.join(name))
            .find(|path| path.is_file());
        let npm_lock = plugin_dir.join("package-lock.json");
        let mut inputs = vec![plugin_dir.join("package.json"), npm_lock.clone()];
        inputs.extend(bun_lock.clone());
        if modules.is_dir() && !is_stale(&modules, &inputs) {
            return Ok(());
        }

        // The lockfile names the tool that wrote it; using the other one would rewrite the
        // lockfile in the plugin's directory or ignore it.
        let (tool, args): (&str, &[&str]) = if bun_lock.is_some() {
            ("bun", &["install", "--frozen-lockfile"])
        } else if npm_lock.is_file() {
            ("npm", &["ci"])
        } else if self.find_tool("bun").is_some() {
            ("bun", &["install"])
        } else {
            ("npm", &["install"])
        };
        let Some(binary) = self.find_tool(tool) else {
            if modules.is_dir() {
                tracing::warn!(
                    plugin_dir = %plugin_dir.display(),
                    tool,
                    "node_modules may be out of date, but the tool to refresh it is not installed; \
                     starting with the existing node_modules"
                );
                return Ok(());
            }
            return Err(format!(
                "dependencies in '{}' are not installed and `{tool}` is not available to install \
                 them; install {tool} or run `{tool} {}` in the plugin directory",
                plugin_dir.join("package.json").display(),
                args.join(" ")
            ));
        };
        let mut command = tokio::process::Command::new(&binary);
        command.args(args);
        self.run(plugin_dir, &format!("{tool} {}", args.join(" ")), command)
            .await?;
        mark_installed(&modules);
        Ok(())
    }

    /// Runs one install command in the plugin directory, bounded by the timeout.
    async fn run(
        &self,
        plugin_dir: &Path,
        command_line: &str,
        mut command: tokio::process::Command,
    ) -> Result<(), String> {
        tracing::info!(
            plugin_dir = %plugin_dir.display(),
            command = %command_line,
            "Installing plugin dependencies"
        );
        let started = Instant::now();
        command
            .current_dir(plugin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A timed-out install is dropped mid-wait; the child must die with it rather than
            // keep writing into the environment the next attempt will use.
            .kill_on_drop(true);
        let output = match tokio::time::timeout(self.timeout, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => return Err(format!("`{command_line}` could not be started: {err}")),
            Err(_) => {
                return Err(format!(
                    "`{command_line}` did not finish within {} s and was stopped",
                    self.timeout.as_secs()
                ));
            }
        };
        if !output.status.success() {
            // Tools print the reason on stderr; npm sometimes only on stdout.
            let text = if output.stderr.is_empty() {
                &output.stdout
            } else {
                &output.stderr
            };
            return Err(format!(
                "`{command_line}` failed ({}):\n{}",
                output.status,
                tail(&String::from_utf8_lossy(text))
            ));
        }
        tracing::info!(
            plugin_dir = %plugin_dir.display(),
            command = %command_line,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Plugin dependencies installed"
        );
        Ok(())
    }

    async fn lock(&self, plugin_dir: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let key = plugin_dir
            .canonicalize()
            .unwrap_or_else(|_| plugin_dir.to_path_buf());
        let lock = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    fn find_tool(&self, name: &str) -> Option<PathBuf> {
        match &self.search_path {
            Some(dirs) => dirs
                .iter()
                .map(|dir| dir.join(name))
                .find(|path| path.is_file()),
            None => super::find_binary_in_path(name),
        }
    }
}

/// Returns the interpreter of the plugin's own virtual environment (`<plugin>/.venv`).
///
/// `is_file` follows the venv's interpreter symlink, so an environment whose base Python was
/// removed counts as missing rather than failing later with a confusing spawn error.
pub(crate) fn plugin_python(plugin_dir: &Path) -> Option<PathBuf> {
    let python = if cfg!(windows) {
        plugin_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        plugin_dir.join(".venv").join("bin").join("python")
    };
    python.is_file().then_some(python)
}

/// Whether the plugin's `package.json` declares runtime dependencies.
pub(crate) fn declares_node_dependencies(plugin_dir: &Path) -> Result<bool, String> {
    let manifest = plugin_dir.join("package.json");
    let content = match std::fs::read_to_string(&manifest) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot read '{}': {error}", manifest.display())),
    };
    let package: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("invalid '{}': {error}", manifest.display()))?;
    Ok(package
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|dependencies| !dependencies.is_empty()))
}

/// Whether the environment's marker is missing or older than any existing input file.
fn is_stale(env: &Path, inputs: &[PathBuf]) -> bool {
    let Some(installed) = modified(&env.join(MARKER)) else {
        return true;
    };
    inputs
        .iter()
        .filter_map(|input| modified(input))
        .any(|changed| changed > installed)
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Records a successful install. A marker that cannot be written only costs a re-install at the
/// next launch, so it is logged rather than failing a plugin whose environment is fine.
fn mark_installed(env: &Path) {
    if let Err(err) = std::fs::write(env.join(MARKER), b"") {
        tracing::warn!(
            env = %env.display(),
            error = %err,
            "Could not record the dependency install; it will run again at the next launch"
        );
    }
}

/// The last lines of a tool's output.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(OUTPUT_TAIL_LINES)..].join("\n")
}
