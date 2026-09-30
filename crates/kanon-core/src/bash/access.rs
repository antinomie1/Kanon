//! Who may run Bash, and the operator settings that choose how it runs.
//!
//! Bash is granted to explicitly listed administrators — the same `<platform>:<user id>` entries
//! of [`CommandPolicy::admins`](crate::access::CommandPolicy::admins) that unlock restricted
//! commands, taken from the command policy of the instance serving the turn (its own override, or
//! the node's). Group owners and admins as reported by a platform never qualify: they are chosen
//! by the group, not by the operator, and a shell on the node is not something a group may hand
//! out. The identity comes from the inbound event, never from model arguments, message text or a
//! session name.
//!
//! Every decision goes through [`Gate::check`], so the availability hint, the first check of a
//! call and the rechecks after queueing or review can never disagree.

use std::future::Future;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::access::CommandPolicyStore;
use crate::instance::{BashScope, InstanceRegistry};

tokio::task_local! {
    // Task scope keeps overlapping turns in one group from borrowing each other's rights. `None`
    // explicitly shadows any outer scope for turns that have no individual sender.
    static CALLER: Option<BashCaller>;
}

/// The verified sender of one turn, as the pipeline established it before the model ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashCaller {
    /// `<platform>:<user id>` exactly as the adapter reported the sender.
    pub id: String,
    /// Instance serving the turn. Its command policy lists the administrators and its
    /// [`BashScope`] decides where they may run Bash; `None` uses the node-wide command policy.
    pub instance: Option<String>,
    /// The turn's context also carries other group members' words (a shared or observed group
    /// session), so only an instance that explicitly allows it lets the turn run Bash.
    pub shared_context: bool,
}

impl BashCaller {
    /// A caller in its own conversation, governed by the node-wide command policy.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            instance: None,
            shared_context: false,
        }
    }
}

/// Runs one turn with its verified caller, or explicitly without one.
///
/// A turn without a caller can never run Bash; that is what a notice or the console chat gets.
pub async fn with_bash_caller<F: Future>(caller: Option<BashCaller>, turn: F) -> F::Output {
    CALLER.scope(caller, turn).await
}

/// Returns the caller of the current turn, when it has one.
pub(super) fn current_caller() -> Option<BashCaller> {
    CALLER.try_with(Clone::clone).ok().flatten()
}

/// The live stores that together decide whether a caller may run Bash.
pub(super) struct Gate {
    /// Node-wide switch and execution backend.
    pub policy: Arc<BashPolicyStore>,
    /// Node-wide command policy, used by instances without their own.
    pub commands: Arc<CommandPolicyStore>,
    /// Instance catalog holding per-instance command policies and Bash scopes.
    pub instances: Arc<InstanceRegistry>,
}

impl Gate {
    /// Why `caller` may not run Bash right now; `Ok` when it may.
    ///
    /// The reason is phrased for the model (it ends up in the availability hint and in tool
    /// errors), so it names the setting that decides instead of a generic refusal.
    pub async fn check(&self, caller: Option<&BashCaller>) -> Result<(), String> {
        if !self.policy.get().enabled {
            return Err("disabled by the operator".into());
        }
        let Some(caller) = caller else {
            return Err("this turn has no single verified sender".into());
        };
        let commands = match &caller.instance {
            None => self.commands.get(),
            Some(id) => {
                // A deleted instance fails closed: its administrators are no longer defined.
                let instance = self
                    .instances
                    .get(id)
                    .await
                    .ok_or("the bot instance serving this conversation no longer exists")?;
                match instance.bash {
                    BashScope::Disabled => {
                        return Err("disabled for this bot instance".into());
                    }
                    BashScope::OwnContext if caller.shared_context => {
                        return Err("this group conversation also carries other members' \
                            messages (shared or observed group session), and this bot instance \
                            allows Bash only in an administrator's own conversation"
                            .into());
                    }
                    _ => {}
                }
                instance.effective_command_policy(self.commands.get())
            }
        };
        if !commands.admins.iter().any(|admin| *admin == caller.id) {
            return Err("the current sender is not an authorized administrator of this bot".into());
        }
        Ok(())
    }
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
