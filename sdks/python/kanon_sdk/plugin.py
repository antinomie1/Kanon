"""Plugin base class and decorators for Kanon Python SDK.

A plugin is a :class:`Plugin` subclass whose methods are marked with decorators:

* :func:`command` — a slash command (``/weather Paris``), with aliases and an access level, or a
  subcommand of a command group (``@command("todo add")``);
* :func:`trigger` — a regular expression matched against plain messages;
* :func:`tool` — a function the model may call, its schema inferred from the signature;
* :func:`action` — an operator-only management action (never offered to the model);
* :func:`on_event` — a lifecycle event (messages sent, notices, model answers, agent runs and
  tool calls);
* :func:`decorate_reply` — a hook that rewrites the bot's replies before delivery;
* :func:`prepare_turn` — a hook that adds context to the message the model is about to answer;
* :func:`on_llm_request` — a hook that rewrites a conversation's system prompt;
* :func:`~kanon_sdk.web.http_route` — an HTTP route served through the node's gateway.

Tools can also be added and removed while the plugin runs (:meth:`Plugin.add_tool`), and
:attr:`Plugin.kv` is the plugin's corner of the node's key-value store.

Command and trigger handlers receive a :class:`~kanon_sdk.event.CommandEvent` and may answer by
returning text/segments or with ``await event.reply(...)``; ``await event.wait_next()`` asks the
user a follow-up question (see :mod:`kanon_sdk.event`).
"""

import asyncio
import inspect
import itertools
import re
import sys
import traceback
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Sequence, Set, Union

from google.protobuf.json_format import MessageToDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk.context import PluginContext, Replyable, to_segments
from kanon_sdk.event import CommandEvent, Conversations, MessageEvent, _Session, _Turn, run_turn
from kanon_sdk.kv import KV
from kanon_sdk.proto import parse_dict, pb
from kanon_sdk.schema import ToolSignature, infer_tool, restore_integers
from kanon_sdk.web import HttpRequest, HttpResponse, to_http_response

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
    "agent_begin": pb.EVENT_KIND_AGENT_BEGIN,
    "agent_done": pb.EVENT_KIND_AGENT_DONE,
    "tool_call": pb.EVENT_KIND_TOOL_CALL,
    "tool_result": pb.EVENT_KIND_TOOL_RESULT,
}

#: Declaration order of subcommands: ``dir()`` lists methods alphabetically, but a group's help
#: should list its subcommands the way the author wrote them.
_DECLARATION_ORDER = itertools.count()


#: Conversation kinds a command or trigger may be limited to.
CONVERSATION_KINDS = {
    "private": pb.CONVERSATION_KIND_PRIVATE,
    "group": pb.CONVERSATION_KIND_GROUP,
    "channel": pb.CONVERSATION_KIND_CHANNEL,
}


def _scope(platforms: Sequence[str], kinds: Sequence[str]) -> Dict[str, List[Any]]:
    """Validates a command or trigger's platform and conversation-kind limits."""
    if isinstance(platforms, str) or isinstance(kinds, str):
        raise TypeError("platforms and conversation_kinds take a list, not a single string")
    unknown = [kind for kind in kinds if kind not in CONVERSATION_KINDS]
    if unknown:
        raise ValueError(
            f"unknown conversation kinds {unknown}; expected some of {sorted(CONVERSATION_KINDS)}"
        )
    return {
        "platforms": list(platforms),
        "conversation_kinds": [CONVERSATION_KINDS[kind] for kind in kinds],
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
    platforms: Sequence[str] = (),
    conversation_kinds: Sequence[str] = (),
) -> Callable:
    """Declares a slash command handler, or a subcommand of a command group.

    A name with a space declares a subcommand: ``@command("todo add")`` answers ``/todo add milk``
    with ``event.args == ["milk"]``. ``/help`` lists a group's subcommands under it, and ``/todo``
    alone (or with an unknown subcommand) answers with that list — unless the plugin also
    declares ``@command("todo")``, which then handles those cases and carries the group's
    description, aliases, access level and scope. Subcommands take only ``description`` and
    ``usage``: the node routes and checks access for the group as a whole.

    Args:
        name: Command name without the slash, or ``"<group> <subcommand>"``.
        description: One line shown by ``/help``.
        usage: Usage example shown by ``/help``.
        priority: Lower wins when several plugins declare the same name.
        aliases: Other names that invoke the command; the handler always sees ``name``.
        access: ``"everyone"``, ``"admins_in_groups"`` or ``"admins"``. A default the operator
            can override in the node's command policy.
        platforms: Platforms the command answers on; empty means all. Elsewhere Core treats the
            command as undeclared, so another plugin's command of the same name may answer.
        conversation_kinds: ``"private"``, ``"group"`` and/or ``"channel"``; empty means all.
    """
    access_value = _access(access)
    scope = _scope(platforms, conversation_kinds)
    words = name.lstrip("/").split()
    if not words or len(words) > 2:
        raise ValueError(f"command name {name!r} must be one word, or a group and a subcommand")
    group, sub = (words[0], words[1]) if len(words) == 2 else (None, None)
    if sub is not None and (
        aliases or access != "everyone" or platforms or conversation_kinds or priority != 500
    ):
        raise ValueError(
            f"subcommand {name!r} takes only description and usage; set aliases, access, "
            f"priority and scope on @command({group!r})"
        )

    def decorator(fn: Callable) -> Callable:
        fn._kanon_command = {
            "name": sub or words[0],
            "group": group,
            "order": next(_DECLARATION_ORDER),
            "description": description,
            "usage": usage,
            "priority": priority,
            "aliases": [alias.lstrip("/") for alias in aliases],
            "access": access_value,
            **scope,
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
    platforms: Sequence[str] = (),
    conversation_kinds: Sequence[str] = (),
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
        platforms: As for :func:`command`; elsewhere the trigger never matches.
        conversation_kinds: As for :func:`command`.
    """
    access_value = _access(access)
    scope = _scope(platforms, conversation_kinds)
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
            **scope,
        }
        return fn

    return decorator


def _tool_info(
    handler: Callable,
    name: Optional[str],
    description: str,
    parameters: Optional[Dict[str, Any]],
) -> Dict[str, Any]:
    """A tool's name, description, schema and calling convention (see :func:`tool`)."""
    signature: Optional[ToolSignature] = None
    if parameters is None:
        signature = infer_tool(handler)
        parameters = signature.parameters
        description = description or signature.description
    tool_name = name or getattr(handler, "__name__", "")
    if not tool_name:
        raise ValueError("a tool needs a name")
    return {
        "name": tool_name,
        "description": description,
        "parameters": parameters,
        "signature": signature,
    }


def tool(
    name: Union[str, Callable, None] = None,
    description: str = "",
    parameters: Optional[Dict[str, Any]] = None,
) -> Callable:
    """Declares a function the model may call.

    Without ``parameters`` the tool describes itself (see :mod:`kanon_sdk.schema`): its name
    defaults to the method name, its description to the docstring's first paragraph, and its
    parameters come from the annotated arguments, which the handler receives by keyword. An
    argument named ``event`` receives the :class:`~kanon_sdk.event.MessageEvent` the model was
    answering instead (``None`` when no chat message is behind the call), so a tool knows who
    asked without trusting the model to say::

        @tool
        async def remember(self, fact: str, event: MessageEvent) -> str:
            \"\"\"Stores a fact about the user for later conversations.\"\"\"

    With an explicit JSON Schema in ``parameters`` the handler receives the arguments as one
    dict, plus the event if it takes a second parameter.

    The return value is the tool's result: a dict as is, ``bytes`` as raw bytes, anything else
    under ``"result"``. Raising reports the failure to the model.

    Operations an *operator* triggers — credential binding, QR login, diagnostics — belong in
    :func:`action` instead: a tool advertised by an adapter is offered to the model, which then
    tries to invoke it mid-conversation.

    Raises:
        TypeError: When the schema cannot be inferred (see :func:`kanon_sdk.schema.infer_tool`).
    """
    if callable(name):
        # Used bare, as ``@tool``.
        fn = name
        fn._kanon_tool = _tool_info(fn, None, "", None)
        return fn

    def decorator(fn: Callable) -> Callable:
        fn._kanon_tool = _tool_info(fn, name, description, parameters)
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
      answered;
    * ``"agent_begin"`` / ``"agent_done"`` — a ``pb.AgentBeginEvent`` / ``pb.AgentDoneEvent``
      when the agent starts and finishes a conversation turn (``done`` carries success, the
      answer or error, and the tools it called);
    * ``"tool_call"`` / ``"tool_result"`` — a ``pb.ToolCallEvent`` / ``pb.ToolResultEvent`` for
      every tool the agent calls during a turn, with its arguments and its result.

    Each event's ``context`` is the chat message behind it (wrap it in
    :class:`~kanon_sdk.event.MessageEvent` to use the helpers). Events are notifications: the
    handler's return value is ignored, and Core never waits on it.
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


def prepare_turn(fn: Callable) -> Callable:
    """Marks the plugin's turn preparer.

    Before the model answers a message, the handler receives the
    :class:`~kanon_sdk.event.MessageEvent` and the conversation's session id (if it accepts a
    second parameter) and returns text to prepend to the current user message — retrieved
    knowledge, long-term memory. ``None`` or ``""`` adds nothing. The text never reaches the
    system prompt, so the cached request prefix stays stable, and it becomes part of the
    conversation history. Core gives each preparer three seconds and goes ahead without it on
    error or timeout.
    """
    fn._kanon_preparer = True
    return fn


def on_llm_request(fn: Callable) -> Callable:
    """Marks the plugin's system prompt rewriter.

    Before the model answers the first message of a turn, the handler receives the
    :class:`~kanon_sdk.event.MessageEvent`, the conversation's current system prompt (persona,
    skills and earlier plugins' rewrites) and the session id, and returns the new system prompt —
    or ``None`` to leave it alone::

        @on_llm_request
        async def house_rules(self, event: MessageEvent, system_prompt: str) -> str:
            return system_prompt + "\\n\\nAnswer in the group's language."

    The system prompt opens every request, so the provider's prompt cache depends on its bytes:
    return the same text for the same conversation — no clocks, counters or per-message details
    (those belong in :func:`prepare_turn`). Core asks once per turn and reuses the answer for the
    turn's tool rounds; it gives the handler three seconds and keeps the prompt on failure.
    """
    fn._kanon_prompt_rewriter = True
    return fn


async def _call(handler: Callable, *args: Any) -> Any:
    """Calls a sync or async handler with as many of ``args`` as it accepts."""
    accepted = len(inspect.signature(handler).parameters)
    result = handler(*args[:accepted])
    if inspect.isawaitable(result):
        result = await result
    return result


@dataclass
class _Tool:
    """A tool the plugin offers, however it was declared."""

    name: str
    description: str
    parameters: Dict[str, Any]
    handler: Callable
    #: Inferred calling convention (keyword arguments); ``None`` for an explicit schema, whose
    #: handler takes the arguments as one dict.
    signature: Optional[ToolSignature]

    async def call(self, args: Dict[str, Any], event: Optional[MessageEvent]) -> Any:
        args = restore_integers(args, self.parameters)
        if self.signature is None:
            return await _call(self.handler, args, event)
        unknown = sorted(set(args) - set(self.signature.arguments))
        if unknown:
            # Said plainly so the model can correct itself instead of seeing a Python error.
            raise TypeError(f"unexpected arguments {unknown}; expected {self.signature.arguments}")
        kwargs = dict(args)
        if self.signature.wants_event:
            kwargs["event"] = event
        result = self.handler(**kwargs)
        if inspect.isawaitable(result):
            result = await result
        return result


def _without_subcommand(req: pb.CommandExecuteRequest) -> pb.CommandExecuteRequest:
    """The request a subcommand handler sees: the subcommand word removed from the arguments."""
    shifted = pb.CommandExecuteRequest()
    shifted.CopyFrom(req)
    sub = shifted.args[0]
    del shifted.args[0]
    raw = req.raw_args.lstrip()
    shifted.raw_args = raw[len(sub):].lstrip() if raw.startswith(sub) else req.raw_args
    return shifted


def _group_help(group: str, subcommands: Dict[str, Callable]) -> str:
    """One line per subcommand, as ``/help`` shows them."""
    lines = []
    for sub, handler in subcommands.items():
        info = handler._kanon_command
        line = info["usage"] or f"/{group} {sub}"
        if info["description"]:
            line += f" — {info['description']}"
        lines.append(line)
    return "\n".join(lines)


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
        # Command groups: group name -> subcommand name -> handler, in declaration order.
        self._subcommands: Dict[str, Dict[str, Callable]] = {}
        self._trigger_handlers: Dict[str, Callable] = {}
        self._tools: Dict[str, _Tool] = {}
        self._action_handlers: Dict[str, Callable] = {}
        self._event_handlers: Dict[str, List[Callable]] = {}
        self._decorator: Optional[Callable] = None
        self._preparer: Optional[Callable] = None
        self._prompt_rewriter: Optional[Callable] = None
        # HTTP routes: path -> method -> handler.
        self._http_routes: Dict[str, Dict[str, Callable]] = {}
        self._conversations = Conversations()
        # Handler tasks outlive the RPC that started them (see kanon_sdk.event); keeping a
        # reference stops the event loop from garbage-collecting a suspended handler.
        self._tasks: Set[asyncio.Task] = set()
        self._collect_decorated_handlers()

    def _collect_decorated_handlers(self) -> None:
        """Discovers decorated methods and checks that their names do not collide."""
        subcommands: Dict[str, List[Callable]] = {}
        for attr_name in dir(self):
            try:
                attr = getattr(self, attr_name)
            except Exception:
                continue
            if not callable(attr):
                continue

            if hasattr(attr, "_kanon_command"):
                info = attr._kanon_command
                if info["group"] is None:
                    self._command_handlers[info["name"]] = attr
                else:
                    subcommands.setdefault(info["group"], []).append(attr)
            if hasattr(attr, "_kanon_trigger"):
                self._trigger_handlers[attr._kanon_trigger["name"]] = attr
            if hasattr(attr, "_kanon_tool"):
                info = attr._kanon_tool
                if info["name"] in self._tools:
                    raise ValueError(f"{type(self).__name__} declares tool {info['name']!r} twice")
                self._tools[info["name"]] = _Tool(handler=attr, **info)
            if hasattr(attr, "_kanon_action"):
                self._action_handlers[attr._kanon_action["name"]] = attr
            for kind in getattr(attr, "_kanon_events", []):
                self._event_handlers.setdefault(kind, []).append(attr)
            if getattr(attr, "_kanon_decorator", False):
                if self._decorator is not None:
                    raise ValueError(f"{type(self).__name__} declares more than one @decorate_reply")
                self._decorator = attr
            if getattr(attr, "_kanon_preparer", False):
                if self._preparer is not None:
                    raise ValueError(f"{type(self).__name__} declares more than one @prepare_turn")
                self._preparer = attr
            if getattr(attr, "_kanon_prompt_rewriter", False):
                if self._prompt_rewriter is not None:
                    raise ValueError(
                        f"{type(self).__name__} declares more than one @on_llm_request"
                    )
                self._prompt_rewriter = attr
            for path, methods in getattr(attr, "_kanon_http", []):
                route = self._http_routes.setdefault(path, {})
                for method in methods:
                    if method in route:
                        raise ValueError(
                            f"{type(self).__name__} declares {method} {path} more than once"
                        )
                    route[method] = attr

        for group, handlers in subcommands.items():
            handlers.sort(key=lambda handler: handler._kanon_command["order"])
            table: Dict[str, Callable] = {}
            for handler in handlers:
                sub = handler._kanon_command["name"]
                if sub in table:
                    raise ValueError(f"{type(self).__name__} declares /{group} {sub} twice")
                table[sub] = handler
            self._subcommands[group] = table

        # Core sends commands and triggers through the same RPC, naming either in `command`, so
        # the two share one namespace (a command group is a command).
        clash = (set(self._command_handlers) | set(self._subcommands)) & set(self._trigger_handlers)
        if clash:
            raise ValueError(f"names used by both a command and a trigger: {sorted(clash)}")
        for kind in self.events:
            if kind not in EVENT_KINDS:
                raise ValueError(f"unknown event kind {kind!r} in {type(self).__name__}.events")

    @property
    def core(self):
        """The host's :class:`~kanon_sdk.context.CoreHandle`, or ``None`` in standalone mode."""
        return self.context.core if self.context is not None else None

    @property
    def kv(self) -> KV:
        """The plugin's namespace in the node's central key-value store (see :class:`KV`).

        Raises:
            RuntimeError: In standalone mode, where there is no node to store anything in.
        """
        core = self.core
        if core is None:
            raise RuntimeError("no Core connection: the KV store is unavailable in standalone mode")
        return core.kv

    async def add_tool(
        self,
        handler: Callable,
        *,
        name: Optional[str] = None,
        description: str = "",
        parameters: Optional[Dict[str, Any]] = None,
    ) -> None:
        """Offers a new tool to the model while the plugin runs.

        ``handler`` is described exactly as :func:`tool` describes a decorated method, and the
        node picks the tool up from the next turn on. Use it for tools that depend on
        configuration or on something discovered at runtime.

        Raises:
            ValueError: If a tool of that name exists.
            grpc.aio.AioRpcError: If the node could not reread the plugin's tools; the tool is
                then not added, so the plugin and the node keep agreeing.
        """
        info = _tool_info(handler, name, description, parameters)
        if info["name"] in self._tools:
            raise ValueError(f"tool {info['name']!r} already exists")
        self._tools[info["name"]] = _Tool(handler=handler, **info)
        try:
            await self._refresh_meta()
        except BaseException:
            del self._tools[info["name"]]
            raise

    async def remove_tool(self, name: str) -> bool:
        """Withdraws a tool from the model; returns whether it existed.

        Raises:
            grpc.aio.AioRpcError: If the node could not reread the plugin's tools; the tool then
                stays, so the plugin keeps serving what the node still offers.
        """
        removed = self._tools.pop(name, None)
        if removed is None:
            return False
        try:
            await self._refresh_meta()
        except BaseException:
            self._tools[name] = removed
            raise
        return True

    async def _refresh_meta(self) -> None:
        # Standalone (no node) there is nobody to tell; the local table is all there is.
        if self.core is not None:
            await self.core.refresh_meta()

    def meta(self) -> pb.PluginMeta:
        """Constructs and returns static metadata for this plugin."""
        commands: List[pb.CommandMeta] = []
        for name in sorted(set(self._command_handlers) | set(self._subcommands)):
            handler = self._command_handlers.get(name)
            info = handler._kanon_command if handler is not None else {"name": name}
            commands.append(
                pb.CommandMeta(
                    name=name,
                    description=info.get("description", ""),
                    usage=info.get("usage", ""),
                    priority=info.get("priority", 500),
                    aliases=info.get("aliases", []),
                    access=info.get("access", pb.COMMAND_ACCESS_EVERYONE),
                    platforms=info.get("platforms", []),
                    conversation_kinds=info.get("conversation_kinds", []),
                    subcommands=[
                        pb.CommandMeta(
                            name=sub,
                            description=sub_handler._kanon_command["description"],
                            usage=sub_handler._kanon_command["usage"],
                        )
                        for sub, sub_handler in self._subcommands.get(name, {}).items()
                    ],
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
                    platforms=info["platforms"],
                    conversation_kinds=info["conversation_kinds"],
                )
            )

        tools: List[pb.ToolMeta] = []
        for spec in self._tools.values():
            param_struct = Struct()
            if spec.parameters:
                parse_dict(spec.parameters, param_struct)
            tools.append(
                pb.ToolMeta(
                    name=spec.name,
                    description=spec.description,
                    parameters=param_struct if spec.parameters else None,
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
            prepares_turns=self._preparer is not None,
            rewrites_system_prompt=self._prompt_rewriter is not None,
            serves_http=bool(self._http_routes),
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
        subcommands = self._subcommands.get(req.command)
        if subcommands is not None:
            sub = subcommands.get(req.args[0]) if req.args else None
            if sub is not None:
                handler, req = sub, _without_subcommand(req)
            elif handler is None:
                # No group handler: `/todo` alone or with an unknown subcommand lists the group.
                return pb.CommandExecuteResponse(
                    success=True,
                    replies=to_segments(_group_help(req.command, subcommands)),
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
            payload = Struct()
            if result:
                parse_dict(result, payload)
        except Exception as exc:
            return pb.PluginActionResponse(success=False, error_message=f"Action failed: {exc}")

        return pb.PluginActionResponse(success=True, error_message="", result=payload)

    async def on_call_tool(
        self,
        req: pb.ToolCallRequest,
    ) -> pb.ToolCallResponse:
        """Executes an LLM tool call dispatched by the Core microkernel."""
        spec = self._tools.get(req.tool_name)
        if spec is None:
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
            result = await spec.call(args_dict, event)
            if isinstance(result, pb.ToolCallResponse):
                return result
            if isinstance(result, bytes):
                return pb.ToolCallResponse(call_id=req.call_id, success=True, raw_bytes=result)
            result_struct = Struct()
            if not isinstance(result, dict):
                # JSON values keep their shape (a list stays a list); anything else becomes text.
                plain = isinstance(result, (str, int, float, bool, list, tuple)) or result is None
                result = {"result": list(result) if isinstance(result, tuple) else result}
                if not plain:
                    result = {"result": str(result["result"])}
            parse_dict(result, result_struct)
        except Exception as exc:  # noqa: BLE001 - the model is told the tool failed
            return pb.ToolCallResponse(
                call_id=req.call_id,
                success=False,
                error_message=f"{type(exc).__name__}: {exc}",
            )

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

    async def on_prepare_turn(self, req: pb.PrepareTurnRequest) -> pb.PrepareTurnResult:
        """Runs the plugin's :func:`prepare_turn` handler for the turn the model will answer."""
        if self._preparer is None:
            return pb.PrepareTurnResult()
        text = await _call(self._preparer, MessageEvent(req.context, self.core), req.session_id)
        return pb.PrepareTurnResult(text=text or "")

    async def on_llm_request(self, req: pb.LlmRequestHookRequest) -> pb.LlmRequestHookResult:
        """Runs the plugin's :func:`on_llm_request` handler; unset means "leave it alone"."""
        if self._prompt_rewriter is None:
            return pb.LlmRequestHookResult()
        prompt = await _call(
            self._prompt_rewriter,
            MessageEvent(req.context, self.core),
            req.system_prompt,
            req.session_id,
        )
        if prompt is None:
            return pb.LlmRequestHookResult()
        if not isinstance(prompt, str):
            raise TypeError(
                f"@on_llm_request must return a str or None, not {type(prompt).__name__}"
            )
        return pb.LlmRequestHookResult(system_prompt=prompt)

    async def on_http_request(self, req: pb.HttpRequest) -> pb.HttpResponse:
        """Routes a forwarded HTTP request to its :func:`~kanon_sdk.web.http_route` handler."""
        request = HttpRequest.from_proto(req)
        route = self._http_routes.get(request.path)
        if route is None:
            return HttpResponse.text("Not Found", status=404).to_proto()
        handler = route.get(request.method)
        if handler is None:
            response = HttpResponse.text("Method Not Allowed", status=405)
            response.headers["allow"] = ", ".join(sorted(route))
            return response.to_proto()
        try:
            result = await _call(handler, request)
            return to_http_response(result).to_proto()
        except Exception:  # noqa: BLE001 - the caller gets a 500, the author gets the traceback
            print(
                f"[kanon-sdk] HTTP {request.method} {request.path} failed:\n"
                f"{traceback.format_exc()}",
                file=sys.stderr,
            )
            return HttpResponse.text("Internal Server Error", status=500).to_proto()

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
