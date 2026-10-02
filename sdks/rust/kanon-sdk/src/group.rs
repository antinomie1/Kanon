//! Command groups: one slash command with subcommands (`/admin ban <user>`, `/admin kick ..`),
//! declared with [`Router::command_group`](crate::router::Router::command_group).
//!
//! The core routes the whole command to the group (it knows subcommands only for help
//! listings), so the SDK dispatches on the first argument. `/admin` alone, an unknown
//! subcommand, and a continuation no suspended handler is waiting for all answer with the
//! group's help.

use std::future::Future;
use std::sync::Arc;

use kanon_proto::v1::{CommandAccess, CommandMeta, MessageSegment};

use crate::event::CommandEvent;
use crate::plugin::PluginResult;
use crate::router::{CommandHandler, CommandSpec, box_command};
use crate::segment::IntoReply;

/// The subcommands of a group, built inside [`Router::command_group`]'s closure.
///
/// [`Router::command_group`]: crate::router::Router::command_group
pub struct CommandGroup {
    name: String,
    subcommands: Vec<(CommandMeta, CommandHandler)>,
}

impl CommandGroup {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subcommands: Vec::new(),
        }
    }

    /// Declares subcommand `spec` (a name, or a [`CommandSpec`] with description, usage and
    /// aliases). Its handler sees `args`/`raw_args` without the subcommand's word, and
    /// [`CommandEvent::subcommand`] names it.
    ///
    /// Who may run a subcommand, and where, is decided by the group's own spec: the core never
    /// sees subcommands.
    ///
    /// # Panics
    /// If the name or an alias is taken by another subcommand, or `spec` sets access, platforms
    /// or conversation kinds (they would silently not apply — set them on the group).
    pub fn command<F, Fut, R>(mut self, spec: impl Into<CommandSpec>, handler: F) -> Self
    where
        F: Fn(CommandEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoReply + 'static,
    {
        let mut meta = spec.into().into_meta();
        let group = &self.name;
        let sub = meta.name.clone();
        assert!(
            !sub.is_empty() && !sub.contains(char::is_whitespace),
            "subcommand '{sub}' of '/{group}' must be one word"
        );
        assert!(
            meta.access == CommandAccess::Everyone as i32
                && meta.platforms.is_empty()
                && meta.conversation_kinds.is_empty(),
            "subcommand '{sub}' of '/{group}': access, platforms and conversation kinds apply to the whole group; set them on the group's CommandSpec"
        );
        for word in std::iter::once(&meta.name).chain(&meta.aliases) {
            assert!(
                self.find(word).is_none(),
                "subcommand '{word}' of '/{group}' is already declared"
            );
        }
        if meta.usage == format!("/{sub}") {
            meta.usage = format!("/{group} {sub}");
        }
        self.subcommands.push((meta, box_command(handler)));
        self
    }

    /// The subcommand `word` names, by name or alias.
    fn find(&self, word: &str) -> Option<&(CommandMeta, CommandHandler)> {
        self.subcommands
            .iter()
            .find(|(meta, _)| meta.name == word || meta.aliases.iter().any(|alias| alias == word))
    }

    /// The subcommands' metadata, for `CommandMeta.subcommands`.
    pub(crate) fn metas(&self) -> Vec<CommandMeta> {
        self.subcommands
            .iter()
            .map(|(meta, _)| meta.clone())
            .collect()
    }

    /// The handler the router registers for the group's name: it dispatches to subcommands.
    pub(crate) fn into_handler(self, group: CommandMeta) -> CommandHandler {
        let table = Arc::new(Dispatch {
            group,
            commands: self,
        });
        Arc::new(move |event| {
            let table = table.clone();
            Box::pin(async move { table.dispatch(event).await })
        })
    }
}

/// A group's metadata and subcommands, shared by every invocation.
struct Dispatch {
    group: CommandMeta,
    commands: CommandGroup,
}

impl Dispatch {
    async fn dispatch(&self, event: CommandEvent) -> PluginResult<Vec<MessageSegment>> {
        // A continuation carries the sender's whole next message, not a subcommand; reaching
        // here means the handler that asked for it is gone (e.g. the host restarted), and which
        // subcommand that was is unknown.
        if event.continuation() {
            return Ok(self.help(None).into_segments());
        }
        let Some(word) = event.args().first() else {
            return Ok(self.help(None).into_segments());
        };
        match self.commands.find(word) {
            Some((meta, handler)) => handler(event.for_subcommand(&meta.name, word)).await,
            None => Ok(self.help(Some(word)).into_segments()),
        }
    }

    /// The group's help: its usage and one line per subcommand, after a note naming an
    /// unknown subcommand when there was one.
    fn help(&self, unknown: Option<&str>) -> String {
        let mut lines = Vec::new();
        if let Some(word) = unknown {
            lines.push(format!("Unknown subcommand: {word}"));
        }
        lines.push(line(&self.group.usage, &self.group.description));
        for (meta, _) in &self.commands.subcommands {
            lines.push(format!("  {}", line(&meta.usage, &meta.description)));
        }
        lines.join("\n")
    }
}

fn line(usage: &str, description: &str) -> String {
    if description.is_empty() {
        usage.to_string()
    } else {
        format!("{usage} — {description}")
    }
}
