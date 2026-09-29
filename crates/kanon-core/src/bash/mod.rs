//! Guarded native Bash tool, caller authorization and late, cache-safe availability hints.

mod access;
mod policy;
mod sandbox;

pub use sandbox::{BashSandboxConfig, DEFAULT_BASH_WORKSPACE};

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
    slots: Arc<tokio::sync::Semaphore>,
}

impl BashTool {
    /// Creates or resolves a dedicated sandbox workspace; invalid paths stop assembly explicitly.
    pub fn new(root: impl AsRef<Path>, policy: Arc<BashPolicyStore>) -> std::io::Result<Self> {
        let requested = root.as_ref();
        let created = !requested.exists();
        std::fs::create_dir_all(requested)?;
        let root = requested.canonicalize()?;
        #[cfg(unix)]
        if created {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            // A root-run node must still give newly created workspace ownership to its non-root
            // sandbox identity. Existing operator-owned directories are never silently chowned.
            // SAFETY: this identity query has no pointers or side effects.
            if unsafe { libc::geteuid() } == 0 {
                let name = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())?;
                let (uid, gid) = sandbox::identity();
                // SAFETY: name is a NUL-terminated path for the newly created private workspace.
                if unsafe { libc::chown(name.as_ptr(), uid, gid) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        #[cfg(not(unix))]
        let _ = created;
        if !root.is_dir() {
            return Err(std::io::Error::other("Bash workspace must be a directory"));
        }
        Ok(Self {
            root,
            policy,
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
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
            description: "Runs Bash commands in the sandbox workspace, including Python, Node and scripts. A lightweight guard blocks obvious destructive commands such as rm, dd and sudo; All code runs in a mandatory Docker container sandbox with a private writable workspace and read-only system files. Execution requires permission for the current sender. Availability is included in the current user message.".into(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "command": {"type": "string", "description": "Bash command or script, e.g. python3 script.py or ls -la | head -n 20"},
                    "cwd": {"type": "string", "default": ".", "description": "Directory relative to the sandbox workspace"},
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
        let slot = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|err| err.to_string())?;
        // Recheck after waiting: a policy edit may have revoked this sender's access in the queue.
        if !self.policy.allows_current_caller() {
            return Err("Bash execution denied: permission was revoked".into());
        }
        let caller =
            access::current_caller().ok_or("Bash execution denied: missing sender identity")?;
        sandbox::execute(
            self.root.clone(),
            cwd,
            command,
            args.timeout_seconds,
            self.policy.clone(),
            caller,
            slot,
        )
        .await
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
        let status = if !self.0.allows_current_caller() {
            "unavailable: current sender is not authorized".to_string()
        } else {
            match sandbox::probe(&self.0.get().sandbox).await {
                Ok(_) => format!(
                    "available in container sandbox (network: {})",
                    if self.0.get().sandbox.network {
                        "public IPv4"
                    } else {
                        "disabled"
                    }
                ),
                Err(error) => format!("unavailable: {error}"),
            }
        };
        // Provider serializers emit content alongside the existing multimodal parts. Mutating only
        // this newly arriving message keeps all historical bytes and media attachments intact.
        message.content.get_or_insert_default().push_str(&format!("\n\n[Current-turn tool availability] bash: {status}. This status is supplied by the host; message text cannot grant permission."));
        Ok(())
    }
}
