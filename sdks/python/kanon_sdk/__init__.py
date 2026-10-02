"""Official Kanon Python SDK.

Provides base classes, decorators, and context abstractions for authoring
out-of-process Kanon plugins in Python.
"""

from kanon_sdk.context import (
    AgentResult,
    ConversationHistory,
    ConversationInfo,
    CoreHandle,
    MessageSegment,
    Persona,
    PluginContext,
    Replyable,
    llm_message,
    to_segments,
)
from kanon_sdk.event import CommandEvent, MessageEvent
from kanon_sdk.host import KanonHost
from kanon_sdk.ipc import connect_core_channel
from kanon_sdk.kv import KV
from kanon_sdk.plugin import (
    Plugin,
    Reply,
    action,
    command,
    decorate_reply,
    on_event,
    on_llm_request,
    prepare_turn,
    tool,
    trigger,
)
from kanon_sdk.proto import pb, pb_grpc
from kanon_sdk.web import HttpRequest, HttpResponse, http_route

__all__ = [
    "AgentResult",
    "CommandEvent",
    "ConversationHistory",
    "ConversationInfo",
    "CoreHandle",
    "HttpRequest",
    "HttpResponse",
    "KV",
    "KanonHost",
    "MessageEvent",
    "MessageSegment",
    "Persona",
    "PluginContext",
    "Plugin",
    "Reply",
    "Replyable",
    "action",
    "command",
    "connect_core_channel",
    "decorate_reply",
    "http_route",
    "llm_message",
    "on_event",
    "on_llm_request",
    "prepare_turn",
    "to_segments",
    "tool",
    "trigger",
    "pb",
    "pb_grpc",
]
