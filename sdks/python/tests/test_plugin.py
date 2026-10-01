"""Tests for the plugin decorators, the event-style dispatch and multi-turn conversations."""

import asyncio
import sys
import unittest
from pathlib import Path
from typing import List

_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

from google.protobuf.struct_pb2 import Value

from kanon_sdk import (
    CommandEvent,
    MessageSegment,
    Plugin,
    PluginContext,
    Reply,
    command,
    decorate_reply,
    on_event,
    trigger,
)
from kanon_sdk.context import CoreHandle
from kanon_sdk.proto import pb


def request(command_name: str, text: str = "", args=(), continuation: bool = False):
    """A command request as Core sends it, from sender u1 in group g1."""
    return pb.CommandExecuteRequest(
        plugin_id="test.plugin",
        command=command_name,
        args=list(args),
        raw_args=text,
        continuation=continuation,
        context=pb.PipelineEventRequest(
            event_id=f"onebot:{text}",
            platform="onebot",
            channel_id="group:g1",
            sender_id="u1",
            raw_text=text,
        ),
    )


def texts(response: pb.CommandExecuteResponse) -> List[str]:
    return [segment.text.content for segment in response.replies]


class Demo(Plugin):
    id = "test.plugin"

    def __init__(self) -> None:
        self.events: List[str] = []
        super().__init__()

    @command("echo", aliases=("e",), access="admins_in_groups")
    async def echo(self, event: CommandEvent, args: List[str]) -> str:
        return " ".join(args)

    @command("ask")
    async def ask(self, event: CommandEvent) -> None:
        await event.reply("name?")
        try:
            answer = await event.wait_next(timeout=30)
        except asyncio.TimeoutError:
            return
        await answer.reply(f"hello {answer.text}")

    @command("boom")
    def boom(self) -> None:
        raise RuntimeError("bad input")

    @trigger(r"^ping$")
    async def ping(self, event: CommandEvent) -> list:
        return [MessageSegment.quote(event.event_id), "pong"]

    @on_event("llm_response")
    async def seen(self, event: pb.LlmResponseEvent) -> None:
        self.events.append(event.content)

    @decorate_reply
    async def sign(self, reply: Reply):
        if reply.source != "llm":
            return None
        return list(reply.segments) + [MessageSegment.text(" — bot")]


class TestMeta(unittest.TestCase):
    def test_meta_lists_commands_triggers_and_hooks(self) -> None:
        meta = Demo().meta()
        echo = next(c for c in meta.commands if c.name == "echo")
        self.assertEqual(list(echo.aliases), ["e"])
        self.assertEqual(echo.access, pb.COMMAND_ACCESS_ADMINS_IN_GROUPS)
        self.assertEqual([(t.name, t.pattern) for t in meta.triggers], [("ping", "^ping$")])
        self.assertEqual(list(meta.events), [pb.EVENT_KIND_LLM_RESPONSE])
        self.assertTrue(meta.decorates_replies)

    def test_command_and_trigger_names_must_not_collide(self) -> None:
        class Clash(Plugin):
            @command("x")
            async def a(self) -> None: ...

            @trigger("^x$", name="x")
            async def b(self) -> None: ...

        with self.assertRaises(ValueError):
            Clash()

    def test_unknown_access_level_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            command("x", access="root")


class TestDispatch(unittest.IsolatedAsyncioTestCase):
    async def test_return_value_becomes_the_reply(self) -> None:
        response = await Demo().on_execute_command(request("echo", "a b", ["a", "b"]))
        self.assertTrue(response.success)
        self.assertEqual(texts(response), ["a b"])
        self.assertEqual(response.capture_seconds, 0)

    async def test_trigger_returns_mixed_segments(self) -> None:
        response = await Demo().on_execute_command(request("ping", "ping"))
        self.assertEqual(response.replies[0].reply.target_message_id, "onebot:ping")
        self.assertEqual(response.replies[1].text.content, "pong")

    async def test_handler_exception_is_reported(self) -> None:
        response = await Demo().on_execute_command(request("boom"))
        self.assertFalse(response.success)
        self.assertIn("bad input", response.error_message)

    async def test_unknown_command_fails(self) -> None:
        response = await Demo().on_execute_command(request("nope"))
        self.assertFalse(response.success)

    async def test_wait_next_spans_two_rpcs(self) -> None:
        plugin = Demo()
        first = await plugin.on_execute_command(request("ask", ""))
        # The first turn ends at wait_next: its reply goes out and Core is asked to capture.
        self.assertEqual(texts(first), ["name?"])
        self.assertEqual(first.capture_seconds, 30)

        second = await plugin.on_execute_command(request("ask", "Ann", continuation=True))
        self.assertTrue(second.success)
        self.assertEqual(texts(second), ["hello Ann"])
        self.assertEqual(second.capture_seconds, 0)

    async def test_continuation_without_waiter_calls_the_handler(self) -> None:
        # E.g. the host restarted while Core still held the capture: the handler runs afresh.
        response = await Demo().on_execute_command(
            request("echo", "late", ["late"], continuation=True)
        )
        self.assertEqual(texts(response), ["late"])

    async def test_newer_wait_supersedes_older_one(self) -> None:
        plugin = Demo()
        await plugin.on_execute_command(request("ask"))
        await plugin.on_execute_command(request("ask"))
        # Only one handler is waiting for the conversation; the older one was told to stop.
        response = await plugin.on_execute_command(request("ask", "Bo", continuation=True))
        self.assertEqual(texts(response), ["hello Bo"])
        await asyncio.sleep(0)
        # Both handlers have finished: none is left suspended until its timeout.
        self.assertFalse(plugin._tasks)


class TestHooks(unittest.IsolatedAsyncioTestCase):
    async def test_on_event_dispatches_by_kind(self) -> None:
        plugin = Demo()
        await plugin.on_event(
            pb.EventNotification(llm_response=pb.LlmResponseEvent(content="answer"))
        )
        await plugin.on_event(pb.EventNotification(message_sent=pb.MessageSentEvent()))
        self.assertEqual(plugin.events, ["answer"])

    async def test_decorator_rewrites_only_llm_replies(self) -> None:
        plugin = Demo()
        llm = await plugin.on_decorate_reply(
            pb.DecorateReplyRequest(
                segments=[MessageSegment.text("hi")], source=pb.REPLY_SOURCE_LLM
            )
        )
        self.assertTrue(llm.modified)
        self.assertEqual([s.text.content for s in llm.segments], ["hi", " — bot"])

        cmd = await plugin.on_decorate_reply(
            pb.DecorateReplyRequest(source=pb.REPLY_SOURCE_COMMAND, command="echo")
        )
        self.assertFalse(cmd.modified)


class ApiStub:
    """Fake ``BotApiService`` stub recording CallPlatformApi requests."""

    def __init__(self) -> None:
        self.requests: List[pb.PlatformApiRequest] = []

    async def CallPlatformApi(self, request: pb.PlatformApiRequest) -> pb.PlatformApiResponse:  # noqa: N802
        self.requests.append(request)
        result = Value()
        result.list_value.values.add().string_value = "member"
        return pb.PlatformApiResponse(result=result)


class TestCoreHandle(unittest.IsolatedAsyncioTestCase):
    async def test_call_platform_api_round_trips_json(self) -> None:
        stub = ApiStub()
        handle = CoreHandle(stub)  # type: ignore[arg-type]
        result = await handle.call_platform_api("onebot", "get_group_member_list", group_id=1)
        self.assertEqual(result, ["member"])
        sent = stub.requests[0]
        self.assertEqual((sent.platform, sent.action), ("onebot", "get_group_member_list"))
        self.assertEqual(sent.params["group_id"], 1)

    async def test_plugin_context_reaches_events(self) -> None:
        plugin = Demo()
        handle = CoreHandle(ApiStub())  # type: ignore[arg-type]
        plugin.context = PluginContext(data_dir=Path("."), config={}, core=handle)
        self.assertIs(plugin.core, handle)


if __name__ == "__main__":
    unittest.main()
