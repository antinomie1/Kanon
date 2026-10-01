//! [`Router`]: a [`Plugin`] assembled from closures, so a plugin declares each command, trigger,
//! tool and hook once — next to its handler — instead of matching names by hand in trait
//! methods and repeating them in [`PluginMeta`].
//!
//! ```ignore
//! let plugin = Router::new("org.example.echo", "Echo", "0.1.0")
//!     .command(CommandSpec::new("echo").usage("/echo <text>"), |event| async move {
//!         Ok(event.raw_args().to_string())
//!     })
//!     .trigger(TriggerSpec::new("ping", "^ping$"), |event| async move {
//!         Ok(vec![segment::quote(event.event_id()), segment::text("pong")])
//!     });
//! KanonHost::new(plugin).run().await?;
//! ```
//!
//! Handlers are `'static` closures; share state by capturing an `Arc`. The context handed to
//! `on_load` (data directory, configuration, Core handle) is available through
//! [`Router::context`].

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kanon_proto::v1::{
    CommandAccess, CommandExecuteRequest, CommandExecuteResponse, CommandMeta, ConversationKind,
    DecorateReplyRequest, EventKind, EventNotification, LlmResponseEvent, MessageSegment,
    MessageSentEvent, PipelineEventRequest, PluginMeta, PreFilterResult, PrepareTurnRequest,
    ReplySource, ToolCallRequest, ToolCallResponse, ToolMeta, TriggerMeta, event_notification,
    tool_call_request, tool_call_response,
};

use crate::context::{CoreHandle, PluginContext};
use crate::event::{CommandEvent, Conversations, MessageEvent, Session};
use crate::plugin::{Plugin, PluginResult};
use crate::segment::IntoReply;

/// A boxed, sendable future, the return type of stored handlers.
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

type CommandHandler =
    Arc<dyn Fn(CommandEvent) -> BoxFuture<PluginResult<Vec<MessageSegment>>> + Send + Sync>;
type ToolHandler = Arc<
    dyn Fn(serde_json::Value, Option<MessageEvent>) -> BoxFuture<PluginResult<serde_json::Value>>
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
type PreFilterHandler =
    Arc<dyn Fn(MessageEvent) -> BoxFuture<PluginResult<Option<PreFilterResult>>> + Send + Sync>;

/// Declaration of a slash command.
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

/// Declaration of a tool the model may call.
#[derive(Debug, Clone)]
pub struct ToolSpec(ToolMeta);

impl ToolSpec {
    /// A tool named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(ToolMeta {
            name: name.into(),
            ..Default::default()
        })
    }

    /// What the tool does, for the model.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.0.description = description.into();
        self
    }

    /// JSON Schema of the arguments; must be a JSON object.
    ///
    /// # Panics
    /// If `schema` is not an object — a schema mistake should stop the plugin at startup.
    pub fn parameters(mut self, schema: serde_json::Value) -> Self {
        let serde_json::Value::Object(fields) = schema else {
            panic!("tool '{}': parameters must be a JSON object", self.0.name);
        };
        self.0.parameters = Some(crate::json::to_struct(fields));
        self
    }
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

/// Shared view of the context the host hands to `on_load`, readable from handlers.
#[derive(Debug, Clone, Default)]
pub struct ContextSlot(Arc<RwLock<Option<PluginContext>>>);

impl ContextSlot {
    /// The context, `None` before the host loaded the plugin.
    pub fn get(&self) -> Option<PluginContext> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The Core handle, `None` before loading or in standalone mode.
    pub fn core(&self) -> Option<CoreHandle> {
        self.get().and_then(|ctx| ctx.core)
    }

    fn update(&self, apply: impl FnOnce(&mut Option<PluginContext>)) {
        apply(
            &mut self
                .0
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }
}

/// A [`Plugin`] built from handler closures. See the [module docs](self).
pub struct Router {
    meta: PluginMeta,
    // Commands and triggers share one namespace: Core names either in `CommandExecuteRequest`.
    commands: HashMap<String, CommandHandler>,
    tools: HashMap<String, ToolHandler>,
    actions: HashMap<String, ActionHandler>,
    events: Vec<(EventKind, EventHandler)>,
    decorator: Option<DecorateHandler>,
    pre_filter: Option<PreFilterHandler>,
    preparer: Option<PrepareHandler>,
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
            tools: HashMap::new(),
            actions: HashMap::new(),
            events: Vec::new(),
            decorator: None,
            pre_filter: None,
            preparer: None,
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
    /// need the data directory or configuration.
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

    /// Declares a slash command. The handler answers by returning text or segments (`Ok(())`
    /// for no reply), or through `event.reply(..)`.
    ///
    /// # Panics
    /// If a command or trigger with the same name was already declared.
    pub fn command<F, Fut, R>(mut self, spec: CommandSpec, handler: F) -> Self
    where
        F: Fn(CommandEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoReply + 'static,
    {
        self.claim_command_name(&spec.0.name);
        self.commands
            .insert(spec.0.name.clone(), box_command(handler));
        self.meta.commands.push(spec.0);
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

    /// Declares a tool. The handler receives the model's arguments (a JSON object) and the
    /// message the model was answering (`None` outside a platform conversation), and returns
    /// a JSON result; a non-object result is wrapped as `{"result": value}`. An error is
    /// reported to the model as a failed call.
    ///
    /// # Panics
    /// If a tool with the same name was already declared.
    pub fn tool<F, Fut>(mut self, spec: ToolSpec, handler: F) -> Self
    where
        F: Fn(serde_json::Value, Option<MessageEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<serde_json::Value>> + Send + 'static,
    {
        assert!(
            !self.tools.contains_key(&spec.0.name),
            "tool '{}' is already declared",
            spec.0.name
        );
        self.tools.insert(
            spec.0.name.clone(),
            Arc::new(move |args, event| Box::pin(handler(args, event))),
        );
        self.meta.tools.push(spec.0);
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

    /// Subscribes to a lifecycle event. Core never waits on event handlers, and an error is
    /// only logged.
    pub fn subscribe<F, Fut>(mut self, kind: EventKind, handler: F) -> Self
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<()>> + Send + 'static,
    {
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
        let event = CommandEvent::new(request, self.context.core(), Some(session.clone()));

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

fn box_command<F, Fut, R>(handler: F) -> CommandHandler
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
        self.meta.clone()
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
            Some(handler) => handler(MessageEvent::new(req, self.context.core())).await,
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
                    self.context.core(),
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
        let Some(handler) = self.tools.get(&req.tool_name) else {
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
        let event = req
            .context
            .map(|ctx| MessageEvent::new(ctx, self.context.core()));
        Ok(match handler(args, event).await {
            Ok(value) => ToolCallResponse {
                call_id: req.call_id,
                success: true,
                payload: Some(tool_call_response::Payload::StructuredResult(
                    result_struct(value),
                )),
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
        let (kind, event) = match notification.detail {
            Some(event_notification::Detail::MessageSent(sent)) => {
                (EventKind::MessageSent, Event::MessageSent(sent))
            }
            Some(event_notification::Detail::Notice(notice)) => (
                EventKind::Notice,
                Event::Notice(MessageEvent::new(notice, self.context.core())),
            ),
            Some(event_notification::Detail::LlmResponse(answer)) => {
                (EventKind::LlmResponse, Event::LlmResponse(answer))
            }
            // Agent and tool events are not routed to handlers yet.
            Some(_) | None => return Ok(()),
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
            event: MessageEvent::new(req.context.unwrap_or_default(), self.context.core()),
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
            MessageEvent::new(req.context.unwrap_or_default(), self.context.core()),
            req.session_id,
        )
        .await
    }
}
