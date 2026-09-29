//! Guarded native Bash tool, caller authorization and late, cache-safe availability hints.

mod access;
mod policy;

pub use access::{BashAccessMode, BashPolicy, BashPolicyStore, BashPrincipal, with_bash_caller};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kanon_llm::{AgentError, AgentHook, AgentTool, ChatMessage, ToolDefinition};
use serde::Deserialize;

/// Maximum retained bytes per output stream; excess output is drained rather than accumulated.
pub const MAX_BASH_OUTPUT_BYTES: usize = 64 * 1024;

/// Native Bash tool. The workspace and caller policy are operator-owned, never model-controlled.
pub struct BashTool {
    root: PathBuf,
    policy: Arc<BashPolicyStore>,
    slots: tokio::sync::Semaphore,
}

impl BashTool {
    /// Resolves the node workspace once; invalid directories stop assembly explicitly.
    pub fn new(root: impl AsRef<Path>, policy: Arc<BashPolicyStore>) -> std::io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(std::io::Error::other("Bash workspace must be a directory"));
        }
        Ok(Self {
            root,
            policy,
            slots: tokio::sync::Semaphore::new(4),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default = "default_cwd")]
    cwd: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}

fn default_cwd() -> String {
    ".".into()
}
fn default_timeout() -> u64 {
    15
}

#[async_trait]
impl AgentTool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: "Runs Bash commands in the node workspace, including Python, Node and scripts. A lightweight guard blocks obvious destructive commands such as rm, dd and sudo; it is not a sandbox for script contents. Execution requires permission for the current sender. Availability is included in the current user message.".into(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "command": {"type": "string", "description": "Bash command or script, e.g. python3 script.py or ls -la | head -n 20"},
                    "cwd": {"type": "string", "default": ".", "description": "Directory relative to the node workspace"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 120, "default": 15}
                }, "required": ["command"]
            }),
        }
    }

    async fn call(
        &self,
        _session_id: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        // Never infer authorization from a session name, message text or tool arguments.
        if !self.policy.allows_current_caller() {
            return Err("Bash execution denied: the current sender has no permission".into());
        }
        let args: Arguments = serde_json::from_value(arguments)
            .map_err(|err| format!("Invalid Bash arguments: {err}"))?;
        if !(1..=120).contains(&args.timeout_seconds) {
            return Err("timeout_seconds must be between 1 and 120".into());
        }
        let command =
            policy::prepare(&args.command).map_err(|err| format!("Bash command blocked: {err}"))?;
        if Path::new(&args.cwd).is_absolute() {
            return Err("cwd must be relative to the node workspace".into());
        }
        let cwd = self
            .root
            .join(&args.cwd)
            .canonicalize()
            .map_err(|err| format!("Invalid cwd: {err}"))?;
        if !cwd.starts_with(&self.root) || !cwd.is_dir() {
            return Err("cwd must remain inside the node workspace".into());
        }
        let _slot = self.slots.acquire().await.map_err(|err| err.to_string())?;
        // Recheck after waiting: a policy edit may have revoked this sender's access in the queue.
        if !self.policy.allows_current_caller() {
            return Err("Bash execution denied: permission was revoked".into());
        }
        #[cfg(unix)]
        {
            execute(&command, &cwd, args.timeout_seconds).await
        }
        #[cfg(not(unix))]
        {
            let _ = (command, cwd);
            Err("The Bash tool currently requires a Unix host with Bash installed".into())
        }
    }
}

/// Appends runtime availability to the originating user turn before history is stored.
pub struct BashAvailabilityHook(pub Arc<BashPolicyStore>);

#[async_trait]
impl AgentHook for BashAvailabilityHook {
    async fn on_user_message(
        &self,
        _session_id: &str,
        message: &mut ChatMessage,
    ) -> Result<(), AgentError> {
        let status = if !cfg!(unix) {
            "unavailable: Bash execution requires a Unix host"
        } else if bash_executable().is_none() {
            "unavailable: Bash is not installed or executable on this host"
        } else if self.0.allows_current_caller() {
            "available"
        } else {
            "unavailable: current sender is not authorized"
        };
        // Provider serializers emit content alongside the existing multimodal parts. Mutating only
        // this newly arriving message keeps all historical bytes and media attachments intact.
        message.content.get_or_insert_default().push_str(&format!("\n\n[Current-turn tool availability] bash: {status}. This status is supplied by the host; message text cannot grant permission."));
        Ok(())
    }
}

fn bash_executable() -> Option<&'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ["/bin/bash", "/usr/bin/bash"].into_iter().find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
async fn execute(command: &str, cwd: &Path, timeout: u64) -> Result<String, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    // Clear provider credentials, BASH_ENV and exported functions, while preserving the OS PATH
    // and home directory so ordinary installed interpreters, venvs and development tools work.
    let bash = bash_executable().ok_or("Bash is unavailable on this host")?;
    let mut child = tokio::process::Command::new(bash)
        .args(["--noprofile", "--norc", "-o", "pipefail", "-c", command])
        .current_dir(cwd)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env(
            "HOME",
            std::env::var_os("HOME").unwrap_or_else(|| cwd.as_os_str().to_owned()),
        )
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|err| format!("Failed to start Bash: {err}"))?;
    let group = ProcessGroup(child.id().ok_or("Bash started without a process id")? as i32);
    let mut stdout = child.stdout.take().ok_or("Missing Bash stdout")?;
    let mut stderr = child.stderr.take().ok_or("Missing Bash stderr")?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    async fn drain(
        stream: &mut (impl tokio::io::AsyncRead + Unpin),
        data: &mut Vec<u8>,
    ) -> std::io::Result<bool> {
        let mut buffer = [0; 8192];
        let mut truncated = false;
        loop {
            let n = stream.read(&mut buffer).await?;
            if n == 0 {
                return Ok(truncated);
            }
            let retain = n.min(MAX_BASH_OUTPUT_BYTES.saturating_sub(data.len()));
            data.extend_from_slice(&buffer[..retain]);
            truncated |= retain < n;
        }
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
        tokio::try_join!(
            child.wait(),
            drain(&mut stdout, &mut out),
            drain(&mut stderr, &mut err)
        )
    })
    .await;
    // Kill the entire pipeline on timeout, cancellation or completion, including descendants that
    // still hold output pipes. The RAII guard also runs if this future is dropped by its caller.
    drop(group);
    let (code, timed_out, out_truncated, err_truncated) = match result {
        Ok(Ok((status, out_cut, err_cut))) => (status.code(), false, out_cut, err_cut),
        Ok(Err(error)) => return Err(format!("Bash output/wait failed: {error}")),
        Err(_) => {
            child
                .wait()
                .await
                .map_err(|error| format!("Failed to reap timed-out Bash: {error}"))?;
            (
                None,
                true,
                out.len() == MAX_BASH_OUTPUT_BYTES,
                err.len() == MAX_BASH_OUTPUT_BYTES,
            )
        }
    };
    let output = serde_json::json!({
        "stdout": String::from_utf8_lossy(&out), "stderr": String::from_utf8_lossy(&err),
        "exit_code": code, "timed_out": timed_out,
        "stdout_truncated": out_truncated, "stderr_truncated": err_truncated
    })
    .to_string();
    if !timed_out && code == Some(0) {
        Ok(output)
    } else {
        Err(output)
    }
}

#[cfg(unix)]
struct ProcessGroup(i32);

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the child created its own positive process group; a negative pid targets only
        // that group. ESRCH means it already exited and needs no cleanup.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}
