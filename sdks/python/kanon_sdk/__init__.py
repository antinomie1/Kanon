"""Official Kanon Python SDK.

Provides base classes, decorators, and context abstractions for authoring
out-of-process Kanon plugins in Python.
"""

from kanon_sdk.context import (
    ConversationHistory,
    CoreHandle,
    MessageSegment,
    PluginContext,
    Replyable,
    llm_message,
    to_segments,
)
from kanon_sdk.event import CommandEvent, MessageEvent
from kanon_sdk.host import KanonHost
from kanon_sdk.ipc import connect_core_channel
from kanon_sdk.plugin import (
    Plugin,
    Reply,
    action,
    command,
    decorate_reply,
    on_event,
    prepare_turn,
    tool,
    trigger,
)
from kanon_sdk.proto import pb, pb_grpc

__all__ = [
    "CommandEvent",
    "ConversationHistory",
    "CoreHandle",
    "KanonHost",
    "MessageEvent",
    "MessageSegment",
    "PluginContext",
    "Plugin",
    "Reply",
    "Replyable",
    "action",
    "command",
    "connect_core_channel",
    "decorate_reply",
    "llm_message",
    "on_event",
    "prepare_turn",
    "to_segments",
    "tool",
    "trigger",
    "pb",
    "pb_grpc",
]
