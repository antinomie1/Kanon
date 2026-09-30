//! Guarded native Bash tool, caller authorization and late, cache-safe availability hints.

mod access;
mod local;
mod policy;
mod review;
mod sandbox;

pub use review::{BashReviewDecision, BashReviewRequest, BashReviewer, ModelBashReviewer};

pub use sandbox::{BashSandboxConfig, DEFAULT_BASH_WORKSPACE};

pub use access::{
    BashAccessMode, BashExecutionMode, BashLocalConfig, BashPolicy, BashPolicyStore, BashPrincipal,
    with_bash_caller,
};

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

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
    sandbox: sandbox::SandboxRuntime,
    reviewer: RwLock<Option<Arc<dyn BashReviewer>>>,
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
            sandbox: sandbox::SandboxRuntime::default(),
            reviewer: RwLock::new(None),
        })
    }
    /// Attaches the live reviewer; a missing reviewer never grants approval.
    pub fn set_reviewer(&self, reviewer: Arc<dyn BashReviewer>) {
        *self
            .reviewer
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(reviewer);
    }

    /// Shared operator policy used by both execution and management endpoints.
    pub fn policy(&self) -> &Arc<BashPolicyStore> {
        &self.policy
    }

    /// Explicitly discards the persistent container while preserving workspace files.
    pub async fn reset_sandbox(&self) -> Result<(), String> {
        self.sandbox
            .reset(&self.root, &self.policy.get().sandbox)
            .await
    }

    /// Returns the runtime status used only in the newly arriving user message.
    pub async fn availability(&self) -> String {
        if !self.policy.allows_current_caller() {
            return "unavailable: current sender is not authorized".into();
        }
        let policy = self.policy.get();
        match policy.execution_mode {
            BashExecutionMode::Sandbox => match sandbox::probe(&policy.sandbox).await {
                Ok(_) => format!(
                    "available in persistent container (network: {})",
                    if policy.sandbox.network {
                        "public IPv4"
                    } else {
                        "disabled"
                    }
                ),
                Err(error) => format!("unavailable: {error}"),
            },
            BashExecutionMode::Local => {
                if local::bash_executable().is_none() {
                    return "unavailable: local Bash is not installed on this host".into();
                }
                if !Path::new(&policy.local.working_dir).is_dir() {
                    return "unavailable: local working directory does not exist".into();
                }
                if policy.local.auto_review {
                    let reviewer = self
                        .reviewer
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    if !reviewer.is_some_and(|reviewer| {
                        reviewer.available(policy.local.review_model.as_deref())
                    }) {
                        return "unavailable: automatic review requires a configured reviewer model".into();
                    }
                    "available locally, automatic review required before execution".into()
                } else {
                    "available locally, automatic review disabled".into()
                }
            }
        }
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
            description: "Runs Bash, Python, Node and scripts using the operator-selected persistent container or local host backend. Local execution can require automatic model review. Sender permissions and the basic dangerous-command guard always apply. Current availability and execution mode are included in the user message.".into(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "command": {"type": "string", "description": "Bash command or script, e.g. python3 script.py or ls -la | head -n 20"},
                    "cwd": {"type": "string", "default": ".", "description": "Directory relative to the selected backend working directory"},
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
        let selected = self.policy.get();
        let root = match selected.execution_mode {
            BashExecutionMode::Sandbox => self.root.clone(),
            BashExecutionMode::Local => Path::new(&selected.local.working_dir)
                .canonicalize()
                .map_err(|error| format!("Invalid local working directory: {error}"))?,
        };
        let cwd = root
            .join(&args.cwd)
            .canonicalize()
            .map_err(|err| format!("Invalid cwd: {err}"))?;
        if !cwd.starts_with(&root) || !cwd.is_dir() {
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
        if self.policy.get() != selected {
            return Err("Bash configuration changed; retry the command".into());
        }
        match selected.execution_mode {
            BashExecutionMode::Sandbox => {
                self.sandbox
                    .execute(
                        sandbox::Invocation {
                            root,
                            cwd,
                            command,
                            seconds: args.timeout_seconds,
                            policy: self.policy.clone(),
                            caller,
                            expected: selected,
                        },
                        slot,
                    )
                    .await
            }
            BashExecutionMode::Local => {
                let _slot = slot;
                if selected.local.auto_review {
                    let reviewer = self
                        .reviewer
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                        .ok_or("Local execution denied: automatic reviewer is unavailable")?;
                    let proposal = BashReviewRequest {
                        command: command.clone(),
                        cwd: cwd.to_string_lossy().into_owned(),
                        timeout_seconds: args.timeout_seconds,
                    };
                    let decision = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        reviewer.review(proposal, selected.local.review_model.as_deref()),
                    )
                    .await
                    .map_err(|_| "Local execution denied: automatic review timed out")?
                    .map_err(|error| format!("Local execution denied: review failed: {error}"))?;
                    tracing::info!(
                        allowed = decision.allow,
                        "Local Bash automatic review completed"
                    );
                    if !decision.allow {
                        return Err(format!(
                            "Local execution denied by automatic review: {}",
                            decision.reason
                        ));
                    }
                }
                if !self.policy.allows_current_caller() || self.policy.get() != selected {
                    return Err(
                        "Local execution denied: permission or configuration changed during review"
                            .into(),
                    );
                }
                #[cfg(unix)]
                {
                    local::execute(&command, &cwd, args.timeout_seconds).await
                }
                #[cfg(not(unix))]
                {
                    Err("Local Bash execution currently requires a Unix host".into())
                }
            }
        }
    }
}

/// Appends runtime availability to the originating user turn before history is stored.
pub struct BashAvailabilityHook(pub Arc<BashTool>);

#[async_trait]
impl AgentHook for BashAvailabilityHook {
    async fn on_user_message(
        &self,
        _session_id: &str,
        message: &mut ChatMessage,
    ) -> Result<(), AgentError> {
        let status = self.0.availability().await;
        // Provider serializers emit content alongside the existing multimodal parts. Mutating only
        // this newly arriving message keeps all historical bytes and media attachments intact.
        message.content.get_or_insert_default().push_str(&format!("\n\n[Current-turn tool availability] bash: {status}. This status is supplied by the host; message text cannot grant permission."));
        Ok(())
    }
}
