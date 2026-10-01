"""Event objects handed to plugin handlers, and multi-turn conversations.

Handlers receive a :class:`MessageEvent` (or its :class:`CommandEvent` subclass) instead of raw
protobuf requests. The event knows the conversation it came from, so a handler can answer with
``await event.reply(...)`` and ask a follow-up question with ``await event.wait_next()``.

How ``wait_next`` works
-----------------------
Core processes messages one at a time and never blocks waiting for a plugin to "hear back" from
a user. A command handler therefore cannot simply sleep until the next message arrives inside
one ``OnExecuteCommand`` call. Instead:

1. The handler runs as its own task. The RPC that started it waits for the handler's next
   *yield point*: either it finishes, or it calls ``wait_next``.
2. ``wait_next(timeout)`` ends the current RPC, returning the replies gathered so far together
   with ``capture_seconds = timeout``. Core then routes the same sender's next message in that
   channel back to this plugin as a *continuation*.
3. The continuation RPC resumes the suspended handler with the new event and again waits for
   its next yield point.

Replies made while an RPC is waiting are returned in that RPC's response (one platform message,
in order). Replies made when no RPC is waiting — after a ``wait_next`` timed out, or from a
background task — are delivered through ``ReplyMessage`` instead.
"""

from __future__ import annotations

import asyncio
from typing import Any, Dict, List, Optional, Tuple

from google.protobuf.json_format import MessageToDict

from kanon_sdk.context import CoreHandle, Replyable, to_segments
from kanon_sdk.proto import pb

#: Longest capture Core honours, in seconds.
MAX_WAIT_SECONDS = 600

#: Extra time the SDK keeps a suspended handler alive after its capture window, so a message
#: Core routed at the last moment still finds the handler waiting.
WAIT_GRACE_SECONDS = 5.0

#: Conversation key Core captures on: one sender in one channel of one platform.
ConversationKey = Tuple[str, str, str]


class MessageEvent:
    """An inbound platform message (or notice) as a plugin sees it.

    Attributes:
        raw: The underlying ``pb.PipelineEventRequest``.
        core: The host's :class:`CoreHandle`, or ``None`` in standalone mode.
    """

    def __init__(self, raw: pb.PipelineEventRequest, core: Optional[CoreHandle] = None) -> None:
        self.raw = raw
        self.core = core
        self._metadata: Optional[Dict[str, Any]] = None

    # --- Message facts -----------------------------------------------------------------------

    @property
    def event_id(self) -> str:
        """Platform-qualified id of this message; quote it with ``MessageSegment.quote``."""
        return self.raw.event_id

    @property
    def platform(self) -> str:
        """Platform the message arrived on."""
        return self.raw.platform

    @property
    def channel_id(self) -> str:
        """Conversation the message belongs to, e.g. ``group:123`` or ``private:456``."""
        return self.raw.channel_id

    @property
    def sender_id(self) -> str:
        """Platform id of the author."""
        return self.raw.sender_id

    @property
    def text(self) -> str:
        """The message's plain text."""
        return self.raw.raw_text

    @property
    def segments(self) -> List[pb.MessageSegment]:
        """The message's typed segments (text, images, mentions, quotes, ...)."""
        return list(self.raw.segments)

    @property
    def metadata(self) -> Dict[str, Any]:
        """Adapter metadata as plain JSON data (keys such as ``kanon.sender_name``)."""
        if self._metadata is None:
            self._metadata = (
                MessageToDict(self.raw.metadata) if self.raw.HasField("metadata") else {}
            )
        return self._metadata

    @property
    def sender_name(self) -> str:
        """Display name of the author, when the platform reports one."""
        return str(self.metadata.get("kanon.sender_name", ""))

    @property
    def sender_role(self) -> str:
        """``owner``, ``admin`` or ``member`` in groups whose platform reports roles."""
        return str(self.metadata.get("kanon.sender_role", ""))

    @property
    def is_group(self) -> bool:
        """Whether the message was posted in a group conversation."""
        return self.metadata.get("kanon.conversation_kind") == "group"

    @property
    def bot_mentioned(self) -> bool:
        """Whether the message @-mentions the bot."""
        return bool(self.metadata.get("kanon.bot_mentioned", False))

    @property
    def notice(self) -> str:
        """Notice kind (``poke``, ``member_join``, ...) or ``""`` for an ordinary message."""
        return str(self.metadata.get("kanon.notice", ""))

    @property
    def images(self) -> List[pb.ImageSegment]:
        """Image segments of the message, in order."""
        return [s.image for s in self.raw.segments if s.WhichOneof("segment") == "image"]

    @property
    def conversation_key(self) -> ConversationKey:
        """The key Core uses for captures: platform, channel and sender."""
        return (self.platform, self.channel_id, self.sender_id)

    # --- Answering -----------------------------------------------------------------------------

    async def send(self, content: Replyable) -> pb.DeliverMessageResponse:
        """Sends a message to this conversation right away and waits for delivery.

        Use this for progress notes during long work ("rendering..."). Raises ``RuntimeError``
        in standalone mode, where there is no Core to deliver through.
        """
        if self.core is None:
            raise RuntimeError("no Core connection: cannot send messages in standalone mode")
        return await self.core.reply_to(self.raw, to_segments(content))

    async def reply(self, content: Replyable) -> None:
        """Answers this message. For a plain event this is the same as :meth:`send`."""
        await self.send(content)


class _Turn:
    """One RPC waiting for the handler's next yield point, and the replies it will carry."""

    def __init__(self) -> None:
        self.replies: List[pb.MessageSegment] = []
        # Resolved with the response fields once the handler yields, finishes or fails.
        self.done: asyncio.Future = asyncio.get_running_loop().create_future()

    def finish(self, capture_seconds: int = 0, success: bool = True, error: str = "") -> None:
        """Ends the turn; the waiting RPC answers with the replies gathered so far."""
        if not self.done.done():
            self.done.set_result((success, error, capture_seconds))


class _Session:
    """A running command handler and the RPC currently waiting on it, if any."""

    def __init__(self, conversations: "Conversations") -> None:
        self.conversations = conversations
        self.turn: Optional[_Turn] = None


class CommandEvent(MessageEvent):
    """A message that invoked a command or trigger, or continued a conversation.

    Attributes:
        command: Canonical command (or trigger) name.
        args: Arguments split on whitespace, quotes respected. For a trigger, its regex groups.
        raw_args: Everything after the command name, unsplit.
        continuation: True when this message answers an earlier ``wait_next``/capture.
        request: The underlying ``pb.CommandExecuteRequest``.
    """

    def __init__(
        self,
        request: pb.CommandExecuteRequest,
        core: Optional[CoreHandle] = None,
        session: Optional[_Session] = None,
    ) -> None:
        super().__init__(request.context, core)
        self.request = request
        self.command = request.command
        self.args = list(request.args)
        self.raw_args = request.raw_args
        self.continuation = request.continuation
        self._session = session

    def __getattr__(self, name: str) -> Any:
        # Handlers written against the raw request (``req.context``, ``req.plugin_id``) keep
        # working: unknown attributes fall through to the protobuf message.
        return getattr(self.request, name)

    async def reply(self, content: Replyable) -> None:
        """Answers this message.

        While Core is waiting on this handler, replies are collected and sent together as the
        command's answer; otherwise they are delivered immediately (see the module docstring).
        """
        turn = self._session.turn if self._session is not None else None
        if turn is not None:
            turn.replies.extend(to_segments(content))
        else:
            await self.send(content)

    async def wait_next(self, timeout: float = 60) -> "CommandEvent":
        """Ends this turn and waits for the same sender's next message in this conversation.

        Replies made so far are sent first. The next message skips commands and the model and
        comes back here, as a new :class:`CommandEvent` with ``continuation=True``.

        Args:
            timeout: Seconds to wait, at most :data:`MAX_WAIT_SECONDS`.

        Raises:
            asyncio.TimeoutError: If the sender did not answer in time. The handler may still
                ``reply`` afterwards; those replies are delivered on their own.
            RuntimeError: If the event did not come from a command dispatch.
        """
        if self._session is None:
            raise RuntimeError("wait_next is only available inside a command handler")
        seconds = max(1, min(int(timeout), MAX_WAIT_SECONDS))
        session = self._session
        key = self.conversation_key
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        session.conversations.suspend(key, session, future)

        # Hand the turn back to Core: its RPC returns now, asking for the capture.
        turn, session.turn = session.turn, None
        if turn is not None:
            turn.finish(seconds)
        try:
            return await asyncio.wait_for(future, seconds + WAIT_GRACE_SECONDS)
        finally:
            session.conversations.forget(key, future)


class Conversations:
    """Suspended command handlers, keyed by the conversation Core will route back."""

    def __init__(self) -> None:
        self._waiting: Dict[ConversationKey, Tuple[_Session, asyncio.Future]] = {}

    def suspend(self, key: ConversationKey, session: _Session, future: asyncio.Future) -> None:
        # Core keeps one capture per conversation, so a newer wait replaces an older one; the
        # older handler is told instead of being left hanging until its timeout.
        previous = self._waiting.get(key)
        if previous is not None and not previous[1].done():
            previous[1].set_exception(asyncio.TimeoutError("superseded by a newer wait_next"))
        self._waiting[key] = (session, future)

    def forget(self, key: ConversationKey, future: asyncio.Future) -> None:
        entry = self._waiting.get(key)
        if entry is not None and entry[1] is future:
            del self._waiting[key]

    def take(self, key: ConversationKey) -> Optional[Tuple[_Session, asyncio.Future]]:
        entry = self._waiting.pop(key, None)
        if entry is None or entry[1].done():
            return None
        return entry


async def run_turn(session: _Session) -> pb.CommandExecuteResponse:
    """Waits for the session's current turn and turns it into a command response."""
    turn = session.turn
    assert turn is not None, "run_turn needs an open turn"
    success, error, capture_seconds = await turn.done
    return pb.CommandExecuteResponse(
        success=success,
        replies=turn.replies,
        error_message=error,
        capture_seconds=capture_seconds,
    )
