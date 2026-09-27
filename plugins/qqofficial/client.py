"""WebSocket Gateway client for Kanon QQ Official adapter.

Encapsulates botpy.Client event callbacks and routes inbound messages and notices
to the Kanon Core pipeline with Fast-ACK guarantees.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Dict, List, Optional

import botpy
try:
    from botpy.message import C2CMessage, DirectMessage, GroupMessage, Message
except ImportError:
    from botpy.types.message import C2CMessage, DirectMessage, GroupMessage, Message

if TYPE_CHECKING:
    from main import QQOfficialAdapter


def _attachment_segment(attachment: Any) -> Optional[Dict[str, Any]]:
    """Maps one botpy attachment to a proto-JSON ``MessageSegment`` dict.

    Returns ``None`` for an attachment without a URL: every segment variant that
    can carry it is source-based, so an URL-less entry would be valueless for the
    model. Skipping one unreadable attachment keeps the rest of the message
    (text and mentions) intact instead of failing the whole ingest.

    ``getattr`` is used throughout because botpy's attachment classes differ
    between versions and some payloads omit ``content_type``/``filename``.
    """
    url = getattr(attachment, "url", None) or ""
    if not url:
        return None

    content_type = getattr(attachment, "content_type", None) or ""
    filename = getattr(attachment, "filename", None) or ""
    kind = content_type.lower()

    if kind.startswith("image"):
        # QQ reports the media type of an image attachment, so carry it through
        # as the segment MIME type instead of making Core guess from the URL.
        image: Dict[str, Any] = {"url": url}
        if content_type:
            image["mime_type"] = content_type
        if filename:
            image["filename"] = filename
        return {"image": image}

    if kind.startswith("audio") or kind.startswith("voice"):
        return {"audio": {"url": url}}

    if kind.startswith("video"):
        # The proto has no video variant: a video is a platform-specific payload
        # the model cannot consume as a first-class segment yet, so preserve it
        # verbatim as a named custom segment.
        return {
            "custom": {
                "type_name": "qqofficial.video",
                "payload": {"url": url, "content_type": content_type},
            }
        }

    payload: Dict[str, Any] = {"url": url, "content_type": content_type}
    if filename:
        payload["filename"] = filename
    return {"custom": {"type_name": "qqofficial.file", "payload": payload}}


def _mention_segment(member: Any) -> Optional[Dict[str, Any]]:
    """Maps one botpy mentioned member to a proto-JSON ``mention`` segment.

    Group mentions expose ``member_openid`` while guild mentions expose ``id``;
    accept either so one helper serves every callback. A mention without any id
    cannot be resolved by the model and is skipped.
    """
    target_user_id = getattr(member, "member_openid", None) or getattr(member, "id", None) or ""
    if not target_user_id:
        return None
    # Display names are optional in botpy and field names vary by version.
    display_name = (
        getattr(member, "username", None)
        or getattr(member, "nickname", None)
        or getattr(member, "nick", None)
        or getattr(member, "name", None)
        or ""
    )
    return {
        "mention": {
            "target_user_id": target_user_id,
            "display_name": display_name,
        }
    }


def _build_inbound_segments(message: Any, content: str) -> List[Dict[str, Any]]:
    """Builds the proto-JSON segment list describing one inbound botpy message.

    Order matters for the model-visible rendering: text first, then mentions,
    then attachments, mirroring how the parts appear in the original message.
    ``content`` is the already-normalized text the callback forwards as
    ``text``, so the text segment and the raw text stay identical.
    """
    segments: List[Dict[str, Any]] = []
    if content:
        segments.append({"text": {"content": content}})

    for member in getattr(message, "mentions", None) or []:
        mention = _mention_segment(member)
        if mention is not None:
            segments.append(mention)

    for attachment in getattr(message, "attachments", None) or []:
        segment = _attachment_segment(attachment)
        if segment is not None:
            segments.append(segment)

    return segments


def _timestamp_extra(message: Any) -> Dict[str, Any]:
    """Platform-neutral timestamp the core adds to context when the operator enables it.

    botpy exposes an ISO-8601 string on every message type; passing it through as text keeps date
    formatting out of the core, which does not ship a date library. Only a real scalar is
    forwarded, so an unexpected object attribute is ignored rather than stringified into the
    prompt.
    """
    timestamp = getattr(message, "timestamp", None)
    if isinstance(timestamp, (str, int, float)) and str(timestamp).strip():
        return {"kanon.timestamp_text": str(timestamp)}
    return {}

class KanonBotClient(botpy.Client):
    """QQ Official bot client bridging WebSocket gateway events to Kanon Core."""

    def __init__(self, adapter: Any, *args: Any, **kwargs: Any) -> None:
        if "intents" not in kwargs and not args:
            kwargs["intents"] = botpy.Intents.none()
        super().__init__(*args, **kwargs)
        self.adapter = adapter

    async def on_ready(self) -> None:
        """Invoked when WebSocket gateway connection and handshake succeed."""
        bot_name = getattr(getattr(self, "robot", None), "name", "QQ Bot")
        print(
            f"[QQOfficial] QQ Bot '{bot_name}' connected to gateway successfully! "
            "Online and listening for events.",
            flush=True,
        )

    async def on_group_at_message_create(self, message: GroupMessage) -> None:
        """Handles group @ mentions."""
        content = (message.content or "").strip()
        print(
            f"[QQOfficial] Received Group @ Message from member={message.author.member_openid} "
            f"in group={message.group_openid}: '{content}' (id={message.id})",
            flush=True,
        )
        mentions: List[str] = [
            getattr(m, "member_openid", "")
            for m in getattr(message, "mentions", [])
        ]
        await self.adapter.ingest_qq_message(
            channel_id=f"group:{message.group_openid}",
            sender_id=message.author.member_openid,
            content=content,
            msg_id=message.id,
            scene="group",
            extra={
                **_timestamp_extra(message),
                "mentions": mentions,
                # Core owns the reply policy and reads these two platform-neutral
                # keys: a group @-callback is inherently addressed to the bot.
                "kanon.conversation_kind": "group",
                "kanon.bot_mentioned": True,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_group_message_create(self, message: GroupMessage) -> None:
        """Handles unmentioned group messages for authorized private domain bots."""
        content = (message.content or "").strip()
        print(
            f"[QQOfficial] Received Group Message from member={message.author.member_openid} "
            f"in group={message.group_openid}: '{content}' (id={message.id})",
            flush=True,
        )
        await self.adapter.ingest_qq_message(
            channel_id=f"group:{message.group_openid}",
            sender_id=message.author.member_openid,
            content=content,
            msg_id=message.id,
            scene="group",
            extra={
                **_timestamp_extra(message),
                "unmentioned": True,
                # Unmentioned means the bot was not addressed; Core's "mention"
                # policy must therefore be free to drop this event.
                "kanon.conversation_kind": "group",
                "kanon.bot_mentioned": False,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_c2c_message_create(self, message: C2CMessage) -> None:
        """Handles direct private messages (C2C)."""
        content = (message.content or "").strip()
        print(
            f"[QQOfficial] Received C2C Private Message from user={message.author.user_openid}: "
            f"'{content}' (id={message.id})",
            flush=True,
        )
        await self.adapter.ingest_qq_message(
            channel_id=f"c2c:{message.author.user_openid}",
            sender_id=message.author.user_openid,
            content=content,
            msg_id=message.id,
            scene="c2c",
            extra={
                **_timestamp_extra(message),
                # A C2C conversation is one-to-one, so Core always answers it.
                "kanon.conversation_kind": "private",
                "kanon.bot_mentioned": False,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_at_message_create(self, message: Message) -> None:
        """Handles guild channel @ mentions."""
        content = (message.content or "").strip()
        print(
            f"[QQOfficial] Received Guild @ Message from author={message.author.id} "
            f"in channel={message.channel_id}: '{content}' (id={message.id})",
            flush=True,
        )
        await self.adapter.ingest_qq_message(
            channel_id=f"guild:{message.channel_id}",
            sender_id=message.author.id,
            content=content,
            msg_id=message.id,
            scene="guild",
            extra={
                **_timestamp_extra(message),
                "guild_id": getattr(message, "guild_id", ""),
                # A guild channel is a broadcast-style conversation; the policy
                # applies, and this @-callback means the bot was addressed.
                "kanon.conversation_kind": "channel",
                "kanon.bot_mentioned": True,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_message_create(self, message: Message) -> None:
        """Handles unmentioned guild channel messages for authorized bots."""
        content = (message.content or "").strip()
        print(
            f"[QQOfficial] Received Guild Message from author={message.author.id} "
            f"in channel={message.channel_id}: '{content}' (id={message.id})",
            flush=True,
        )
        await self.adapter.ingest_qq_message(
            channel_id=f"guild:{message.channel_id}",
            sender_id=message.author.id,
            content=content,
            msg_id=message.id,
            scene="guild",
            extra={
                **_timestamp_extra(message),
                "guild_id": getattr(message, "guild_id", ""),
                "unmentioned": True,
                # Unmentioned in a guild channel: Core may legitimately stay quiet.
                "kanon.conversation_kind": "channel",
                "kanon.bot_mentioned": False,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_direct_message_create(self, message: DirectMessage) -> None:
        """Handles direct messages within guild."""
        content = (message.content or "").strip()
        channel_id = getattr(message, "channel_id", "") or getattr(message, "guild_id", "")
        author_id = getattr(message.author, "id", "")
        print(
            f"[QQOfficial] Received Guild DM from author={author_id} "
            f"in channel={channel_id}: '{content}' (id={message.id})",
            flush=True,
        )
        await self.adapter.ingest_qq_message(
            channel_id=f"guild_dm:{channel_id}",
            sender_id=author_id,
            content=content,
            msg_id=message.id,
            scene="guild_dm",
            extra={
                **_timestamp_extra(message),
                # A guild direct message is a one-to-one conversation, so Core
                # always answers it regardless of the group policy.
                "kanon.conversation_kind": "private",
                "kanon.bot_mentioned": False,
            },
            segments=_build_inbound_segments(message, content),
        )

    async def on_group_add_robot(self, event: Any) -> None:
        """Handles bot added to QQ group notice."""
        group_openid = getattr(event, "group_openid", "")
        op_openid = getattr(event, "op_member_openid", "")
        await self.adapter.ingest_notice(
            channel_id=f"group:{group_openid}",
            event_type="group_add_robot",
            data={"group_openid": group_openid, "op_member_openid": op_openid},
        )

    async def on_group_del_robot(self, event: Any) -> None:
        """Handles bot removed from QQ group notice."""
        group_openid = getattr(event, "group_openid", "")
        await self.adapter.ingest_notice(
            channel_id=f"group:{group_openid}",
            event_type="group_del_robot",
            data={"group_openid": group_openid},
        )

    async def on_friend_add(self, event: Any) -> None:
        """Handles friend added notice."""
        openid = getattr(event, "openid", "")
        await self.adapter.ingest_notice(
            channel_id=f"c2c:{openid}",
            event_type="friend_add",
            data={"openid": openid},
        )

    async def on_friend_del(self, event: Any) -> None:
        """Handles friend removed notice."""
        openid = getattr(event, "openid", "")
        await self.adapter.ingest_notice(
            channel_id=f"c2c:{openid}",
            event_type="friend_del",
            data={"openid": openid},
        )
