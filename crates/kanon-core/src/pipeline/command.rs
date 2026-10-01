//! Slash command extraction, routing, and dispatching.
//!
//! Inspects inbound message text for slash command prefixes (e.g. `/rustcalc <expr>`),
//! resolves target plugins via metadata discovered during the `GetPluginMeta` handshake,
//! and dispatches execution to [`ManagedHost::execute_command`].

use std::sync::Arc;

use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, CommandMeta, PipelineEventRequest,
};

use crate::supervisor::ManagedHost;

/// Resolved target destination for an executed slash command.
#[derive(Clone)]
pub struct MatchedCommand {
    /// Host process managing the plugin.
    pub host: Arc<ManagedHost>,
    /// Unique identifier of the target plugin.
    pub plugin_id: String,
    /// Command metadata discovered during initial handshake.
    pub meta: CommandMeta,
}

/// Router responsible for parsing and matching slash commands to registered plugin hosts.
pub struct CommandRouter;

impl CommandRouter {
    /// Parses an incoming text string to extract a slash command name and argument list.
    ///
    /// Returns `Some((command_name, arguments))` if the string starts with a slash `/`,
    /// or `None` if the text represents plain conversation or an empty command.
    ///
    /// # Examples
    /// - `"/rustcalc 2 + 2"` -> `Some(("rustcalc", vec!["2", "+", "2"]))`
    /// - `"   /weather beijing  "` -> `Some(("weather", vec!["beijing"]))`
    /// - `"/help"` -> `Some(("help", vec![]))`
    /// - `"hello kanon"` -> `None`
    /// - `"/"` -> `None`
    pub fn parse_command(text: &str) -> Option<(String, Vec<String>)> {
        let trimmed = text.trim();
        let stripped = trimmed.strip_prefix('/')?;
        let mut tokens = stripped.split_whitespace();
        let name = tokens.next()?;
        if name.is_empty() {
            return None;
        }

        let args = tokens.map(String::from).collect();
        Some((name.to_string(), args))
    }

    /// Resolves a parsed command name to a matching plugin host and command metadata.
    ///
    /// Inspects the metadata registered on each active [`ManagedHost`].
    /// If multiple plugins register identical command names, candidates are sorted
    /// by command priority ascending, host priority ascending, and finally `host_id`
    /// to guarantee deterministic routing.
    pub fn resolve(command_name: &str, hosts: &[Arc<ManagedHost>]) -> Option<MatchedCommand> {
        let target_name = command_name.trim_start_matches('/');
        let mut candidates = Vec::new();

        for host in hosts {
            for plugin in host.metas() {
                for cmd in &plugin.commands {
                    if cmd.name.trim_start_matches('/') == target_name {
                        candidates.push((
                            cmd.priority,
                            host.priority,
                            &host.host_id,
                            host.clone(),
                            plugin.id.clone(),
                            cmd.clone(),
                        ));
                    }
                }
            }
        }

        // Sort candidates: lowest numerical priority value wins.
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(b.2))
        });

        candidates
            .into_iter()
            .next()
            .map(|(_, _, _, host, plugin_id, meta)| MatchedCommand {
                host,
                plugin_id,
                meta,
            })
    }

    /// Dispatches a command execution request to the target plugin host.
    #[allow(clippy::result_large_err)]
    pub async fn dispatch(
        target: &MatchedCommand,
        args: Vec<String>,
        context: PipelineEventRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let req = CommandExecuteRequest {
            plugin_id: target.plugin_id.clone(),
            command: target.meta.name.clone(),
            args,
            context: Some(context),
        };

        target.host.execute_command(req).await
    }
}
