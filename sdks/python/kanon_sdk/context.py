"""Context and message segment abstractions for Kanon Python SDK."""

import asyncio
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, AsyncIterator, Dict, List, Optional, Sequence, Tuple, Union

from google.protobuf.json_format import MessageToDict, ParseDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk.kv import KV
from kanon_sdk.proto import pb, pb_grpc


@dataclass
class ConversationHistory:
    """A model conversation as returned by :meth:`CoreHandle.conversation_history`.

    Attributes:
        session_id: The session the conversation is stored under (stable until ``/new``).
        summary: Summary of compacted older turns; ``""`` if never compacted.
        messages: ``(role, text)`` pairs, oldest first, with role ``"user"`` or ``"assistant"``.
    """

    session_id: str
    summary: str = ""
    messages: List[Tuple[str, str]] = field(default_factory=list)


@dataclass
class ConversationInfo:
    """One conversation of a chat, as listed by :meth:`CoreHandle.list_conversations`.

    Attributes:
        session_id: Identifies the conversation for ``switch_conversation``/``delete_conversation``.
        current: Whether the chat's next message continues this conversation.
        title: The first user message, shortened; ``""`` while it has no messages.
        message_count: Stored user and assistant messages (summarized ones excluded).
        last_active_at: Unix seconds of the last turn; ``0`` if it never had one.
    """

    session_id: str
    current: bool
    title: str
    message_count: int
    last_active_at: int


@dataclass
class Persona:
    """A persona of the node's catalog (see :meth:`CoreHandle.list_personas`).

    Attributes:
        id: Stable identifier instances and conversations refer to.
        name: Display name.
        prompt: The system text that opens every request of a conversation using it.
        builtin: Shipped with the node; it cannot be changed or deleted.
    """

    id: str
    name: str
    prompt: str
    builtin: bool = False


@dataclass
class AgentResult:
    """What :meth:`CoreHandle.run_agent` produced. Nothing was sent to the chat.

    Attributes:
        content: The agent's final answer, without reasoning.
        attachments: Media the tools produced (``mime_type`` plus ``file_path`` or ``url``),
            for the plugin to send if it wants to.
        tools: Names of the tools called, in order.
        session_id: The conversation the run continued, or the discarded private session.
    """

    content: str
    attachments: List[pb.ToolAttachment] = field(default_factory=list)
    tools: List[str] = field(default_factory=list)
    session_id: str = ""


#: Roles of conversation messages, in both directions.
_ROLES = {pb.LLM_ROLE_USER: "user", pb.LLM_ROLE_ASSISTANT: "assistant"}
_ROLE_VALUES = {name: value for value, name in _ROLES.items()}


def _raw_event(event: Any) -> pb.PipelineEventRequest:
    """The protobuf message behind a :class:`~kanon_sdk.event.MessageEvent` (or the message)."""
    return getattr(event, "raw", event)


def _conversations(response: pb.ConversationList) -> List[ConversationInfo]:
    return [
        ConversationInfo(
            session_id=item.session_id,
            current=item.current,
            title=item.title,
            message_count=item.message_count,
            last_active_at=item.last_active_at,
        )
        for item in response.conversations
    ]


class CoreHandle:
    """Adapter-facing handle for pushing inbound events into the Kanon Core pipeline.

    A handle wraps one *already connected* ``BotApiServiceStub``; it never dials,
    never re-dials and never closes a channel itself. Connection ownership stays
    with the plugin host, which keeps exactly one IPC endpoint and one lifecycle
    in the process.

    Why a single shared channel instead of dialing per call
    -------------------------------------------------------
    Adapters ingest at message rate, and every ``grpc.aio.insecure_channel``
    allocates a fresh HTTP/2 connection (file descriptor, connectivity task and
    handshake round trips) to ``core.sock``. Dialing per ``ingest_event`` call
    would multiply descriptors and latency for no benefit, and would leave
    orphaned channels behind whenever a task is cancelled mid-flight. Core
    exposes one endpoint and multiplexes concurrent streams over HTTP/2, so a
    single long-lived channel is both the cheapest and the intended topology;
    it also reconnects on its own after a Core restart, so adapters need no
    dialing or reconnection logic of their own.

    Because several adapter tasks (one poller per platform/chat) may share that
    channel, the handle serializes ingest calls behind an ``asyncio.Lock``:

    * ``accepted == False`` backpressure stays actionable - one call in flight at
      a time means a "queue full" answer is never masked by a concurrent burst;
    * events reach Core in the same order the adapter tasks scheduled them,
      which matters for per-channel conversational ordering;
    * the shared stub is never used concurrently with host shutdown, so closing
      the channel cannot race an in-flight RPC from another task.

    Fast-ACK and why ``IngestEventResponse.accepted`` matters
    ---------------------------------------------------------
    Core never blocks on the pipeline: ``IngestEvent`` performs a non-blocking
    enqueue into a bounded queue and answers immediately. A response therefore
    means *"Core received your event"*, not *"Core processed your event"*. When
    the queue is full Core returns ``accepted=False`` and drops the event to
    protect the microkernel; adapters must inspect that flag (and may retry later
    at their own discretion). This handle deliberately returns the raw response
    instead of raising or retrying, so the caller owns that policy.

    Transport failures are *not* converted into a response: if Core is
    unreachable, the RPC raises ``grpc.aio.AioRpcError``. The SDK never
    fabricates ``accepted=True`` (or any synthetic response) for a call that did
    not reach Core.

    Usage (platform adapter plugin)::

        import asyncio

        import grpc

        from kanon_sdk import Plugin, PluginContext


        class TelegramAdapter(Plugin):
            async def on_load(self, ctx: PluginContext) -> None:
                # Capture the handle once: ctx.core is None in standalone mode,
                # i.e. when the host could not reach Core at startup.
                self.core = ctx.core
                self._poller = asyncio.create_task(self._poll_updates())

            async def _poll_updates(self) -> None:
                while True:
                    update = await telegram_get_updates()
                    if self.core is None:
                        continue  # Standalone mode: nothing to ingest into.
                    try:
                        resp = await self.core.ingest_event(
                            platform="telegram",
                            channel_id=str(update.chat_id),
                            sender_id=str(update.user_id),
                            text=update.text,
                            metadata={"update_id": update.update_id},
                        )
                    except grpc.aio.AioRpcError:
                        continue  # Core was not reached; retry on the next poll.
                    if not resp.accepted:
                        continue  # Fast-ACK backpressure: Core queue is full.

            async def on_deliver_message(self, req):
                # Adapters must also override the outbound hook; see Plugin.
                ...
    """

    def __init__(
        self,
        stub: pb_grpc.BotApiServiceStub,
        *,
        plugin_id: str = "",
        host_id: str = "",
    ) -> None:
        """Wraps an existing ``BotApiService`` stub without touching its channel.

        Args:
            stub: The host's connected stub.
            plugin_id: The plugin this handle acts for: the namespace of :attr:`kv` and the
                owner of agent runs and rendered images. The host always sets it.
            host_id: The host's id, which :meth:`refresh_meta` names.
        """
        # The stub (and therefore the channel) belongs to the host process: the
        # handle only issues RPCs, so channel creation/closure has a single owner.
        self._stub = stub
        self.plugin_id = plugin_id
        self.host_id = host_id
        self._kv: Optional[KV] = None
        # Serializes concurrent adapter tasks sharing this handle. The lock is
        # created lazily against the running loop, so constructing a CoreHandle
        # outside a loop (e.g. at import time) stays safe.
        self._lock = asyncio.Lock()

    def _require_plugin_id(self, what: str) -> str:
        if not self.plugin_id:
            raise RuntimeError(f"{what} needs a CoreHandle created with plugin_id")
        return self.plugin_id

    @property
    def kv(self) -> KV:
        """The plugin's namespace in the node's central key-value store (see :class:`KV`)."""
        if self._kv is None:
            self._kv = KV(self._stub, self._require_plugin_id("the KV store"))
        return self._kv

    async def reply_to(
        self,
        event: pb.PipelineEventRequest,
        segments: "Replyable",
    ) -> pb.DeliverMessageResponse:
        """Reply with the original platform context and await actual delivery.

        The existing shared Core channel owns authentication and reconnects.
        Unlike SendMessage admission, success here is the platform adapter's
        delivery result. RPC failures/timeouts are ambiguous: never retry a
        reply automatically, or mark an upload review delivered on that basis.
        """
        return await self._stub.ReplyMessage(
            pb.DeliverMessageRequest(
                platform=event.platform,
                channel_id=event.channel_id,
                recipient_id=event.sender_id,
                event_id=event.event_id,
                segments=to_segments(segments),
            ),
            timeout=35.0,
        )

    async def send_message(
        self,
        platform: str,
        channel_id: str,
        segments: "Replyable",
        recipient_id: str = "",
    ) -> pb.SendMessageResponse:
        """Sends a message on the bot's own initiative (reminders, broadcasts, subscriptions).

        Success means Core accepted the message into its outbound queue, not that the platform
        delivered it; use :meth:`reply_to` when the delivery outcome matters.

        Args:
            platform: Platform identifier, e.g. ``"onebot"``.
            channel_id: Conversation to send to, as events report it (``"group:123"``).
            segments: Text, a segment, or a list of either (see :func:`to_segments`).
            recipient_id: Optional user the message is addressed to.
        """
        return await self._stub.SendMessage(
            pb.SendMessageRequest(
                platform=platform,
                channel_id=channel_id,
                recipient_id=recipient_id,
                segments=to_segments(segments),
            )
        )

    async def request_llm(
        self,
        prompt: Optional[str] = None,
        *,
        messages: Optional[Sequence[pb.LLMMessage]] = None,
        system_prompt: str = "",
        model: str = "",
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
    ) -> str:
        """Asks the node's model one question and returns the complete answer.

        The call is independent of every chat conversation: nothing is read from or written to
        any session's memory. Pass either ``prompt`` (a single user turn) or ``messages`` (a
        whole exchange, oldest first; see :func:`llm_message`).

        Raises:
            ValueError: If neither or both of ``prompt`` and ``messages`` are given.
            grpc.aio.AioRpcError: ``UNAVAILABLE`` when the node has no model configured,
                ``INVALID_ARGUMENT`` for messages the model cannot take.
        """
        parts: List[str] = []
        async for delta in self.stream_llm(
            prompt,
            messages=messages,
            system_prompt=system_prompt,
            model=model,
            temperature=temperature,
            max_tokens=max_tokens,
        ):
            parts.append(delta)
        return "".join(parts)

    async def stream_llm(
        self,
        prompt: Optional[str] = None,
        *,
        messages: Optional[Sequence[pb.LLMMessage]] = None,
        system_prompt: str = "",
        model: str = "",
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
    ) -> AsyncIterator[str]:
        """Like :meth:`request_llm`, but yields the answer as it is generated."""
        if (prompt is None) == (messages is None):
            raise ValueError("pass exactly one of prompt and messages")
        turns = [llm_message(prompt)] if prompt is not None else list(messages or [])
        request = pb.LLMRequest(
            model=model,
            system_prompt=system_prompt,
            messages=turns,
        )
        # Optional scalars: leaving them unset keeps the provider's own defaults.
        if temperature is not None:
            request.temperature = temperature
        if max_tokens is not None:
            request.max_tokens = max_tokens
        async for chunk in self._stub.RequestLLM(request):
            if chunk.delta_text:
                yield chunk.delta_text

    async def call_platform_api(self, platform: str, action: str, **params: Any) -> Any:
        """Calls one action of a built-in adapter's platform API and returns its result.

        This reaches what the generic contract does not model, e.g. OneBot's
        ``get_group_member_list`` or Milky's ``set_group_member_mute``. The result is plain
        JSON data (dicts, lists, numbers as floats); ``None`` when the action returns nothing.

        Raises:
            grpc.aio.AioRpcError: ``NOT_FOUND`` for an unknown platform, ``UNIMPLEMENTED`` when
                the adapter offers no API, ``UNAVAILABLE`` when the platform refused the call.
        """
        request = pb.PlatformApiRequest(platform=platform, action=action)
        ParseDict(params, request.params)
        response = await self._stub.CallPlatformApi(request)
        if not response.HasField("result"):
            return None
        return MessageToDict(response.result)

    async def conversation_history(self, event: Any, limit: int = 0) -> ConversationHistory:
        """Reads the model conversation ``event`` belongs to.

        It is the same session the model would continue when answering ``event``. Only user
        and assistant turns are returned; tool calls, tool results and the model's reasoning
        are left out. History is read-only.

        Args:
            event: A :class:`~kanon_sdk.event.MessageEvent` (or a ``pb.PipelineEventRequest``).
            limit: Keep only this many of the most recent messages; ``0`` keeps all.

        Raises:
            grpc.aio.AioRpcError: ``NOT_FOUND`` when no bot instance answers on the platform,
                ``UNAVAILABLE`` when no model is configured.
        """
        raw = getattr(event, "raw", event)
        response = await self._stub.GetConversationHistory(
            pb.ConversationHistoryRequest(context=raw, limit=limit)
        )
        return ConversationHistory(
            session_id=response.session_id,
            summary=response.summary,
            messages=[(_ROLES.get(m.role, ""), m.text) for m in response.messages],
        )

    # --- Conversations of a chat ---------------------------------------------------------------
    #
    # A chat (a private chat, a group member, or a whole group when the group shares one session)
    # can hold several conversations, exactly what the built-in /ls, /new, /switch and /del
    # manage. ``event`` names the chat; every call returns the list as it is afterwards. Errors:
    # NOT_FOUND (no instance serves the chat, unknown session_id), FAILED_PRECONDITION (a turn is
    # writing the conversation right now; try again later).

    async def list_conversations(self, event: Any) -> List[ConversationInfo]:
        """The chat's conversations, oldest first (``/ls`` numbers them from 1)."""
        response = await self._stub.ListConversations(
            pb.ConversationsRequest(context=_raw_event(event))
        )
        return _conversations(response)

    async def new_conversation(self, event: Any) -> List[ConversationInfo]:
        """Starts an empty conversation and makes it current, like ``/new``."""
        response = await self._stub.NewConversation(
            pb.ConversationsRequest(context=_raw_event(event))
        )
        return _conversations(response)

    async def switch_conversation(self, event: Any, session_id: str) -> List[ConversationInfo]:
        """Makes ``session_id`` the chat's current conversation, like ``/switch``.

        In groups the built-in ``/switch`` is for admins by default; a plugin acting for a user
        should check the sender's role itself.
        """
        response = await self._stub.SwitchConversation(
            pb.SelectConversationRequest(context=_raw_event(event), session_id=session_id)
        )
        return _conversations(response)

    async def delete_conversation(self, event: Any, session_id: str) -> List[ConversationInfo]:
        """Deletes a conversation with its history, like ``/del``.

        Deleting the current conversation moves the chat to a new, empty one. The same
        permission advice as for :meth:`switch_conversation` applies.
        """
        response = await self._stub.DeleteConversation(
            pb.SelectConversationRequest(context=_raw_event(event), session_id=session_id)
        )
        return _conversations(response)

    async def append_conversation(
        self, event: Any, messages: Sequence[Tuple[str, str]]
    ) -> str:
        """Appends whole turns to the chat's current conversation; returns its session id.

        The model reads them as the conversation's own history from its next turn on. Existing
        messages are never changed.

        Args:
            event: The chat.
            messages: ``(role, text)`` pairs, oldest first, role ``"user"`` or ``"assistant"``.
        """
        history = []
        for role, text in messages:
            if role not in _ROLE_VALUES:
                raise ValueError(f"unknown role {role!r}; expected 'user' or 'assistant'")
            history.append(pb.HistoryMessage(role=_ROLE_VALUES[role], text=text))
        response = await self._stub.AppendConversation(
            pb.AppendConversationRequest(context=_raw_event(event), messages=history)
        )
        return response.session_id

    # --- Personas ------------------------------------------------------------------------------

    async def list_personas(self) -> List[Persona]:
        """The node's persona catalog: built-in, operator-made and plugin-made personas."""
        response = await self._stub.ListPersonas(pb.ListPersonasRequest())
        return [
            Persona(id=item.id, name=item.name, prompt=item.prompt, builtin=item.builtin)
            for item in response.personas
        ]

    async def upsert_persona(self, id: str, name: str, prompt: str) -> bool:  # noqa: A002
        """Creates or replaces a persona; returns ``True`` when one was replaced.

        Conversations using it pick up the new prompt from their next turn.
        """
        response = await self._stub.UpsertPersona(pb.Persona(id=id, name=name, prompt=prompt))
        return response.replaced

    async def delete_persona(self, id: str) -> bool:  # noqa: A002
        """Deletes a persona; returns whether it existed."""
        response = await self._stub.DeletePersona(pb.DeletePersonaRequest(id=id))
        return response.deleted

    # --- The agent -----------------------------------------------------------------------------

    async def run_agent(
        self,
        prompt: str = "",
        *,
        event: Any = None,
        in_conversation: bool = False,
        images: Sequence[pb.MessageSegment] = (),
        system_prompt: str = "",
        model: str = "",
        use_tools: bool = True,
        max_steps: int = 0,
    ) -> AgentResult:
        """Lets the node's agent (the model plus its tool loop) answer ``prompt``.

        Unlike :meth:`request_llm`, the agent can call tools — plugin, MCP and built-in ones.
        The answer is returned, never sent: the plugin decides what reaches the chat.

        Args:
            prompt: The user message; may be empty only when ``images`` are given.
            event: The chat the run serves. It picks the bot instance (its model, plugins and
                tool policy) and is what tools see as their context.
            in_conversation: Run inside the chat's current conversation, with its history and
                persona, and append the turn to it — exactly as if the model had answered a
                message. Needs ``event``. Otherwise the run uses a private session that is
                discarded afterwards.
            images: Image segments for this turn (see :meth:`MessageSegment.image_bytes`; raw
                bytes need an ``image/*`` mime type). The model must accept images.
            system_prompt: Instructions for a private run; ignored in a conversation.
            model: ``"<provider>/<model-id>"``; empty uses the instance's or node's model.
            use_tools: Offer tools to the model; ``False`` makes it a plain answer.
            max_steps: Tool rounds allowed; ``0`` is the agent's default.

        Raises:
            grpc.aio.AioRpcError: ``INVALID_ARGUMENT`` (empty run, bad image, unknown model),
                ``NOT_FOUND`` (no instance serves the chat), ``UNAVAILABLE`` (no model, or the
                model or a tool failed), ``FAILED_PRECONDITION`` (the conversation is busy),
                ``ABORTED`` (stopped with ``/stop``).
        """
        request = pb.RunAgentRequest(
            plugin_id=self._require_plugin_id("run_agent"),
            prompt=prompt,
            images=_image_parts(images),
            in_conversation=in_conversation,
            system_prompt=system_prompt,
            model=model,
            use_tools=use_tools,
            max_steps=max_steps,
        )
        if event is not None:
            request.context.CopyFrom(_raw_event(event))
        response = await self._stub.RunAgent(request)
        return AgentResult(
            content=response.content,
            attachments=list(response.attachments),
            tools=list(response.tools),
            session_id=response.session_id,
        )

    # --- Rendering -----------------------------------------------------------------------------

    async def render_text(self, text: str, width: int = 0) -> pb.MessageSegment:
        """Lays ``text`` out as a PNG card and returns it as an image segment, ready to send.

        Lines wrap to ``width`` pixels (default 720, 200–2000), blank lines separate paragraphs
        and a line starting with ``"# "`` is a heading; CJK and emoji use the node's fonts.
        The file is cleaned up after a day, so send it soon.
        """
        response = await self._stub.RenderImage(
            pb.RenderImageRequest(
                plugin_id=self._require_plugin_id("render_text"), text=text, width=width
            )
        )
        return MessageSegment.image_file(response.file_path, mime_type="image/png")

    async def render_svg(self, svg: str) -> pb.MessageSegment:
        """Renders an SVG document to PNG at its own size; returns an image segment.

        Embedded images must be ``data:`` URIs; references to files on the node are ignored.
        """
        response = await self._stub.RenderImage(
            pb.RenderImageRequest(plugin_id=self._require_plugin_id("render_svg"), svg=svg)
        )
        return MessageSegment.image_file(response.file_path, mime_type="image/png")

    # --- Metadata ------------------------------------------------------------------------------

    async def refresh_meta(self) -> List[str]:
        """Asks the node to read this host's metadata again; returns the plugin ids it now holds.

        :meth:`kanon_sdk.Plugin.add_tool` and ``remove_tool`` call this; the change applies
        from the next turn on.
        """
        if not self.host_id:
            raise RuntimeError("refresh_meta needs a CoreHandle created with host_id")
        response = await self._stub.RefreshPluginMeta(
            pb.RefreshPluginMetaRequest(host_id=self.host_id)
        )
        return list(response.plugin_ids)

    async def ingest_event(
        self,
        platform: str,
        channel_id: str,
        sender_id: str,
        text: str,
        event_id: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        segments: Optional[List[Dict[str, Any]]] = None,
    ) -> pb.IngestEventResponse:
        """Pushes one inbound platform message into the Core pipeline.

        Args:
            platform: Adapter platform identifier, e.g. ``"telegram"``.
            channel_id: Platform-side conversation/channel/group identifier.
            sender_id: Platform-side identifier of the message author.
            text: Raw inbound text. Kept for adapters and events that carry only
                plain text; for anything the model must *see* (mentions, images,
                audio, replies), describe it through ``segments`` instead of
                encoding it into this string.
            event_id: Optional caller-supplied idempotency/deduplication id. When
                omitted a random ``uuid4().hex`` is generated, so every call
                still carries a non-empty id.
            metadata: Optional JSON-compatible mapping forwarded to Core as a
                ``google.protobuf.Struct`` (nested dicts/lists are supported).
                Platform-neutral well-known keys such as
                ``"kanon.conversation_kind"`` and ``"kanon.bot_mentioned"``
                belong here, because Core (not the adapter) applies the reply
                policy to them.
            segments: Optional inbound message content in proto-JSON segment
                shape, e.g. ``{"text": {"content": "hi"}}``,
                ``{"image": {"url": "https://...", "mime_type": "image/png"}}``
                or ``{"mention": {"target_user_id": "abc"}}``. This is the rule
                for rich media: every part of the message the model may need is
                expressed as a typed segment, never as flattened strings or
                metadata, so Core renders one canonical model-visible message.
                Each entry must set exactly one ``MessageSegment`` variant;
                omitting the argument keeps the previous text-only behaviour.

        Returns:
            The raw ``pb.IngestEventResponse``. ``accepted`` reflects Core's
            Fast-ACK decision: ``False`` means the event was *not* enqueued
            because the ingest queue was full, so callers must not assume
            success. ``event_id`` echoes the id used for this event.

        Raises:
            ValueError: If a ``segments`` entry sets no ``MessageSegment``
                variant. Such an entry carries no content the model could
                render, so failing here names the offending index instead of
                shipping an empty segment Core would have to ignore.
            grpc.aio.AioRpcError: If the shared channel could not reach Core, or
                Core answered with an error status. Deliberately not swallowed:
                "Core unreachable" and "Core rejected the event" are different
                outcomes and the adapter decides how to react.
        """
        # Always ship a non-empty event id: Core logs and returns it verbatim, and
        # an empty id would make deduplication and tracing impossible.
        resolved_event_id = event_id or uuid.uuid4().hex

        # Convert the caller's mapping into a Struct up front, using the same
        # JSON mapping helper as Plugin.meta()/on_call_tool so payload conversion
        # follows one rule across the SDK. ParseDict fails loudly on values that
        # JSON cannot express, which is preferable to silently dropping fields.
        event_metadata: Optional[Struct] = None
        if metadata is not None:
            event_metadata = Struct()
            ParseDict(metadata, event_metadata)

        # Segments travel in proto-JSON shape so adapters can forward platform
        # JSON-derived dicts directly. ParseDict is the same mapping helper used
        # for metadata, and it fails loudly on a malformed payload rather than
        # silently dropping a part of the user's message.
        event_segments: List[pb.MessageSegment] = []
        if segments is not None:
            for index, segment in enumerate(segments):
                parsed_segment = pb.MessageSegment()
                ParseDict(segment, parsed_segment)
                # An empty dict parses without error but leaves the `segment`
                # oneof unset; such an entry has no content the model could
                # render, so reject it with the index instead of forwarding a
                # valueless segment Core would have to ignore.
                if parsed_segment.WhichOneof("segment") is None:
                    raise ValueError(
                        f"segments[{index}] does not set any MessageSegment variant"
                    )
                event_segments.append(parsed_segment)

        # Both the outer request and the nested event carry the platform: Core's
        # pipeline worker prefers event.platform and only falls back to the outer
        # field, so setting both keeps routing unambiguous.
        request = pb.IngestEventRequest(
            platform=platform,
            event=pb.PipelineEventRequest(
                event_id=resolved_event_id,
                platform=platform,
                channel_id=channel_id,
                sender_id=sender_id,
                raw_text=text,
                metadata=event_metadata,
                segments=event_segments,
            ),
        )

        # Single-flight per handle: see the class docstring for why adapter tasks
        # share one channel and one lock instead of dialing concurrently. The
        # await is short by design - Core Fast-ACKs without waiting for the LLM.
        async with self._lock:
            return await self._stub.IngestEvent(request)


class PluginContext:
    """Runtime context provided to a plugin during initialization and execution.

    Attributes:
        data_dir: Writable per-plugin data directory (``./data/plugins/<id>/``).
        config: Static plugin configuration mapping (empty when absent).
        core: Optional :class:`CoreHandle` for pushing inbound events into Core.
            Built by the host from the same channel used for ``RegisterHost``.
            It is ``None`` in standalone mode - when ``KANON_CORE_SOCK`` is
            unset, its socket is missing, or registration failed - and plugins
            must treat that as "no Core available" rather than dialing their own.
    """

    def __init__(
        self,
        data_dir: Path,
        config: Optional[Dict[str, Any]] = None,
        core: Optional[CoreHandle] = None,
    ):
        """Builds a context; ``config`` and ``core`` are optional keywords."""
        self.data_dir = data_dir
        self.config = config or {}
        # Never a fabricated stub: either the host proved Core reachable, or None.
        self.core = core


#: Anything that can be turned into reply segments: text, one segment, or a list of either.
Replyable = Union[str, pb.MessageSegment, Sequence[Union[str, pb.MessageSegment]]]


def to_segments(value: Optional[Replyable]) -> List[pb.MessageSegment]:
    """Normalizes text, a segment, or a list of either into a list of segments.

    Plain strings become text segments, so handlers can answer with whatever is most natural.
    """
    if value is None:
        return []
    if isinstance(value, (str, pb.MessageSegment)):
        value = [value]
    segments: List[pb.MessageSegment] = []
    for item in value:
        if isinstance(item, str):
            segments.append(MessageSegment.text(item))
        elif isinstance(item, pb.MessageSegment):
            segments.append(item)
        else:
            raise TypeError(f"cannot send {type(item).__name__} as a message segment")
    return segments


def _image_parts(images: Sequence[pb.MessageSegment]) -> List[pb.ImageSegment]:
    """The image payloads of image segments; anything else is a caller mistake."""
    parts = []
    for segment in images:
        if segment.WhichOneof("segment") != "image":
            raise ValueError("images must be image segments (see MessageSegment.image_*)")
        parts.append(segment.image)
    return parts


def llm_message(text: str, role: str = "user", images: Sequence[pb.MessageSegment] = ()) -> pb.LLMMessage:
    """Builds one turn for :meth:`CoreHandle.request_llm`.

    Args:
        text: The turn's text.
        role: ``"user"`` or ``"assistant"``.
        images: Image segments (see :meth:`MessageSegment.image_url`); user turns only.
    """
    if role not in _ROLE_VALUES:
        raise ValueError(f"unknown LLM role {role!r}; expected 'user' or 'assistant'")
    return pb.LLMMessage(role=_ROLE_VALUES[role], text=text, images=_image_parts(images))


class MessageSegment:
    """Factory for typed :class:`pb.MessageSegment` values.

    Media sources come in three forms: a URL the platform fetches, a local file path (read by the
    adapter on the Kanon host), or raw bytes.
    """

    @staticmethod
    def text(content: str) -> pb.MessageSegment:
        """A plain text segment."""
        return pb.MessageSegment(text=pb.TextSegment(content=content))

    @staticmethod
    def image_url(
        url: str,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """An image the platform downloads from ``url``."""
        return pb.MessageSegment(
            image=pb.ImageSegment(url=url, mime_type=mime_type, filename=filename)
        )

    @staticmethod
    def image_file(
        file_path: str,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """An image read from a local file."""
        return pb.MessageSegment(
            image=pb.ImageSegment(file_path=file_path, mime_type=mime_type, filename=filename)
        )

    @staticmethod
    def image_bytes(
        raw_bytes: bytes,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """An image from raw bytes."""
        return pb.MessageSegment(
            image=pb.ImageSegment(raw_bytes=raw_bytes, mime_type=mime_type, filename=filename)
        )

    @staticmethod
    def audio_url(url: str) -> pb.MessageSegment:
        """A voice message the platform downloads from ``url``."""
        return pb.MessageSegment(audio=pb.AudioSegment(url=url))

    @staticmethod
    def audio_file(file_path: str) -> pb.MessageSegment:
        """A voice message read from a local file."""
        return pb.MessageSegment(audio=pb.AudioSegment(file_path=file_path))

    @staticmethod
    def audio_bytes(raw_bytes: bytes) -> pb.MessageSegment:
        """A voice message from raw bytes."""
        return pb.MessageSegment(audio=pb.AudioSegment(raw_bytes=raw_bytes))

    @staticmethod
    def video_url(url: str, mime_type: Optional[str] = None) -> pb.MessageSegment:
        """A video the platform downloads from ``url``."""
        return pb.MessageSegment(video=pb.VideoSegment(url=url, mime_type=mime_type))

    @staticmethod
    def video_file(file_path: str, mime_type: Optional[str] = None) -> pb.MessageSegment:
        """A video read from a local file."""
        return pb.MessageSegment(video=pb.VideoSegment(file_path=file_path, mime_type=mime_type))

    @staticmethod
    def file(
        name: str,
        *,
        url: Optional[str] = None,
        file_path: Optional[str] = None,
        raw_bytes: Optional[bytes] = None,
    ) -> pb.MessageSegment:
        """A document named ``name``, from exactly one of ``url``, ``file_path``, ``raw_bytes``."""
        sources = {"url": url, "file_path": file_path, "raw_bytes": raw_bytes}
        given = {key: value for key, value in sources.items() if value is not None}
        if len(given) != 1:
            raise ValueError("MessageSegment.file needs exactly one of url, file_path, raw_bytes")
        if not name:
            raise ValueError("MessageSegment.file needs a name")
        return pb.MessageSegment(file=pb.FileSegment(name=name, **given))

    @staticmethod
    def face(face_id: str) -> pb.MessageSegment:
        """A platform built-in emoji (QQ face id)."""
        return pb.MessageSegment(face=pb.FaceSegment(id=str(face_id)))

    @staticmethod
    def mention(user_id: str, display_name: str = "") -> pb.MessageSegment:
        """An @-mention of one user."""
        return pb.MessageSegment(
            mention=pb.MentionSegment(target_user_id=str(user_id), display_name=display_name)
        )

    @staticmethod
    def mention_all() -> pb.MessageSegment:
        """An @-mention of everyone."""
        return pb.MessageSegment(mention=pb.MentionSegment(is_all=True))

    @staticmethod
    def quote(event_id: str) -> pb.MessageSegment:
        """A quote of the message with ``event_id`` (sent as the platform's native reply)."""
        return pb.MessageSegment(reply=pb.ReplySegment(target_message_id=event_id))
