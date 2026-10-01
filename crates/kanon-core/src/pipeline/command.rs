//! Slash command extraction, routing, and dispatching.
//!
//! Inspects inbound message text for slash command prefixes (e.g. `/rustcalc <expr>`),
//! resolves target plugins via metadata discovered during the `GetPluginMeta` handshake,
//! and dispatches execution to [`ManagedHost::execute_command`]. Plugin triggers — regular
//! expressions matched against plain messages — are resolved here as well and dispatched through
//! the same RPC, so a plugin handles both with one handler registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, CommandMeta, PipelineEventRequest, TriggerMeta,
};
use regex::Regex;

use crate::access::CommandAccess;
use crate::conversation::ConversationKind;
use crate::pipeline::capture::Capture;
use crate::supervisor::ManagedHost;

/// A slash command split into its name and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Command name without the slash, exactly as typed (possibly an alias).
    pub name: String,
    /// Arguments split on whitespace, with quoted text kept together.
    pub args: Vec<String>,
    /// Everything after the command name, trimmed but otherwise untouched.
    pub raw_args: String,
}

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

impl MatchedCommand {
    /// Canonical command name without the slash, used for routing and for the command policy.
    pub fn name(&self) -> &str {
        self.meta.name.trim_start_matches('/')
    }

    /// Access level the plugin declared for this command.
    pub fn access(&self) -> CommandAccess {
        CommandAccess::from_proto(self.meta.access())
    }
}

/// A trigger that matched a message, with its capture groups.
#[derive(Clone)]
pub struct MatchedTrigger {
    /// Host process managing the plugin.
    pub host: Arc<ManagedHost>,
    /// Unique identifier of the target plugin.
    pub plugin_id: String,
    /// Trigger metadata discovered during the handshake.
    pub meta: TriggerMeta,
    /// Capture groups 1..n; a group that did not participate in the match is empty.
    pub captures: Vec<String>,
}

impl MatchedTrigger {
    /// Access level the plugin declared for this trigger.
    pub fn access(&self) -> CommandAccess {
        CommandAccess::from_proto(self.meta.access())
    }
}

/// Router responsible for parsing and matching slash commands to registered plugin hosts.
pub struct CommandRouter;

impl CommandRouter {
    /// Parses an incoming text string into a slash command.
    ///
    /// Returns `None` if the text does not start with `/` or names no command.
    ///
    /// # Examples
    /// - `"/rustcalc 2 + 2"` -> name `rustcalc`, args `["2", "+", "2"]`
    /// - `"/say \"hello world\" now"` -> name `say`, args `["hello world", "now"]`
    /// - `"hello kanon"` -> `None`
    /// - `"/"` -> `None`
    pub fn parse_command(text: &str) -> Option<ParsedCommand> {
        let stripped = text.trim().strip_prefix('/')?;
        let name_end = stripped.find(char::is_whitespace).unwrap_or(stripped.len());
        let name = &stripped[..name_end];
        if name.is_empty() {
            return None;
        }

        let raw_args = stripped[name_end..].trim();
        Some(ParsedCommand {
            name: name.to_string(),
            args: split_args(raw_args),
            raw_args: raw_args.to_string(),
        })
    }

    /// Resolves a parsed command name (or one of its aliases) to a plugin host.
    ///
    /// If multiple plugins register identical command names, candidates are sorted
    /// by command priority ascending, host priority ascending, and finally `host_id`
    /// to guarantee deterministic routing. A canonical name and an alias compete on equal terms:
    /// the priority, not the kind of match, decides.
    ///
    /// A command whose declared platforms or conversation kinds exclude `event` is not a
    /// candidate at all, so another plugin's command of the same name may answer instead.
    pub fn resolve(
        command_name: &str,
        hosts: &[Arc<ManagedHost>],
        event: &PipelineEventRequest,
    ) -> Option<MatchedCommand> {
        let target_name = command_name.trim_start_matches('/');
        let mut candidates = Vec::new();

        for host in hosts {
            for plugin in host.metas() {
                for cmd in &plugin.commands {
                    let names = std::iter::once(&cmd.name).chain(cmd.aliases.iter());
                    if names
                        .map(|name| name.trim_start_matches('/'))
                        .any(|name| name == target_name)
                        && in_scope(&cmd.platforms, &cmd.conversation_kinds, event)
                    {
                        candidates.push((
                            cmd.priority,
                            host.priority,
                            host.host_id.clone(),
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
                .then_with(|| a.2.cmp(&b.2))
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
        parsed: ParsedCommand,
        context: PipelineEventRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let req = CommandExecuteRequest {
            plugin_id: target.plugin_id.clone(),
            command: target.name().to_string(),
            args: parsed.args,
            context: Some(context),
            raw_args: parsed.raw_args,
            continuation: false,
        };

        target.host.execute_command(req).await
    }

    /// Dispatches a matched trigger to its plugin through `OnExecuteCommand`.
    #[allow(clippy::result_large_err)]
    pub async fn dispatch_trigger(
        target: &MatchedTrigger,
        text: &str,
        context: PipelineEventRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let req = CommandExecuteRequest {
            plugin_id: target.plugin_id.clone(),
            command: target.meta.name.clone(),
            args: target.captures.clone(),
            context: Some(context),
            raw_args: text.to_string(),
            continuation: false,
        };

        target.host.execute_command(req).await
    }

    /// Delivers a captured sender's next message to the plugin that captured it.
    ///
    /// The whole message is the argument text: the plugin asked a question and the message is
    /// the answer, so nothing (not even a leading slash) is stripped from it.
    #[allow(clippy::result_large_err)]
    pub async fn dispatch_continuation(
        host: &ManagedHost,
        capture: &Capture,
        text: &str,
        context: PipelineEventRequest,
    ) -> Result<CommandExecuteResponse, tonic::Status> {
        let text = text.trim();
        let req = CommandExecuteRequest {
            plugin_id: capture.plugin_id.clone(),
            command: capture.command.clone(),
            args: split_args(text),
            context: Some(context),
            raw_args: text.to_string(),
            continuation: true,
        };
        host.execute_command(req).await
    }
}

/// Whether a command or trigger limited to `platforms` and `kinds` applies to `event`.
///
/// An empty list places no limit. The conversation kind is read the same way the reply policy
/// reads it, so an adapter that reports no kind is treated as a private chat everywhere.
fn in_scope(platforms: &[String], kinds: &[i32], event: &PipelineEventRequest) -> bool {
    use kanon_proto::v1::ConversationKind as Declared;

    let kind = match ConversationKind::from_metadata(event.metadata.as_ref()) {
        ConversationKind::Private => Declared::Private,
        ConversationKind::Group => Declared::Group,
        ConversationKind::Channel => Declared::Channel,
    } as i32;
    (platforms.is_empty() || platforms.iter().any(|platform| *platform == event.platform))
        && (kinds.is_empty() || kinds.contains(&kind))
}

/// Splits command arguments on whitespace, keeping quoted text together.
///
/// `"…"`, `'…'` and the full-width `“…”` (which Chinese input methods produce for a typed `"`)
/// group their content into one argument, and the quotes themselves are dropped. A quote that is
/// never closed extends to the end of the text: users routinely forget the closing quote, and
/// taking the rest literally is more predictable than rejecting the whole command.
pub fn split_args(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    // Whether `current` holds an argument, so `""` still yields an empty argument.
    let mut started = false;
    let mut closing: Option<char> = None;

    for ch in raw.chars() {
        match closing {
            Some(close) if ch == close => closing = None,
            Some(_) => current.push(ch),
            None => match ch {
                '"' | '\'' => {
                    closing = Some(ch);
                    started = true;
                }
                '“' => {
                    closing = Some('”');
                    started = true;
                }
                ch if ch.is_whitespace() => {
                    if started {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                ch => {
                    current.push(ch);
                    started = true;
                }
            },
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// Compiled trigger patterns, keyed by their source.
///
/// Patterns come from plugin metadata, which can change on every configuration reload, so they
/// are compiled lazily and remembered; an invalid pattern is remembered as `None` so it is
/// reported once instead of on every message.
#[derive(Debug, Default)]
pub struct TriggerMatcher {
    compiled: Mutex<HashMap<String, Option<Regex>>>,
}

/// Upper bound on remembered patterns; plugins never declare more than a handful, so reaching it
/// means patterns are churning and the cache is simply rebuilt.
const TRIGGER_CACHE_LIMIT: usize = 512;

impl TriggerMatcher {
    /// Finds the first trigger (lowest priority, then host priority, then `host_id`) matching
    /// the text of `event` whose access level `allowed` accepts.
    ///
    /// `text` is the message text with leading mentions removed. Triggers whose declared
    /// platforms or conversation kinds exclude `event` are never tried.
    pub fn resolve(
        &self,
        text: &str,
        event: &PipelineEventRequest,
        hosts: &[Arc<ManagedHost>],
        allowed: impl Fn(&TriggerMeta) -> bool,
    ) -> Option<MatchedTrigger> {
        let mut candidates: Vec<(i32, i32, String, Arc<ManagedHost>, String, TriggerMeta)> =
            Vec::new();
        for host in hosts {
            for plugin in host.metas() {
                for trigger in plugin.triggers {
                    if !in_scope(&trigger.platforms, &trigger.conversation_kinds, event) {
                        continue;
                    }
                    candidates.push((
                        trigger.priority,
                        host.priority,
                        host.host_id.clone(),
                        host.clone(),
                        plugin.id.clone(),
                        trigger,
                    ));
                }
            }
        }
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });

        for (_, _, _, host, plugin_id, meta) in candidates {
            let Some(captures) = self.captures(&meta, &plugin_id, text) else {
                continue;
            };
            if !allowed(&meta) {
                tracing::debug!(
                    trigger = %meta.name,
                    plugin_id = %plugin_id,
                    "Trigger matched but the sender may not fire it; skipping"
                );
                continue;
            }
            return Some(MatchedTrigger {
                host,
                plugin_id,
                meta,
                captures,
            });
        }
        None
    }

    /// Matches one trigger, returning its capture groups when it matches.
    fn captures(&self, meta: &TriggerMeta, plugin_id: &str, text: &str) -> Option<Vec<String>> {
        let regex = {
            let mut compiled = self
                .compiled
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if compiled.len() >= TRIGGER_CACHE_LIMIT && !compiled.contains_key(&meta.pattern) {
                compiled.clear();
            }
            compiled
                .entry(meta.pattern.clone())
                .or_insert_with(|| match Regex::new(&meta.pattern) {
                    Ok(regex) => Some(regex),
                    Err(err) => {
                        tracing::warn!(
                            trigger = %meta.name,
                            plugin_id = %plugin_id,
                            error = %err,
                            "Trigger pattern is not a valid regular expression; it never matches"
                        );
                        None
                    }
                })
                .clone()?
        };

        let captures = regex.captures(text)?;
        Some(
            captures
                .iter()
                .skip(1)
                .map(|group| group.map_or_else(String::new, |m| m.as_str().to_string()))
                .collect(),
        )
    }
}
