"""Context and message segment abstractions for Kanon Python SDK."""

import asyncio
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional

from google.protobuf.json_format import ParseDict
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


class MessageSegment:
    """Helper factory for constructing strongly typed Protobuf MessageSegment instances."""

    @staticmethod
    def text(content: str) -> pb.MessageSegment:
        """Constructs a plain text segment."""
        return pb.MessageSegment(text=pb.TextSegment(content=content))

    @staticmethod
    def image_url(
        url: str,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """Constructs an image segment referencing a remote URL."""
        return pb.MessageSegment(
            image=pb.ImageSegment(
                url=url,
                mime_type=mime_type,
                filename=filename,
            )
        )

    @staticmethod
    def image_file(
        file_path: str,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """Constructs an image segment referencing a local zero-copy physical file."""
        return pb.MessageSegment(
            image=pb.ImageSegment(
                file_path=file_path,
                mime_type=mime_type,
                filename=filename,
            )
        )

    @staticmethod
    def image_bytes(
        raw_bytes: bytes,
        mime_type: Optional[str] = None,
        filename: Optional[str] = None,
    ) -> pb.MessageSegment:
        """Constructs an image segment from raw byte payload."""
        return pb.MessageSegment(
            image=pb.ImageSegment(
                raw_bytes=raw_bytes,
                mime_type=mime_type,
                filename=filename,
            )
        )
