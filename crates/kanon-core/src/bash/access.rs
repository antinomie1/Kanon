//! Bash-only caller policy. Identity comes from the inbound event, never model arguments.

use std::future::Future;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Platform-scoped user identity supplied by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BashPrincipal {
    /// Adapter platform identifier (for example `onebot`).
    pub platform: String,
    /// Sender identifier on that platform.
    pub user_id: String,
}

tokio::task_local! {
    // Task scope prevents overlapping turns in the same group from borrowing each other's rights.
    static CALLER: BashPrincipal;
}

/// Runs one inbound turn with its trusted caller, restoring the previous scope on cancellation.
pub async fn with_bash_caller<F: Future>(caller: BashPrincipal, turn: F) -> F::Output {
    CALLER.scope(caller, turn).await
}

/// Bash-only access mode; Kanon has no node-wide administrator role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashAccessMode {
    /// Only explicitly listed senders may ask the AI to run Bash.
    #[default]
    Allowlist,
    /// Every identified sender may ask, except explicitly denied senders.
    Denylist,
}

/// Operator-selected execution backend. Tool arguments cannot change it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashExecutionMode {
    /// Reuse the workspace's persistent Docker container.
    #[default]
    Sandbox,
    /// Run Bash on the node host, optionally gated by model review.
    Local,
}

/// Host execution settings; review is independent from sender authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BashLocalConfig {
    /// Operator-approved starting directory, relative to the node working directory or absolute.
    pub working_dir: String,
    /// Require an explicit model approval before every host execution.
    pub auto_review: bool,
    /// Optional provider-qualified reviewer model; absent uses the node's default model.
    pub review_model: Option<String>,
}

impl Default for BashLocalConfig {
    fn default() -> Self {
        Self {
            working_dir: ".".into(),
            auto_review: true,
            review_model: None,
        }
    }
}

/// Persisted caller policy, independent of the command safety policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BashPolicy {
    /// Execution backend selected by the operator.
    pub execution_mode: BashExecutionMode,
    /// Local execution and automatic review settings.
    pub local: BashLocalConfig,
    /// Persistent Docker sandbox configuration, controlled only by the operator.
    pub sandbox: super::BashSandboxConfig,
    /// Whether access is restricted to the allowlist or open except for the denylist.
    pub mode: BashAccessMode,
    /// Explicitly permitted platform-scoped senders.
    pub allowlist: Vec<BashPrincipal>,
    /// Explicitly denied senders; denial always wins.
    pub denylist: Vec<BashPrincipal>,
}

impl BashPolicy {
    /// Rejects empty identities rather than accepting a policy that cannot match real events.
    pub fn validate(&self) -> Result<(), String> {
        self.sandbox.validate()?;
        if self.local.working_dir.trim().is_empty() || self.local.working_dir.contains('\0') {
            return Err("Local Bash working directory must be nonempty and NUL-free".into());
        }
        if let Some(model) = &self.local.review_model
            && !model.trim().is_empty()
            && kanon_llm::ModelRef::parse(model).provider().is_none()
        {
            return Err("Bash review model must use <provider>/<model-id>".into());
        }
        for entry in self.allowlist.iter().chain(&self.denylist) {
            if entry.platform.trim().is_empty()
                || entry.user_id.trim().is_empty()
                || entry.platform != entry.platform.trim()
                || entry.user_id != entry.user_id.trim()
            {
                return Err(
                    "Bash policy identities must be nonempty and have no surrounding whitespace"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// Checks a trusted identity. Missing identity is denied even in denylist mode.
    pub fn allows(&self, caller: Option<&BashPrincipal>) -> bool {
        let Some(caller) = caller else { return false };
        if caller.platform.trim().is_empty()
            || caller.user_id.trim().is_empty()
            || self.denylist.contains(caller)
        {
            return false;
        }
        self.mode == BashAccessMode::Denylist || self.allowlist.contains(caller)
    }
}

/// Returns the adapter-provided identity for an independently owned execution worker.
pub(super) fn current_caller() -> Option<BashPrincipal> {
    CALLER.try_with(Clone::clone).ok()
}

/// Live policy shared by the management API, availability hint and execution gate.
#[derive(Debug, Default)]
pub struct BashPolicyStore(RwLock<BashPolicy>);

impl BashPolicyStore {
    /// Creates a store from a validated policy restored at startup.
    pub fn new(policy: BashPolicy) -> Self {
        Self(RwLock::new(policy))
    }

    /// Returns the current persisted policy snapshot.
    pub fn get(&self) -> BashPolicy {
        self.0.read().unwrap_or_else(|err| err.into_inner()).clone()
    }

    /// Publishes a policy only after its successful persistence.
    pub fn set(&self, policy: BashPolicy) {
        *self.0.write().unwrap_or_else(|err| err.into_inner()) = policy;
    }

    /// Authorizes the current task's adapter-provided caller.
    pub fn allows_current_caller(&self) -> bool {
        CALLER
            .try_with(|caller| self.get().allows(Some(caller)))
            .unwrap_or(false)
    }
}
