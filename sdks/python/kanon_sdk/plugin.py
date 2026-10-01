"""Plugin base class and decorators for Kanon Python SDK.

A plugin is a :class:`Plugin` subclass whose methods are marked with decorators:

* :func:`command` — a slash command (``/weather Paris``), with aliases and an access level;
* :func:`trigger` — a regular expression matched against plain messages;
* :func:`tool` — a function the model may call;
* :func:`action` — an operator-only management action (never offered to the model);
* :func:`on_event` — a lifecycle event (``message_sent``, ``notice``, ``llm_response``);
* :func:`decorate_reply` — a hook that rewrites the bot's replies before delivery.

Command and trigger handlers receive a :class:`~kanon_sdk.event.CommandEvent` and may answer by
returning text/segments or with ``await event.reply(...)``; ``await event.wait_next()`` asks the
user a follow-up question (see :mod:`kanon_sdk.event`).
"""

import asyncio
import inspect
import re
import sys
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Sequence, Set

from google.protobuf.json_format import MessageToDict, ParseDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk.context import PluginContext, Replyable, to_segments
from kanon_sdk.event import CommandEvent, Conversations, MessageEvent, _Session, _Turn, run_turn
from kanon_sdk.proto import pb

#: Access levels a command or trigger may declare. The operator's command policy overrides them.
ACCESS_LEVELS = {
    "everyone": pb.COMMAND_ACCESS_EVERYONE,
    "admins_in_groups": pb.COMMAND_ACCESS_ADMINS_IN_GROUPS,
    "admins": pb.COMMAND_ACCESS_ADMINS,
}

#: Lifecycle events a plugin may subscribe to with :func:`on_event`.
EVENT_KINDS = {
    "message_sent": pb.EVENT_KIND_MESSAGE_SENT,
    "notice": pb.EVENT_KIND_NOTICE,
    "llm_response": pb.EVENT_KIND_LLM_RESPONSE,
}


def _access(level: str) -> int:
    if level not in ACCESS_LEVELS:
        raise ValueError(f"unknown access level {level!r}; expected one of {sorted(ACCESS_LEVELS)}")
    return ACCESS_LEVELS[level]


def command(
    name: str,
    description: str = "",
    usage: str = "",
    priority: int = 500,
    aliases: Sequence[str] = (),
    access: str = "everyone",
) -> Callable:
    """Declares a slash command handler.

    Args:
        name: Command name without the slash.
        description: One line shown by ``/help``.
        usage: Usage example shown by ``/help``.
        priority: Lower wins when several plugins declare the same name.
        aliases: Other names that invoke the command; the handler always sees ``name``.
        access: ``"everyone"``, ``"admins_in_groups"`` or ``"admins"``. A default the operator
            can override in the node's command policy.
    """
    access_value = _access(access)

    def decorator(fn: Callable) -> Callable:
        fn._kanon_command = {
            "name": name.lstrip("/"),
            "description": description,
            "usage": usage,
            "priority": priority,
            "aliases": [alias.lstrip("/") for alias in aliases],
            "access": access_value,
        }
        return fn

    return decorator


def trigger(
    pattern: str,
    *,
    name: Optional[str] = None,
    description: str = "",
    priority: int = 500,
    access: str = "everyone",
) -> Callable:
    """Declares a handler for plain messages matching a regular expression.

    Core matches the pattern (Rust ``regex`` syntax, which shares Python's common subset) against
    the message text; the handler's ``event.args`` holds the capture groups, with ``""`` for a
    group that did not participate. Triggers run after slash commands and before the model.

    Args:
        pattern: Regular expression; anchor it (``^...$``) unless it may match anywhere.
        name: Name used for routing and logs; defaults to the method name.
        description: Shown under "消息触发" in ``/help``; leave empty to keep it unlisted.
        priority: Lower wins when several triggers match.
        access: As for :func:`command`.
    """
    access_value = _access(access)
    # Validate early with Python's engine: a pattern it rejects is almost certainly a mistake,
    # and an invalid pattern would otherwise only show up as a warning in Core's log.
    re.compile(pattern)

    def decorator(fn: Callable) -> Callable:
        fn._kanon_trigger = {
            "name": name or fn.__name__,
            "pattern": pattern,
            "description": description,
            "priority": priority,
            "access": access_value,
        }
        return fn

    return decorator


def tool(
    name: str,
    description: str = "",
    parameters: Optional[Dict[str, Any]] = None,
) -> Callable:
    """Declares an LLM tool call handler.

    The handler receives the model's arguments as a dict and, if it accepts a second parameter,
    the :class:`~kanon_sdk.event.MessageEvent` the model was answering (``None`` when the call
    did not come from a chat message) — so a tool knows who asked without trusting the model to
    pass it along.

    Operations an *operator* triggers — credential binding, QR login, diagnostics — belong in
    :func:`action` instead: a tool advertised by an adapter is offered to the model, which then
    tries to invoke it mid-conversation.
    """

    def decorator(fn: Callable) -> Callable:
        fn._kanon_tool = {
            "name": name,
            "description": description,
            "parameters": parameters or {},
        }
        return fn

    return decorator


def action(
    name: str,
    parameters: Optional[Dict[str, Any]] = None,
) -> Callable:
    """Declares a management action invoked by the control plane.

    Actions are never advertised to the model and never appear in :meth:`Plugin.meta`: they exist
    for the console (``POST /api/v1/plugins/{id}/actions/{action}``) and for core endpoints that
    drive plugin administration, such as the QQ credential binding flow.

    Args:
        name: Action name the control plane invokes.
        parameters: JSON Schema for the action arguments (used for documentation).
    """

    def decorator(fn: Callable) -> Callable:
        fn._kanon_action = {
            "name": name,
            "parameters": parameters or {},
        }
        return fn

    return decorator


def on_event(kind: str) -> Callable:
    """Subscribes a handler to a lifecycle event.

    Kinds and what the handler receives:

    * ``"message_sent"`` — a ``pb.MessageSentEvent`` for every message the bot delivered;
    * ``"notice"`` — a :class:`~kanon_sdk.event.MessageEvent` for a platform notice (join, poke,
      recall, ...), whether or not the bot reacts to it;
    * ``"llm_response"`` — a ``pb.LlmResponseEvent`` with the model's answer and the message it
      answered.

    Events are notifications: the handler's return value is ignored, and Core never waits on it.
    """
    if kind not in EVENT_KINDS:
        raise ValueError(f"unknown event kind {kind!r}; expected one of {sorted(EVENT_KINDS)}")

    def decorator(fn: Callable) -> Callable:
        kinds = list(getattr(fn, "_kanon_events", []))
        kinds.append(kind)
        fn._kanon_events = kinds
        return fn

    return decorator


@dataclass
class Reply:
    """A reply about to be delivered, as seen by a :func:`decorate_reply` handler.

    Attributes:
        event: The message being answered.
        segments: The reply's segments.
        source: ``"llm"`` for a model answer, ``"command"`` for a command or trigger answer.
        command: The command or trigger name when ``source == "command"``.
    """

    event: MessageEvent
    segments: List[pb.MessageSegment]
    source: str
    command: str


def decorate_reply(fn: Callable) -> Callable:
    """Marks the plugin's reply decorator.

    The handler receives a :class:`Reply` and returns ``None`` to leave it alone, or new content
    (text, segments) to replace it; an empty list suppresses the reply. It runs on the reply
    path, so keep it fast: Core gives each decorator three seconds and keeps the reply unchanged
    if it fails or times out. Decoration never changes what the model remembers saying.
    """
    fn._kanon_decorator = True
    return fn


async def _call(handler: Callable, *args: Any) -> Any:
    """Calls a sync or async handler with as many of ``args`` as it accepts."""
    accepted = len(inspect.signature(handler).parameters)
    result = handler(*args[:accepted])
    if inspect.isawaitable(result):
        result = await result
    return result


class Plugin:
    """Abstract base class for Kanon out-of-process Python plugins."""

    id: str = "org.kanon.plugin.base"
    name: str = "Base Python Plugin"
    version: str = "0.1.0"
    author: str = "Kanon Dev"
    description: str = "Default Python plugin"
    priority: int = 500
    #: Event kinds to subscribe to in addition to those of :func:`on_event` handlers, for plugins
    #: that override :meth:`on_event` directly.
    events: Sequence[str] = ()

    def __init__(self) -> None:
        self.context: Optional[PluginContext] = None
        self._command_handlers: Dict[str, Callable] = {}
        self._trigger_handlers: Dict[str, Callable] = {}
        self._tool_handlers: Dict[str, Callable] = {}
        self._action_handlers: Dict[str, Callable] = {}
        self._event_handlers: Dict[str, List[Callable]] = {}
        self._decorator: Optional[Callable] = None
        self._conversations = Conversations()
        # Handler tasks outlive the RPC that started them (see kanon_sdk.event); keeping a
        # reference stops the event loop from garbage-collecting a suspended handler.
        self._tasks: Set[asyncio.Task] = set()
        self._collect_decorated_handlers()

    def _collect_decorated_handlers(self) -> None:
        """Discovers decorated methods and checks that their names do not collide."""
        for attr_name in dir(self):
            try:
                attr = getattr(self, attr_name)
            except Exception:
                continue
            if not callable(attr):
                continue

            if hasattr(attr, "_kanon_command"):
                self._command_handlers[attr._kanon_command["name"]] = attr
            if hasattr(attr, "_kanon_trigger"):
                self._trigger_handlers[attr._kanon_trigger["name"]] = attr
            if hasattr(attr, "_kanon_tool"):
                self._tool_handlers[attr._kanon_tool["name"]] = attr
            if hasattr(attr, "_kanon_action"):
                self._action_handlers[attr._kanon_action["name"]] = attr
            for kind in getattr(attr, "_kanon_events", []):
                self._event_handlers.setdefault(kind, []).append(attr)
            if getattr(attr, "_kanon_decorator", False):
                if self._decorator is not None:
                    raise ValueError(f"{type(self).__name__} declares more than one @decorate_reply")
                self._decorator = attr

        # Core sends commands and triggers through the same RPC, naming either in `command`, so
        # the two share one namespace.
        clash = set(self._command_handlers) & set(self._trigger_handlers)
        if clash:
            raise ValueError(f"names used by both a command and a trigger: {sorted(clash)}")
        for kind in self.events:
            if kind not in EVENT_KINDS:
                raise ValueError(f"unknown event kind {kind!r} in {type(self).__name__}.events")

    @property
    def core(self):
        """The host's :class:`~kanon_sdk.context.CoreHandle`, or ``None`` in standalone mode."""
        return self.context.core if self.context is not None else None

    def meta(self) -> pb.PluginMeta:
        """Constructs and returns static metadata for this plugin."""
        commands: List[pb.CommandMeta] = []
        for handler in self._command_handlers.values():
            info = handler._kanon_command
            commands.append(
                pb.CommandMeta(
                    name=info["name"],
                    description=info["description"],
                    usage=info["usage"],
                    priority=info["priority"],
                    aliases=info["aliases"],
                    access=info["access"],
                )
            )

        triggers: List[pb.TriggerMeta] = []
        for handler in self._trigger_handlers.values():
            info = handler._kanon_trigger
            triggers.append(
                pb.TriggerMeta(
                    name=info["name"],
                    description=info["description"],
                    pattern=info["pattern"],
                    priority=info["priority"],
                    access=info["access"],
                )
            )

        tools: List[pb.ToolMeta] = []
        for handler in self._tool_handlers.values():
            info = handler._kanon_tool
            param_struct = Struct()
            if info["parameters"]:
                ParseDict(info["parameters"], param_struct)
            tools.append(
                pb.ToolMeta(
                    name=info["name"],
                    description=info["description"],
                    parameters=param_struct if info["parameters"] else None,
                )
            )

        kinds = set(self._event_handlers) | set(self.events)
        return pb.PluginMeta(
            id=self.id,
            name=self.name,
            version=self.version,
            author=self.author,
            description=self.description,
            commands=commands,
            tools=tools,
            triggers=triggers,
            events=sorted(EVENT_KINDS[kind] for kind in kinds),
            decorates_replies=self._decorator is not None,
        )

    async def on_load(self, ctx: PluginContext) -> None:
        """Lifecycle hook invoked when the plugin host finishes loading the plugin.

        The host has already set :attr:`context` when this runs, so overriding it without
        calling ``super()`` is fine.
        """
        self.context = ctx

    async def on_unload(self) -> None:
        """Lifecycle hook invoked prior to plugin shutdown and process termination."""
        for task in list(self._tasks):
            task.cancel()

    async def on_config_reload(self, config: Dict[str, Any]) -> None:
        """Lifecycle hook invoked when plugin configuration is updated and hot-reloaded."""
        pass

    async def on_pre_filter(
        self,
        req: pb.PipelineEventRequest,
    ) -> Optional[pb.PreFilterResult]:
        """Intercepts inbound messages before command and LLM dispatching.

        Returning None or PreFilterResult with PASS allows downstream execution.
        Returning PreFilterResult with BLOCK halts downstream processing.
        """
        return None

    async def on_execute_command(
        self,
        req: pb.CommandExecuteRequest,
    ) -> pb.CommandExecuteResponse:
        """Runs a command, trigger or continuation and answers once the handler yields.

        A continuation resumes the handler suspended in ``wait_next`` for this conversation; if
        none is waiting (the plugin captured with an explicit ``capture_seconds``, or the host
        restarted), the handler named by ``req.command`` is called with ``continuation=True``.
        """
        if req.continuation:
            event_key = (req.context.platform, req.context.channel_id, req.context.sender_id)
            waiting = self._conversations.take(event_key)
            if waiting is not None:
                session, future = waiting
                session.turn = _Turn()
                future.set_result(CommandEvent(req, self.core, session))
                return await run_turn(session)

        handler = self._command_handlers.get(req.command) or self._trigger_handlers.get(
            req.command
        )
        if handler is None:
            return pb.CommandExecuteResponse(
                success=False,
                error_message=f"Unknown command: {req.command}",
            )

        session = _Session(self._conversations)
        session.turn = _Turn()
        event = CommandEvent(req, self.core, session)
        task = asyncio.create_task(self._run_handler(handler, event, session))
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        return await run_turn(session)

    async def _run_handler(self, handler: Callable, event: CommandEvent, session: _Session) -> None:
        """Runs one command handler to completion, across as many turns as it takes."""
        try:
            result = await _call(handler, event, event.args)
        except asyncio.TimeoutError:
            # The user never answered a wait_next; nothing is waiting for this handler anymore.
            if session.turn is not None:
                session.turn.finish()
            return
        except Exception as exc:  # noqa: BLE001 - reported to Core, or logged when nobody waits
            if session.turn is not None:
                session.turn.finish(success=False, error=f"{type(exc).__name__}: {exc}")
            else:
                print(f"[kanon-sdk] command '{event.command}' failed: {exc!r}", file=sys.stderr)
            return

        if isinstance(result, pb.CommandExecuteResponse):
            # An explicit response: its replies join the turn and its fields are honoured.
            await event.reply(list(result.replies))
            if session.turn is not None:
                session.turn.finish(result.capture_seconds, result.success, result.error_message)
            return
        if result is not None:
            await event.reply(result)
        if session.turn is not None:
            session.turn.finish()

    async def on_invoke_action(
        self,
        plugin_id: str,
        action_name: str,
        parameters: Dict[str, Any],
    ) -> pb.PluginActionResponse:
        """Dispatches a control-plane management action to its handler.

        Actions are the operator-facing counterpart of tools: they are never advertised to the
        model, so credential binding and similar flows cannot be triggered by a chat message.
        Unknown actions and handler failures are reported as explicit, structured errors rather
        than raising through the gRPC layer.
        """
        handler = self._action_handlers.get(action_name)
        if handler is None:
            return pb.PluginActionResponse(
                success=False,
                error_message=(
                    f"Unknown action '{action_name}' for plugin '{plugin_id}'; "
                    f"declared actions: {sorted(self._action_handlers)}"
                ),
            )

        try:
            result = await _call(handler, parameters or {})
        except Exception as exc:
            return pb.PluginActionResponse(success=False, error_message=f"Action failed: {exc}")

        payload = Struct()
        if result:
            ParseDict(result, payload)
        return pb.PluginActionResponse(success=True, error_message="", result=payload)

    async def on_call_tool(
        self,
        req: pb.ToolCallRequest,
    ) -> pb.ToolCallResponse:
        """Executes an LLM tool call dispatched by the Core microkernel."""
        handler = self._tool_handlers.get(req.tool_name)
        if handler is None:
            return pb.ToolCallResponse(
                call_id=req.call_id,
                success=False,
                error_message=f"Unknown tool: {req.tool_name}",
            )

        args_dict: Dict[str, Any] = {}
        if req.HasField("structured_args"):
            args_dict = MessageToDict(req.structured_args)
        event = MessageEvent(req.context, self.core) if req.HasField("context") else None

        try:
            result = await _call(handler, args_dict, event)
        except Exception as exc:  # noqa: BLE001 - the model is told the tool failed
            return pb.ToolCallResponse(
                call_id=req.call_id,
                success=False,
                error_message=f"{type(exc).__name__}: {exc}",
            )

        if isinstance(result, pb.ToolCallResponse):
            return result
        if isinstance(result, bytes):
            return pb.ToolCallResponse(call_id=req.call_id, success=True, raw_bytes=result)
        result_struct = Struct()
        ParseDict(result if isinstance(result, dict) else {"result": str(result)}, result_struct)
        return pb.ToolCallResponse(
            call_id=req.call_id,
            success=True,
            structured_result=result_struct,
        )

    async def on_event(self, req: pb.EventNotification) -> None:
        """Dispatches a lifecycle event to the plugin's :func:`on_event` handlers."""
        kind = req.WhichOneof("detail")
        if kind is None:
            return
        detail: Any = getattr(req, kind)
        if kind == "notice":
            detail = MessageEvent(detail, self.core)
        for handler in self._event_handlers.get(kind, []):
            try:
                await _call(handler, detail)
            except Exception as exc:  # noqa: BLE001 - one failing subscriber must not stop others
                print(f"[kanon-sdk] {kind} handler failed: {exc!r}", file=sys.stderr)

    async def on_decorate_reply(self, req: pb.DecorateReplyRequest) -> pb.DecorateReplyResult:
        """Runs the plugin's :func:`decorate_reply` handler on one reply."""
        if self._decorator is None:
            return pb.DecorateReplyResult(modified=False)
        reply = Reply(
            event=MessageEvent(req.context, self.core),
            segments=list(req.segments),
            source={pb.REPLY_SOURCE_LLM: "llm", pb.REPLY_SOURCE_COMMAND: "command"}.get(
                req.source, ""
            ),
            command=req.command,
        )
        result = await _call(self._decorator, reply)
        if result is None:
            return pb.DecorateReplyResult(modified=False)
        return pb.DecorateReplyResult(modified=True, segments=to_segments(result))

    async def on_deliver_message(
        self,
        req: pb.DeliverMessageRequest,
    ) -> pb.DeliverMessageResponse:
        """Delivers an outbound message to a target platform (adapter hook).

        Platform adapter plugins MUST override this hook; the base implementation
        is an explicit, loud failure rather than a fake success. Core routes every
        pipeline reply for a platform to the host that registered that platform,
        so answering ``success=True`` without actually sending the message would
        silently swallow user-visible replies and hide a missing adapter.

        The returned ``error_message`` names the offending platform so Core logs
        and operator diagnostics point straight at the unconfigured adapter.

        Args:
            req: Outbound message with platform, channel, recipient and segments.

        Returns:
            ``pb.DeliverMessageResponse`` with ``success=False`` and an empty
            ``message_id``; never reports a delivery that did not happen.
        """
        return pb.DeliverMessageResponse(
            success=False,
            message_id="",
            error_message=(
                "plugin does not implement on_deliver_message for platform "
                f"'{req.platform}'"
            ),
        )
