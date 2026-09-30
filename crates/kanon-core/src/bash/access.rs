//! Who may run Bash, and the operator settings that choose how it runs.
//!
//! Bash is granted to the node's explicitly listed administrators — the same `<platform>:<user id>`
//! entries of [`CommandPolicy::admins`] that unlock restricted commands. Group owners and admins as
//! reported by a platform never qualify: they are chosen by the group, not by the operator, and a
//! shell on the node is not something a group may hand out. The identity comes from the inbound
//! event, never from model arguments, message text or a session name.

use std::future::Future;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::access::CommandPolicy;

tokio::task_local! {
    // Task scope keeps overlapping turns in one group from borrowing each other's rights. `None`
    // explicitly shadows any outer scope for turns that have no individual sender.
    static CALLER: Option<String>;
}

/// Runs one turn with its verified caller (`<platform>:<user id>`), or explicitly without one.
///
/// A turn without a caller can never run Bash; that is what a notice or a group session carrying
/// other members' words gets.
pub async fn with_bash_caller<F: Future>(caller: Option<String>, turn: F) -> F::Output {
    CALLER.scope(caller, turn).await
}

/// Returns the caller of the current turn, when it has one.
pub(super) fn current_caller() -> Option<String> {
    CALLER.try_with(Clone::clone).ok().flatten()
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

/// Host execution settings; review is independent from caller authorization.
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
            // The dedicated workspace, not the node directory: a careless `rm -rf *` or a stray
            // `cat` must not start next to `data/system.json` and the session database.
            working_dir: super::DEFAULT_BASH_WORKSPACE.into(),
            auto_review: true,
            review_model: None,
        }
    }
}

/// Persisted Bash settings: an off switch and the execution backend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BashPolicy {
    /// Whether administrators may run Bash at all. Off by default, so naming an administrator for
    /// commands never grants a shell by itself.
    pub enabled: bool,
    /// Execution backend selected by the operator.
    pub execution_mode: BashExecutionMode,
    /// Local execution and automatic review settings.
    pub local: BashLocalConfig,
    /// Persistent Docker sandbox configuration, controlled only by the operator.
    pub sandbox: super::BashSandboxConfig,
}

impl BashPolicy {
    /// Rejects settings that could not run, before they are persisted.
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
        Ok(())
    }

    /// Whether `caller` may run Bash: the tool is enabled and the caller is an explicitly listed
    /// administrator. A turn without a caller is always denied.
    pub fn allows(&self, caller: Option<&str>, commands: &CommandPolicy) -> bool {
        self.enabled
            && caller.is_some_and(|caller| commands.admins.iter().any(|admin| admin == caller))
    }
}

/// Live settings shared by the management API, availability hint and execution gate.
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
}
