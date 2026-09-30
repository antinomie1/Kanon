//! Who may run which command, and who the bot's administrators are.
//!
//! Some commands change state other people depend on: `/model` switches the model of the whole
//! instance, and `/new` in a shared group session wipes what everyone in the group built up. The
//! node-wide [`CommandPolicy`] names administrators and gives every command an access level;
//! a command it does not list is open to everyone, so plugin commands keep working unless an
//! operator restricts them.
//!
//! Administrators are either listed explicitly, as `<platform>:<user id>` exactly as the sender
//! appears in Kanon, or — when the operator allows it — group owners and admins as reported by an
//! adapter with the `sender_role` capability.

use std::collections::BTreeMap;

use kanon_proto::v1::PipelineEventRequest;
use serde::{Deserialize, Serialize};

use crate::conversation::{ConversationKind, default_true};
use crate::notice::metadata_str;

/// Metadata key carrying the sender's display name (group card or nickname).
pub const META_SENDER_NAME: &str = "kanon.sender_name";

/// Metadata key carrying the sender's group role: `owner`, `admin` or `member`.
pub const META_SENDER_ROLE: &str = "kanon.sender_role";

/// Who may run a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAccess {
    /// Anyone.
    Everyone,
    /// Anyone in a private chat; only administrators in groups and channels.
    AdminsInGroups,
    /// Only administrators, everywhere.
    Admins,
}

/// Node-wide command permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPolicy {
    /// Bot administrators, as `<platform>:<user id>`.
    #[serde(default)]
    pub admins: Vec<String>,
    /// Treat group owners and admins (as the platform reports them) as bot administrators in
    /// their own group.
    #[serde(default = "default_true")]
    pub group_admins_are_admins: bool,
    /// Access level per command name (without the slash); unlisted commands are open to everyone.
    #[serde(default = "default_access")]
    pub access: BTreeMap<String, CommandAccess>,
}

/// Restrictive defaults for the built-in commands that affect other people.
fn default_access() -> BTreeMap<String, CommandAccess> {
    BTreeMap::from([
        ("new".to_string(), CommandAccess::AdminsInGroups),
        ("model".to_string(), CommandAccess::Admins),
    ])
}

impl Default for CommandPolicy {
    fn default() -> Self {
        Self {
            admins: Vec::new(),
            group_admins_are_admins: true,
            access: default_access(),
        }
    }
}

impl CommandPolicy {
    /// Normalizes names and rejects malformed entries before anything is applied or saved.
    ///
    /// Command names lose a leading `/` and are lowercased, because commands match
    /// case-insensitively; an administrator must name both a platform and a user.
    pub fn prepare(mut self) -> Result<Self, String> {
        let mut admins = Vec::with_capacity(self.admins.len());
        for admin in &self.admins {
            let admin = admin.trim();
            match admin.split_once(':') {
                Some((platform, user)) if !platform.is_empty() && !user.is_empty() => {
                    if !admins.iter().any(|known: &String| known == admin) {
                        admins.push(admin.to_string());
                    }
                }
                _ => {
                    return Err(format!(
                        "administrator '{admin}' must be written as <platform>:<user id>"
                    ));
                }
            }
        }
        self.admins = admins;
        let mut access = BTreeMap::new();
        for (command, level) in self.access {
            let command = command.trim().trim_start_matches('/').to_ascii_lowercase();
            if command.is_empty() || command.chars().any(char::is_whitespace) {
                return Err(format!("'{command}' is not a command name"));
            }
            access.insert(command, level);
        }
        self.access = access;
        Ok(self)
    }

    /// Whether the sender of `event` is a bot administrator.
    pub fn is_admin(&self, event: &PipelineEventRequest) -> bool {
        let id = format!("{}:{}", event.platform, event.sender_id);
        if self.admins.iter().any(|admin| *admin == id) {
            return true;
        }
        self.group_admins_are_admins
            && ConversationKind::from_metadata(event.metadata.as_ref()).is_policy_governed()
            && matches!(
                metadata_str(event.metadata.as_ref(), META_SENDER_ROLE),
                Some("owner" | "admin")
            )
    }

    /// Whether the sender of `event` may run `command`.
    pub fn allows(&self, command: &str, event: &PipelineEventRequest) -> bool {
        match self.access.get(&command.to_ascii_lowercase()) {
            None | Some(CommandAccess::Everyone) => true,
            Some(CommandAccess::AdminsInGroups) => {
                !ConversationKind::from_metadata(event.metadata.as_ref()).is_policy_governed()
                    || self.is_admin(event)
            }
            Some(CommandAccess::Admins) => self.is_admin(event),
        }
    }
}

/// Hot-swappable node-wide command policy, mirroring the other policy stores.
#[derive(Debug, Default)]
pub struct CommandPolicyStore {
    current: std::sync::RwLock<CommandPolicy>,
}

impl CommandPolicyStore {
    /// Creates a store holding an initial policy.
    pub fn new(policy: CommandPolicy) -> Self {
        Self {
            current: std::sync::RwLock::new(policy),
        }
    }

    /// Returns the current policy.
    pub fn get(&self) -> CommandPolicy {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Replaces the policy.
    pub fn set(&self, policy: CommandPolicy) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }
}
