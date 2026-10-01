"""Context and message segment abstractions for Kanon Python SDK."""

import asyncio
import uuid
from pathlib import Path
from typing import Any, AsyncIterator, Dict, List, Optional, Sequence, Union

from google.protobuf.json_format import MessageToDict, ParseDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk.proto import pb, pb_grpc


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

    def __init__(self, stub: pb_grpc.BotApiServiceStub) -> None:
        """Wraps an existing ``BotApiService`` stub without touching its channel."""
        # The stub (and therefore the channel) belongs to the host process: the
        # handle only issues RPCs, so channel creation/closure has a single owner.
        self._stub = stub
        # Serializes concurrent adapter tasks sharing this handle. The lock is
        # created lazily against the running loop, so constructing a CoreHandle
        # outside a loop (e.g. at import time) stays safe.
        self._lock = asyncio.Lock()

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


def llm_message(text: str, role: str = "user", images: Sequence[pb.MessageSegment] = ()) -> pb.LLMMessage:
    """Builds one turn for :meth:`CoreHandle.request_llm`.

    Args:
        text: The turn's text.
        role: ``"user"`` or ``"assistant"``.
        images: Image segments (see :meth:`MessageSegment.image_url`); user turns only.
    """
    roles = {"user": pb.LLM_ROLE_USER, "assistant": pb.LLM_ROLE_ASSISTANT}
    if role not in roles:
        raise ValueError(f"unknown LLM role {role!r}; expected 'user' or 'assistant'")
    image_parts = []
    for segment in images:
        if segment.WhichOneof("segment") != "image":
            raise ValueError("llm_message images must be image segments")
        image_parts.append(segment.image)
    return pb.LLMMessage(role=roles[role], text=text, images=image_parts)


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
