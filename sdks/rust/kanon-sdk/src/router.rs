//! [`Router`]: a [`Plugin`] assembled from closures, so a plugin declares each command, trigger,
//! tool and hook once — next to its handler — instead of matching names by hand in trait
//! methods and repeating them in [`PluginMeta`].
//!
//! ```ignore
//! /// Arguments of `get_weather`; doc comments become the schema's descriptions.
//! #[derive(Deserialize, JsonSchema)]
//! struct WeatherArgs {
//!     /// City name, e.g. "Paris".
//!     city: String,
//! }
//!
//! let plugin = Router::new("org.example.echo", "Echo", "0.1.0")
//!     .command("echo", |event| async move { Ok(event.raw_args().to_string()) })
//!     .command_group(CommandSpec::new("admin").description("Moderation"), |group| {
//!         group.command("ban", |event| async move { Ok(format!("banned {}", event.raw_args())) })
//!     })
//!     .trigger(TriggerSpec::new("ping", "^ping$"), |event| async move {
//!         Ok(vec![segment::quote(event.event_id()), segment::text("pong")])
//!     })
//!     .tool(
//!         ToolSpec::typed::<WeatherArgs>("get_weather").description("Get the forecast"),
//!         |args, _event| async move { Ok(json!({ "city": args.city, "sky": "clear" })) },
//!     )
//!     .http_route("GET", "/status", |_request| async move { Ok(json!({ "ok": true })) });
//! KanonHost::new(plugin).run().await?;
//! ```
//!
//! Handlers are `'static` closures; share state by capturing an `Arc`. The context handed to
//! `on_load` (data directory, configuration, Core handle) is available through
//! [`Router::context`], which also adds and removes tools at runtime.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kanon_proto::v1::{
    CommandAccess, CommandExecuteRequest, CommandExecuteResponse, CommandMeta, ConversationKind,
    DecorateReplyRequest, EventKind, EventNotification, HttpRequest, HttpResponse,
    LlmRequestHookRequest, LlmResponseEvent, MessageSegment, MessageSentEvent,
    PipelineEventRequest, PluginMeta, PreFilterResult, PrepareTurnRequest, ReplySource,
    ToolAttachment, ToolCallRequest, ToolCallResponse, ToolMeta, TriggerMeta, event_notification,
    tool_call_request, tool_call_response,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::context::{CoreHandle, PluginContext};
use crate::error::CoreError;
use crate::event::{
    AgentBegin, AgentDone, CommandEvent, Conversations, MessageEvent, Session, ToolCall, ToolResult,
};
use crate::group::CommandGroup;
use crate::http::{IntoResponse, Request, Routes};
use crate::plugin::{Plugin, PluginResult};
use crate::segment::IntoReply;

/// A boxed, sendable future, the return type of stored handlers.
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A stored command, trigger or subcommand handler.
pub(crate) type CommandHandler =
    Arc<dyn Fn(CommandEvent) -> BoxFuture<PluginResult<Vec<MessageSegment>>> + Send + Sync>;
/// A stored tool handler: the model's arguments and the message being answered in, the JSON
/// result and the attachments out.
type ToolHandler = Arc<
    dyn Fn(
            serde_json::Value,
            Option<MessageEvent>,
        ) -> BoxFuture<PluginResult<(serde_json::Value, Vec<ToolAttachment>)>>
        + Send
        + Sync,
>;
type ActionHandler =
    Arc<dyn Fn(serde_json::Value) -> BoxFuture<PluginResult<serde_json::Value>> + Send + Sync>;
type EventHandler = Arc<dyn Fn(Event) -> BoxFuture<PluginResult<()>> + Send + Sync>;
type DecorateHandler =
    Arc<dyn Fn(Reply) -> BoxFuture<PluginResult<Option<Vec<MessageSegment>>>> + Send + Sync>;
type PrepareHandler =
    Arc<dyn Fn(MessageEvent, String) -> BoxFuture<PluginResult<String>> + Send + Sync>;
type PromptHandler =
    Arc<dyn Fn(SystemPrompt) -> BoxFuture<PluginResult<Option<String>>> + Send + Sync>;
type PreFilterHandler =
    Arc<dyn Fn(MessageEvent) -> BoxFuture<PluginResult<Option<PreFilterResult>>> + Send + Sync>;

/// Declaration of a slash command. A plain name converts into one: `.command("echo", ..)`.
#[derive(Debug, Clone)]
pub struct CommandSpec(CommandMeta);

impl CommandSpec {
    /// A command named `name` (without the slash), usable by everyone, priority 500.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into().trim_start_matches('/').to_string();
        Self(CommandMeta {
            usage: format!("/{name}"),
            name,
            priority: 500,
            ..Default::default()
        })
    }

    /// One line shown by `/help`.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.0.description = description.into();
        self
    }

    /// Usage example shown by `/help`.
    pub fn usage(mut self, usage: impl Into<String>) -> Self {
        self.0.usage = usage.into();
        self
    }

    /// Lower wins when several plugins declare the same name.
    pub fn priority(mut self, priority: i32) -> Self {
        self.0.priority = priority;
        self
    }

    /// Another name that invokes the command; the handler still sees the canonical name.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.0
            .aliases
            .push(alias.into().trim_start_matches('/').to_string());
        self
    }

    /// Default access level; the operator's command policy overrides it.
    pub fn access(mut self, access: CommandAccess) -> Self {
        self.0.access = access as i32;
        self
    }

    /// Limits the command to `platform`; call repeatedly to allow several. Without any, every
    /// platform is allowed. Elsewhere Core treats the command as undeclared.
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.0.platforms.push(platform.into());
        self
    }

    /// Limits the command to one kind of conversation; call repeatedly to allow several. Without
    /// any, every kind is allowed.
    pub fn conversation_kind(mut self, kind: ConversationKind) -> Self {
        self.0.conversation_kinds.push(kind as i32);
        self
    }

    pub(crate) fn into_meta(self) -> CommandMeta {
        self.0
    }
}

impl From<&str> for CommandSpec {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

impl From<String> for CommandSpec {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

/// Declaration of a regular-expression trigger on plain messages.
///
/// Core matches the pattern (Rust `regex` syntax) against the message text; the handler's
/// `event.args()` holds the capture groups, with `""` for a group that did not participate.
/// Triggers run after slash commands and before the model.
#[derive(Debug, Clone)]
pub struct TriggerSpec(TriggerMeta);

impl TriggerSpec {
    /// A trigger named `name` (for routing and logs) matching `pattern`.
    pub fn new(name: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self(TriggerMeta {
            name: name.into(),
            pattern: pattern.into(),
            priority: 500,
            ..Default::default()
        })
    }

    /// Shown in `/help`; leave empty to keep the trigger unlisted.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.0.description = description.into();
        self
    }

    /// Lower wins when several triggers match.
    pub fn priority(mut self, priority: i32) -> Self {
        self.0.priority = priority;
        self
    }

    /// Default access level; the operator's command policy overrides it.
    pub fn access(mut self, access: CommandAccess) -> Self {
        self.0.access = access as i32;
        self
    }

    /// Limits the trigger to `platform`; call repeatedly to allow several. Without any, every
    /// platform is allowed. Elsewhere Core treats the trigger as undeclared.
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.0.platforms.push(platform.into());
        self
    }

    /// Limits the trigger to one kind of conversation; call repeatedly to allow several. Without
    /// any, every kind is allowed.
    pub fn conversation_kind(mut self, kind: ConversationKind) -> Self {
        self.0.conversation_kinds.push(kind as i32);
        self
    }
}

/// Declaration of a tool the model may call, whose handler receives arguments of type `A`.
///
/// - [`ToolSpec::typed`] infers the JSON Schema from a `#[derive(Deserialize, JsonSchema)]`
///   struct and hands the handler that struct;
/// - [`ToolSpec::new`] takes an explicit schema ([`parameters`](ToolSpec::parameters)) and
///   hands the handler the raw JSON object.
pub struct ToolSpec<A = serde_json::Value> {
    meta: ToolMeta,
    /// Turns the model's JSON arguments into the handler's argument type.
    parse: fn(serde_json::Value) -> Result<A, serde_json::Error>,
}

impl<A> Clone for ToolSpec<A> {
    fn clone(&self) -> Self {
        Self {
            meta: self.meta.clone(),
            parse: self.parse,
        }
    }
}

impl<A> std::fmt::Debug for ToolSpec<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ToolSpec").field(&self.meta).finish()
    }
}

impl ToolSpec {
    /// A tool named `name` whose handler receives the arguments as a JSON object; declare their
    /// schema with [`parameters`](Self::parameters).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            meta: ToolMeta {
                name: name.into(),
                ..Default::default()
            },
            parse: Ok,
        }
    }

    /// A tool named `name` whose arguments deserialize into `A`; the parameters schema is
    /// generated from `A` (see [`crate::schema::tool_parameters`]) and doc comments on its
    /// fields become their descriptions.
    ///
    /// Arguments the model gets wrong (a missing field, a wrong type) are reported back to it as
    /// a failed call naming the problem, so it can correct itself; the handler never runs.
    ///
    /// # Panics
    /// If `A` is not a struct with named fields.
    pub fn typed<A: DeserializeOwned + JsonSchema>(name: impl Into<String>) -> ToolSpec<A> {
        let serde_json::Value::Object(schema) = crate::schema::tool_parameters::<A>() else {
            unreachable!("tool_parameters always returns an object");
        };
        ToolSpec {
            meta: ToolMeta {
                name: name.into(),
                parameters: Some(crate::json::to_struct(schema)),
                ..Default::default()
            },
            parse: parse_typed::<A>,
        }
    }

    /// JSON Schema of the arguments; must be a JSON object.
    ///
    /// # Panics
    /// If `schema` is not an object — a schema mistake should stop the plugin at startup.
    pub fn parameters(mut self, schema: serde_json::Value) -> Self {
        let serde_json::Value::Object(fields) = schema else {
            panic!(
                "tool '{}': parameters must be a JSON object",
                self.meta.name
            );
        };
        self.meta.parameters = Some(crate::json::to_struct(fields));
        self
    }
}

impl<A> ToolSpec<A> {
    /// What the tool does, for the model.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.meta.description = description.into();
        self
    }
}

/// A tool result that also hands media to the user: the `value` is what the model reads, the
/// attachments (pictures, files) go out with the turn's reply. Return it from a tool handler in
/// place of the plain value:
///
/// ```ignore
/// router.tool(ToolSpec::typed::<ChartArgs>("chart"), |args, _event| async move {
///     let path = draw_chart(&args)?;
///     Ok(ToolReply::new("Chart drawn and sent to the user.").file(path, "image/png"))
/// })
/// ```
///
/// The core sends an attachment as the kind its MIME type names (image, voice, video, otherwise
/// a file), keeps attachment paths out of what the model reads, and drops the attachments of a
/// failed call.
#[derive(Debug, Clone)]
pub struct ToolReply<T = serde_json::Value> {
    value: T,
    attachments: Vec<Attachment>,
}

/// Where one attachment of a [`ToolReply`] is.
#[derive(Debug, Clone)]
enum Attachment {
    File { path: PathBuf, mime_type: String },
    Url { url: String, mime_type: String },
}

impl<T> ToolReply<T> {
    /// The result `value`, with nothing attached yet.
    pub fn new(value: T) -> Self {
        Self {
            value,
            attachments: Vec::new(),
        }
    }

    /// Attaches a local file the plugin wrote, e.g. a rendered picture in its data directory.
    ///
    /// A relative path is resolved against the plugin's working directory when the call
    /// returns, because the core reads the file from a process of its own. The file must stay
    /// in place until the reply is delivered.
    pub fn file(mut self, path: impl Into<PathBuf>, mime_type: impl Into<String>) -> Self {
        self.attachments.push(Attachment::File {
            path: path.into(),
            mime_type: mime_type.into(),
        });
        self
    }

    /// Attaches a remote resource the platform fetches itself.
    pub fn url(mut self, url: impl Into<String>, mime_type: impl Into<String>) -> Self {
        self.attachments.push(Attachment::Url {
            url: url.into(),
            mime_type: mime_type.into(),
        });
        self
    }
}

/// What a tool handler may return: any serializable value, or a [`ToolReply`] that carries
/// attachments as well.
pub trait IntoToolOutput {
    /// The JSON the model reads and the attachments delivered to the user.
    fn into_tool_output(self) -> PluginResult<(serde_json::Value, Vec<ToolAttachment>)>;
}

impl<T: Serialize> IntoToolOutput for T {
    fn into_tool_output(self) -> PluginResult<(serde_json::Value, Vec<ToolAttachment>)> {
        Ok((serde_json::to_value(self)?, Vec::new()))
    }
}

impl<T: Serialize> IntoToolOutput for ToolReply<T> {
    fn into_tool_output(self) -> PluginResult<(serde_json::Value, Vec<ToolAttachment>)> {
        let attachments = self
            .attachments
            .into_iter()
            .map(|attachment| match attachment {
                // Failing here fails the call, so the model learns the picture is missing
                // instead of the reply silently going out without it.
                Attachment::File { path, mime_type } => Ok(ToolAttachment {
                    mime_type,
                    file_path: Some(
                        std::path::absolute(&path)
                            .map_err(|err| format!("attachment path '{}': {err}", path.display()))?
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    url: None,
                }),
                Attachment::Url { url, mime_type } => Ok(ToolAttachment {
                    mime_type,
                    file_path: None,
                    url: Some(url),
                }),
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((serde_json::to_value(self.value)?, attachments))
    }
}

/// Deserializes typed tool arguments. Protobuf turned every number into a double on the way, so
/// whole numbers become integers again first, or `{"days": 3}` could not fill a `u32`.
fn parse_typed<A: DeserializeOwned>(value: serde_json::Value) -> Result<A, serde_json::Error> {
    serde_json::from_value(crate::json::integral_numbers(value))
}

/// A lifecycle event, as delivered to [`Router::subscribe`] handlers.
#[derive(Debug, Clone)]
pub enum Event {
    /// The bot delivered a message.
    MessageSent(MessageSentEvent),
    /// A platform notice (join, poke, recall, ...), whether or not the bot reacts to it.
    Notice(MessageEvent),
    /// The model answered a message.
    LlmResponse(LlmResponseEvent),
    /// The agent started answering a message.
    AgentBegin(AgentBegin),
    /// The agent finished a turn.
    AgentDone(AgentDone),
    /// The model asked for a tool call (before it runs).
    ToolCall(ToolCall),
    /// A tool call finished.
    ToolResult(ToolResult),
}

/// A reply about to be delivered, as seen by a [`Router::decorate_reply`] handler.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The message being answered.
    pub event: MessageEvent,
    /// The reply's segments.
    pub segments: Vec<MessageSegment>,
    /// Whether the model or a command produced it.
    pub source: ReplySource,
    /// The command or trigger name when `source` is [`ReplySource::Command`].
    pub command: String,
}

/// The system prompt of a turn about to be answered, as seen by a
/// [`Router::rewrite_system_prompt`] handler.
#[derive(Debug, Clone)]
pub struct SystemPrompt {
    /// The message the model is about to answer.
    pub event: MessageEvent,
    /// The model conversation; the rewrite must be the same for every turn of it.
    pub session_id: String,
    /// The prompt as built so far (instance prompt or persona, skill catalog, and earlier
    /// plugins' rewrites).
    pub prompt: String,
}

/// One registered tool: what the model sees and what runs.
#[derive(Clone)]
struct ToolEntry {
    meta: ToolMeta,
    handler: ToolHandler,
}

/// State shared between a [`Router`] and every [`ContextSlot`] clone.
#[derive(Default)]
struct Shared {
    context: RwLock<Option<PluginContext>>,
    /// In declaration order, which is the order `PluginMeta.tools` reports them in.
    tools: RwLock<Vec<ToolEntry>>,
}

/// Shared handle to a running [`Router`]: the context the host hands to `on_load` (data
/// directory, configuration, Core handle), and the tool registry for adding and removing tools
/// at runtime. Clone it into handlers.
#[derive(Clone, Default)]
pub struct ContextSlot(Arc<Shared>);

impl std::fmt::Debug for ContextSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tools: Vec<String> = self
            .tool_metas()
            .into_iter()
            .map(|meta| meta.name)
            .collect();
        f.debug_struct("ContextSlot")
            .field("context", &self.get())
            .field("tools", &tools)
            .finish()
    }
}

impl ContextSlot {
    /// The context, `None` before the host loaded the plugin.
    pub fn get(&self) -> Option<PluginContext> {
        self.0
            .context
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The Core handle, for handlers that have no event to take it from (HTTP routes, actions,
    /// background tasks). Fails with [`CoreError::Standalone`] before the host loaded the plugin
    /// and in standalone mode.
    pub fn core(&self) -> Result<CoreHandle, CoreError> {
        self.handle().ok_or(CoreError::Standalone)
    }

    /// The Core handle if there is one, for events built by the router.
    fn handle(&self) -> Option<CoreHandle> {
        self.get().and_then(|ctx| ctx.core)
    }

    fn update(&self, apply: impl FnOnce(&mut Option<PluginContext>)) {
        apply(
            &mut self
                .0
                .context
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }

    // The tool table is only ever touched under a short, synchronous lock: never across an
    // await, so a slow tool or refresh cannot block `GetPluginMeta`.
    fn tools(&self) -> std::sync::RwLockReadGuard<'_, Vec<ToolEntry>> {
        self.0
            .tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tools_mut(&self) -> std::sync::RwLockWriteGuard<'_, Vec<ToolEntry>> {
        self.0
            .tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tool_metas(&self) -> Vec<ToolMeta> {
        self.tools()
            .iter()
            .map(|entry| entry.meta.clone())
            .collect()
    }

    fn tool_handler(&self, name: &str) -> Option<ToolHandler> {
        self.tools()
            .iter()
            .find(|entry| entry.meta.name == name)
            .map(|entry| entry.handler.clone())
    }

    fn insert_tool(&self, entry: ToolEntry) -> Result<(), String> {
        let mut tools = self.tools_mut();
        if entry.meta.name.is_empty() {
            return Err("a tool name must not be empty".to_string());
        }
        if tools.iter().any(|known| known.meta.name == entry.meta.name) {
            return Err(format!("tool '{}' is already declared", entry.meta.name));
        }
        tools.push(entry);
        Ok(())
    }

    /// Removes tool `name`, returning it and where it was.
    fn take_tool(&self, name: &str) -> Option<(usize, ToolEntry)> {
        let mut tools = self.tools_mut();
        let index = tools.iter().position(|entry| entry.meta.name == name)?;
        Some((index, tools.remove(index)))
    }

    /// Adds a tool while the plugin runs and tells the core, which offers it from the next turn
    /// on. Takes the same [`ToolSpec`] and handler as [`Router::tool`].
    ///
    /// The tool list is part of the model's request prefix: every change invalidates the
    /// provider's prompt cache, so change tools rarely (on configuration, not per message).
    ///
    /// Fails with [`CoreError::InvalidArgument`] when the name is taken, and with the core's
    /// error when it cannot refresh the plugin's metadata — the tool is then removed again, so
    /// the plugin and the core never disagree. In standalone mode there is no core to tell, and
    /// the tool is only registered.
    pub async fn add_tool<A, F, Fut, R>(
        &self,
        spec: ToolSpec<A>,
        handler: F,
    ) -> Result<(), CoreError>
    where
        A: Send + 'static,
        F: Fn(A, Option<MessageEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoToolOutput + 'static,
    {
        let name = spec.meta.name.clone();
        self.insert_tool(tool_entry(spec, handler))
            .map_err(CoreError::InvalidArgument)?;
        if let Some(core) = self.handle() {
            if let Err(err) = core.refresh_plugin_meta().await {
                self.take_tool(&name);
                return Err(err);
            }
        }
        Ok(())
    }

    /// Removes tool `name` while the plugin runs and tells the core; returns whether it existed.
    ///
    /// When the core cannot refresh the plugin's metadata the tool is restored and the core's
    /// error returned.
    pub async fn remove_tool(&self, name: &str) -> Result<bool, CoreError> {
        let Some((index, entry)) = self.take_tool(name) else {
            return Ok(false);
        };
        if let Some(core) = self.handle() {
            if let Err(err) = core.refresh_plugin_meta().await {
                let mut tools = self.tools_mut();
                let index = index.min(tools.len());
                tools.insert(index, entry);
                return Err(err);
            }
        }
        Ok(true)
    }
}

/// A [`Plugin`] built from handler closures. See the [module docs](self).
pub struct Router {
    /// Everything but `tools`, which live in the shared registry so they can change at runtime.
    meta: PluginMeta,
    // Commands and triggers share one namespace: Core names either in `CommandExecuteRequest`.
    commands: HashMap<String, CommandHandler>,
    actions: HashMap<String, ActionHandler>,
    events: Vec<(EventKind, EventHandler)>,
    decorator: Option<DecorateHandler>,
    pre_filter: Option<PreFilterHandler>,
    preparer: Option<PrepareHandler>,
    prompt_rewriter: Option<PromptHandler>,
    http: Routes,
    context: ContextSlot,
    conversations: Conversations,
}

impl Router {
    /// A plugin with no handlers yet.
    pub fn new(id: impl Into<String>, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            meta: PluginMeta {
                id: id.into(),
                name: name.into(),
                version: version.into(),
                ..Default::default()
            },
            commands: HashMap::new(),
            actions: HashMap::new(),
            events: Vec::new(),
            decorator: None,
            pre_filter: None,
            preparer: None,
            prompt_rewriter: None,
            http: Routes::default(),
            context: ContextSlot::default(),
            conversations: Conversations::default(),
        }
    }

    /// Sets the author shown in the console.
    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.meta.author = author.into();
        self
    }

    /// Sets the description shown in the console.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.meta.description = description.into();
        self
    }

    /// The context slot, filled when the host loads the plugin; clone it into handlers that
    /// need the data directory, configuration or Core handle, or that add tools at runtime.
    pub fn context(&self) -> ContextSlot {
        self.context.clone()
    }

    fn claim_command_name(&self, name: &str) {
        // A duplicate is a programming mistake: Core could route only one of them.
        assert!(
            !self.commands.contains_key(name),
            "'{name}' is already declared as a command or trigger"
        );
    }

    /// Declares a slash command (`spec` is a name or a [`CommandSpec`]). The handler answers by
    /// returning text or segments (`Ok(())` for no reply), or through `event.reply(..)`.
    ///
    /// # Panics
    /// If a command or trigger with the same name was already declared.
    pub fn command<F, Fut, R>(mut self, spec: impl Into<CommandSpec>, handler: F) -> Self
    where
        F: Fn(CommandEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoReply + 'static,
    {
        let meta = spec.into().into_meta();
        self.claim_command_name(&meta.name);
        self.commands
            .insert(meta.name.clone(), box_command(handler));
        self.meta.commands.push(meta);
        self
    }

    /// Declares a command group: `/name <subcommand> ...`, with the subcommands declared in
    /// `build`. `spec` (a name or a [`CommandSpec`]) sets the group's description, access and
    /// scope, which apply to every subcommand.
    ///
    /// ```ignore
    /// router.command_group(CommandSpec::new("admin").access(CommandAccess::Admins), |group| {
    ///     group
    ///         .command(CommandSpec::new("ban").usage("/admin ban <user>"), ban)
    ///         .command("unban", unban)
    /// })
    /// ```
    ///
    /// `/name` alone or an unknown subcommand answers with the group's help; `/help` lists the
    /// subcommands through `CommandMeta.subcommands`.
    ///
    /// # Panics
    /// If the name is taken, or the group has no subcommand.
    pub fn command_group(
        mut self,
        spec: impl Into<CommandSpec>,
        build: impl FnOnce(CommandGroup) -> CommandGroup,
    ) -> Self {
        let mut meta = spec.into().into_meta();
        self.claim_command_name(&meta.name);
        let group = build(CommandGroup::new(&meta.name));
        meta.subcommands = group.metas();
        assert!(
            !meta.subcommands.is_empty(),
            "command group '/{}' declares no subcommand",
            meta.name
        );
        if meta.usage == format!("/{}", meta.name) {
            meta.usage = format!("/{} <subcommand>", meta.name);
        }
        self.commands
            .insert(meta.name.clone(), group.into_handler(meta.clone()));
        self.meta.commands.push(meta);
        self
    }

    /// Declares a trigger. The handler is called like a command handler.
    ///
    /// # Panics
    /// If a command or trigger with the same name was already declared.
    pub fn trigger<F, Fut, R>(mut self, spec: TriggerSpec, handler: F) -> Self
    where
        F: Fn(CommandEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoReply + 'static,
    {
        self.claim_command_name(&spec.0.name);
        self.commands
            .insert(spec.0.name.clone(), box_command(handler));
        self.meta.triggers.push(spec.0);
        self
    }

    /// Declares a tool. The handler receives the model's arguments (a typed struct for
    /// [`ToolSpec::typed`], a JSON object for [`ToolSpec::new`]) and the message the model was
    /// answering (`None` outside a platform conversation), and returns anything serializable;
    /// a result that is not a JSON object is wrapped as `{"result": value}`. Wrap the value in a
    /// [`ToolReply`] to send pictures or files to the user along with it. An error is reported
    /// to the model as a failed call.
    ///
    /// # Panics
    /// If a tool with the same name was already declared.
    pub fn tool<A, F, Fut, R>(self, spec: ToolSpec<A>, handler: F) -> Self
    where
        A: Send + 'static,
        F: Fn(A, Option<MessageEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoToolOutput + 'static,
    {
        if let Err(err) = self.context.insert_tool(tool_entry(spec, handler)) {
            panic!("{err}");
        }
        self
    }

    /// Declares a management action for the control plane
    /// (`POST /api/v1/plugins/{id}/actions/{action}`). Actions are never offered to the model.
    pub fn action<F, Fut>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<serde_json::Value>> + Send + 'static,
    {
        self.actions.insert(
            name.into(),
            Arc::new(move |params| Box::pin(handler(params))),
        );
        self
    }

    /// Subscribes to a lifecycle event: message delivery, notices, model answers, and the
    /// agent's progress ([`EventKind::AgentBegin`], [`EventKind::AgentDone`],
    /// [`EventKind::ToolCall`], [`EventKind::ToolResult`]). Core never waits on event handlers,
    /// and an error is only logged.
    pub fn subscribe<F, Fut>(mut self, kind: EventKind, handler: F) -> Self
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<()>> + Send + 'static,
    {
        assert!(
            kind != EventKind::Unspecified,
            "subscribe to a concrete event kind"
        );
        if !self.meta.events.contains(&(kind as i32)) {
            self.meta.events.push(kind as i32);
        }
        self.events
            .push((kind, Arc::new(move |event| Box::pin(handler(event)))));
        self
    }

    /// Sets the reply decorator: return `Ok(None)` to leave a reply alone, or `Ok(Some(..))`
    /// to replace it (an empty list suppresses it). Core gives it three seconds and keeps the
    /// reply unchanged on error or timeout; decoration never changes what the model remembers.
    pub fn decorate_reply<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Reply) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<Option<Vec<MessageSegment>>>> + Send + 'static,
    {
        self.meta.decorates_replies = true;
        self.decorator = Some(Arc::new(move |reply| Box::pin(handler(reply))));
        self
    }

    /// Sets the turn preparer: before the model answers a message, the handler receives it and
    /// the conversation's session id, and returns text to prepend to the current user message
    /// (empty adds nothing). Use it for retrieval and long-term memory; the text becomes part of
    /// the conversation history. Core gives it three seconds and goes ahead without it on error
    /// or timeout.
    pub fn prepare_turn<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(MessageEvent, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<String>> + Send + 'static,
    {
        self.meta.prepares_turns = true;
        self.preparer = Some(Arc::new(move |event, session_id| {
            Box::pin(handler(event, session_id))
        }));
        self
    }

    /// Sets the system prompt rewriter: once per turn, before the first model request, the
    /// handler sees the prompt and returns `Ok(Some(prompt))` to replace it or `Ok(None)` to
    /// keep it.
    ///
    /// The system prompt opens every request and decides the provider's prompt cache, so the
    /// rewrite must give the same result for every turn of a conversation: no clocks, counters
    /// or per-message data (use [`prepare_turn`](Self::prepare_turn) for those). An error, an
    /// empty prompt or a late answer leaves the prompt unchanged.
    ///
    /// (Named after what it does rather than the hook, `Plugin::on_llm_request`, which an
    /// inherent method of the same name would hide.)
    pub fn rewrite_system_prompt<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(SystemPrompt) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<Option<String>>> + Send + 'static,
    {
        self.meta.rewrites_system_prompt = true;
        self.prompt_rewriter = Some(Arc::new(move |prompt| Box::pin(handler(prompt))));
        self
    }

    /// Serves `method path` under `/api/v1/plugins/<id>/http/`. The handler returns a
    /// [`Response`](crate::http::Response), a `serde_json::Value` (sent as JSON) or text; an
    /// error is logged and answers 500. Unknown paths answer 404, other methods on a
    /// declared path 405.
    ///
    /// # Panics
    /// If `method` is not a word, `path` does not start with `/`, or the route was declared.
    pub fn http_route<F, Fut, R>(mut self, method: &str, path: &str, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoResponse + 'static,
    {
        self.http.add(method, path, handler);
        self.meta.serves_http = true;
        self
    }

    /// Sets the pre-filter, which sees every message before commands and the model.
    pub fn pre_filter<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(MessageEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<Option<PreFilterResult>>> + Send + 'static,
    {
        self.pre_filter = Some(Arc::new(move |event| Box::pin(handler(event))));
        self
    }

    /// Runs `handler` for `request` in a new session and waits for its first yield point.
    async fn start(
        &self,
        handler: CommandHandler,
        request: CommandExecuteRequest,
    ) -> CommandExecuteResponse {
        let session = Session::new(self.conversations.clone());
        let done = session.open_turn();
        let event = CommandEvent::new(request, self.context.handle(), Some(session.clone()));

        // The handler runs as its own task because it may outlive this RPC by suspending in
        // `wait_next`. A second task watches it so a panic still answers the waiting RPC.
        let task = tokio::spawn({
            let session = session.clone();
            async move {
                let command = event.command().to_string();
                match handler(event.clone()).await {
                    Ok(segments) => {
                        if let Err(err) = event.reply(segments).await {
                            tracing::warn!(%command, error = %err, "Late command reply failed");
                        }
                        session.finish(0, true, String::new());
                    }
                    Err(err) if session.has_turn() => session.finish(0, false, err.to_string()),
                    Err(err) => tracing::warn!(%command, error = %err, "Command handler failed"),
                }
            }
        });
        tokio::spawn(async move {
            if let Err(err) = task.await {
                session.finish(0, false, format!("command handler panicked: {err}"));
            }
        });

        settle(done).await
    }

    fn message(&self, raw: Option<PipelineEventRequest>) -> Option<MessageEvent> {
        raw.map(|raw| MessageEvent::new(raw, self.context.handle()))
    }
}

/// Waits for a turn's response.
async fn settle(
    done: tokio::sync::oneshot::Receiver<CommandExecuteResponse>,
) -> CommandExecuteResponse {
    done.await.unwrap_or_else(|_| CommandExecuteResponse {
        success: false,
        error_message: "command handler ended without answering".to_string(),
        ..Default::default()
    })
}

/// Boxes a command, trigger or subcommand handler.
pub(crate) fn box_command<F, Fut, R>(handler: F) -> CommandHandler
where
    F: Fn(CommandEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = PluginResult<R>> + Send + 'static,
    R: IntoReply + 'static,
{
    Arc::new(move |event| {
        let future = handler(event);
        Box::pin(async move { future.await.map(IntoReply::into_segments) })
    })
}

/// Erases a tool's argument and result types behind the JSON the wire carries.
fn tool_entry<A, F, Fut, R>(spec: ToolSpec<A>, handler: F) -> ToolEntry
where
    A: Send + 'static,
    F: Fn(A, Option<MessageEvent>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = PluginResult<R>> + Send + 'static,
    R: IntoToolOutput + 'static,
{
    let name = spec.meta.name.clone();
    let parse = spec.parse;
    let handler: ToolHandler = Arc::new(move |args, event| match parse(args) {
        Ok(args) => {
            let future = handler(args, event);
            Box::pin(async move { future.await?.into_tool_output() })
        }
        // Worded for the model, which reads it and can retry with corrected arguments.
        Err(err) => {
            let message = format!("invalid arguments for tool '{name}': {err}");
            Box::pin(async move { Err(message.into()) })
        }
    });
    ToolEntry {
        meta: spec.meta,
        handler,
    }
}

/// Converts a handler's JSON result into the `Struct` the wire carries.
fn result_struct(value: serde_json::Value) -> prost_types::Struct {
    match value {
        serde_json::Value::Object(fields) => crate::json::to_struct(fields),
        other => {
            crate::json::to_struct(serde_json::Map::from_iter([("result".to_string(), other)]))
        }
    }
}

#[async_trait]
impl Plugin for Router {
    fn meta(&self) -> PluginMeta {
        let mut meta = self.meta.clone();
        meta.tools = self.context.tool_metas();
        meta
    }

    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
        let ctx = ctx.clone();
        self.context.update(|slot| *slot = Some(ctx));
        Ok(())
    }

    async fn on_config_reload(&mut self, config: prost_types::Struct) -> PluginResult<()> {
        self.context.update(|slot| {
            if let Some(ctx) = slot {
                ctx.config = Some(config);
            }
        });
        Ok(())
    }

    async fn on_pre_filter(
        &self,
        req: PipelineEventRequest,
    ) -> PluginResult<Option<PreFilterResult>> {
        match &self.pre_filter {
            Some(handler) => handler(MessageEvent::new(req, self.context.handle())).await,
            None => Ok(None),
        }
    }

    /// Runs a command, trigger or continuation and answers once the handler yields.
    ///
    /// A continuation resumes the handler suspended in `wait_next` for this conversation; if
    /// none is waiting (the host restarted, or the wait already timed out), the handler named
    /// by `req.command` is called afresh with `continuation` set.
    async fn on_execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> PluginResult<CommandExecuteResponse> {
        if req.continuation {
            let key =
                MessageEvent::new(req.context.clone().unwrap_or_default(), None).conversation_key();
            if let Some(waiter) = self.conversations.take(&key) {
                let done = waiter.session.open_turn();
                let event = CommandEvent::new(
                    req.clone(),
                    self.context.handle(),
                    Some(waiter.session.clone()),
                );
                if waiter.resume.send(event).is_ok() {
                    return Ok(settle(done).await);
                }
                // The handler stopped waiting in between; run the command afresh instead.
                waiter.session.abandon_turn();
            }
        }

        match self.commands.get(&req.command) {
            Some(handler) => Ok(self.start(handler.clone(), req).await),
            None => Ok(CommandExecuteResponse {
                success: false,
                error_message: format!("Unknown command: {}", req.command),
                ..Default::default()
            }),
        }
    }

    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {
        let Some(handler) = self.context.tool_handler(&req.tool_name) else {
            return Ok(ToolCallResponse {
                call_id: req.call_id,
                success: false,
                error_message: format!("Unknown tool: {}", req.tool_name),
                ..Default::default()
            });
        };
        let args = match req.payload {
            Some(tool_call_request::Payload::StructuredArgs(args)) => {
                serde_json::Value::Object(crate::json::from_struct(args))
            }
            _ => serde_json::Value::Object(serde_json::Map::new()),
        };
        let event = self.message(req.context);
        Ok(match handler(args, event).await {
            Ok((value, attachments)) => ToolCallResponse {
                call_id: req.call_id,
                success: true,
                payload: Some(tool_call_response::Payload::StructuredResult(
                    result_struct(value),
                )),
                attachments,
                ..Default::default()
            },
            // The model is told the tool failed and can explain or retry.
            Err(err) => ToolCallResponse {
                call_id: req.call_id,
                success: false,
                error_message: err.to_string(),
                ..Default::default()
            },
        })
    }

    async fn on_invoke_action(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> PluginResult<serde_json::Value> {
        match self.actions.get(action) {
            Some(handler) => handler(params).await,
            None => Err(format!(
                "unknown action '{action}'; declared actions: {:?}",
                self.actions.keys().collect::<Vec<_>>()
            )
            .into()),
        }
    }

    async fn on_event(&self, notification: EventNotification) -> PluginResult<()> {
        use event_notification::Detail;
        let (kind, event) = match notification.detail {
            Some(Detail::MessageSent(sent)) => (EventKind::MessageSent, Event::MessageSent(sent)),
            Some(Detail::Notice(notice)) => (
                EventKind::Notice,
                Event::Notice(MessageEvent::new(notice, self.context.handle())),
            ),
            Some(Detail::LlmResponse(answer)) => {
                (EventKind::LlmResponse, Event::LlmResponse(answer))
            }
            Some(Detail::AgentBegin(begin)) => (
                EventKind::AgentBegin,
                Event::AgentBegin(AgentBegin {
                    event: self.message(begin.context),
                    session_id: begin.session_id,
                }),
            ),
            Some(Detail::AgentDone(done)) => (
                EventKind::AgentDone,
                Event::AgentDone(AgentDone {
                    event: self.message(done.context),
                    session_id: done.session_id,
                    success: done.success,
                    content: done.content,
                    error: done.error,
                    tools: done.tools,
                }),
            ),
            Some(Detail::ToolCall(call)) => (
                EventKind::ToolCall,
                Event::ToolCall(ToolCall {
                    event: self.message(call.context),
                    session_id: call.session_id,
                    tool_name: call.tool_name,
                    arguments: serde_json::Value::Object(
                        call.arguments
                            .map(crate::json::from_struct)
                            .unwrap_or_default(),
                    ),
                }),
            ),
            Some(Detail::ToolResult(result)) => (
                EventKind::ToolResult,
                Event::ToolResult(ToolResult {
                    event: self.message(result.context),
                    session_id: result.session_id,
                    tool_name: result.tool_name,
                    success: result.success,
                    result: result.result,
                }),
            ),
            // A notification always names its kind; an empty one is a core or version bug.
            None => return Err("event notification without a detail".into()),
        };
        for (_, handler) in self.events.iter().filter(|(k, _)| *k == kind) {
            // One failing subscriber must not stop the others.
            if let Err(err) = handler(event.clone()).await {
                tracing::warn!(?kind, error = %err, "Event handler failed");
            }
        }
        Ok(())
    }

    async fn on_decorate_reply(
        &self,
        req: DecorateReplyRequest,
    ) -> PluginResult<Option<Vec<MessageSegment>>> {
        let Some(handler) = &self.decorator else {
            return Ok(None);
        };
        handler(Reply {
            event: MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            segments: req.segments,
            source: ReplySource::try_from(req.source).unwrap_or(ReplySource::Unspecified),
            command: req.command,
        })
        .await
    }

    async fn on_prepare_turn(&self, req: PrepareTurnRequest) -> PluginResult<String> {
        let Some(handler) = &self.preparer else {
            return Ok(String::new());
        };
        handler(
            MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            req.session_id,
        )
        .await
    }

    async fn on_llm_request(&self, req: LlmRequestHookRequest) -> PluginResult<Option<String>> {
        let Some(handler) = &self.prompt_rewriter else {
            return Ok(None);
        };
        handler(SystemPrompt {
            event: MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            session_id: req.session_id,
            prompt: req.system_prompt,
        })
        .await
    }

    async fn on_http_request(&self, req: HttpRequest) -> PluginResult<HttpResponse> {
        Ok(self.http.dispatch(req).await)
    }
}
