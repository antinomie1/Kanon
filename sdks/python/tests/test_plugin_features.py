"""Tests for command groups, inferred and runtime tools, prompt rewriting, HTTP routes and the
core calls that back them (KV, agent runs, conversations, rendering)."""

import json
import sys
import unittest
from pathlib import Path
from typing import Any, List, Optional

_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

import grpc
from google.protobuf.json_format import MessageToDict, ParseDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk import (
    CommandEvent,
    CoreHandle,
    HttpRequest,
    HttpResponse,
    MessageEvent,
    MessageSegment,
    Plugin,
    PluginContext,
    command,
    http_route,
    on_llm_request,
    tool,
)
from kanon_sdk.proto import pb


def request(command_name: str, text: str = "") -> pb.CommandExecuteRequest:
    """A command request as Core sends it: arguments split on whitespace."""
    return pb.CommandExecuteRequest(
        plugin_id="test.features",
        command=command_name,
        args=text.split(),
        raw_args=text,
        context=pb.PipelineEventRequest(
            event_id="onebot:1", platform="onebot", channel_id="group:g1", sender_id="u1"
        ),
    )


def texts(response: pb.CommandExecuteResponse) -> List[str]:
    return [segment.text.content for segment in response.replies]


def tool_call(name: str, args: dict, with_context: bool = True) -> pb.ToolCallRequest:
    structured = Struct()
    ParseDict(args, structured)
    call = pb.ToolCallRequest(call_id="c1", tool_name=name, structured_args=structured)
    if with_context:
        call.context.CopyFrom(request("x").context)
    return call


class CoreStub:
    """Fake ``BotApiService``: an in-memory KV, canned answers, and a log of what was asked."""

    def __init__(self) -> None:
        self.kv: dict = {}
        self.calls: List[Any] = []
        self.refresh_error: Optional[Exception] = None

    async def SetStorage(self, req):  # noqa: N802
        self.calls.append(req)
        self.kv[req.key] = req.value
        return pb.SetStorageResponse(success=True)

    async def GetStorage(self, req):  # noqa: N802
        value = self.kv.get(req.key)
        return pb.GetStorageResponse(found=value is not None, value=value or b"")

    async def DeleteStorage(self, req):  # noqa: N802
        return pb.DeleteStorageResponse(deleted=self.kv.pop(req.key, None) is not None)

    async def ListStorage(self, req):  # noqa: N802
        return pb.ListStorageResponse(keys=sorted(k for k in self.kv if k.startswith(req.prefix)))

    async def RefreshPluginMeta(self, req):  # noqa: N802
        self.calls.append(req)
        if self.refresh_error is not None:
            raise self.refresh_error
        return pb.RefreshPluginMetaResponse(plugin_ids=["test.features"])

    async def RunAgent(self, req):  # noqa: N802
        self.calls.append(req)
        return pb.RunAgentResponse(content="done", tools=["search"], session_id="s9")

    async def SwitchConversation(self, req):  # noqa: N802
        self.calls.append(req)
        return pb.ConversationList(
            conversations=[
                pb.ConversationInfo(session_id="a", title="first", message_count=2),
                pb.ConversationInfo(session_id=req.session_id, current=True),
            ]
        )

    async def AppendConversation(self, req):  # noqa: N802
        self.calls.append(req)
        return pb.AppendConversationResponse(session_id="s1")

    async def RenderImage(self, req):  # noqa: N802
        self.calls.append(req)
        return pb.RenderImageResponse(file_path="/render/card.png", width=720, height=100)


class Features(Plugin):
    id = "test.features"

    def __init__(self) -> None:
        super().__init__()
        self.items: List[str] = []

    @command("todo add", description="Add a todo", usage="/todo add <text>")
    async def add(self, event: CommandEvent) -> str:
        self.items.append(event.raw_args)
        return f"added {event.args}"

    @command("todo list", description="List todos")
    async def list_items(self) -> str:
        return ", ".join(self.items) or "empty"

    @command("notes list")
    async def notes(self) -> str:
        return "no notes"

    @command("notes", description="Personal notes")
    async def notes_root(self, event: CommandEvent) -> str:
        return f"notes root {event.args}"

    @tool
    async def greet(self, name: str, event: Optional[MessageEvent], times: int = 1) -> str:
        """Greets someone."""
        sender = event.sender_id if event is not None else "nobody"
        return f"{'hi ' * times}{name} from {sender}".strip()

    @tool("legacy", "Explicit schema", {"type": "object", "properties": {"x": {"type": "number"}}})
    async def legacy(self, args: dict) -> list:
        return [args["x"], args["x"]]

    @on_llm_request
    async def rules(self, event: MessageEvent, system_prompt: str, session_id: str) -> Optional[str]:
        if session_id == "keep":
            return None
        return f"{system_prompt}\nRules for {event.channel_id}."

    @http_route("/stats")
    async def stats(self, request: HttpRequest) -> dict:
        return {"page": request.arg("page"), "items": len(self.items)}

    @http_route("/echo", methods=("POST",))
    async def echo(self, request: HttpRequest) -> HttpResponse:
        return HttpResponse.json(request.json(), status=201)

    @http_route("/broken")
    async def broken(self, request: HttpRequest) -> None:
        raise RuntimeError("secret detail")


def connected(plugin: Plugin) -> CoreStub:
    stub = CoreStub()
    core = CoreHandle(stub, plugin_id=plugin.id, host_id="host_features")  # type: ignore[arg-type]
    plugin.context = PluginContext(data_dir=Path("."), core=core)
    return stub


class TestCommandGroups(unittest.IsolatedAsyncioTestCase):
    def test_meta_lists_groups_with_their_subcommands_in_declaration_order(self) -> None:
        commands = {c.name: c for c in Features().meta().commands}
        self.assertEqual(
            [(s.name, s.usage) for s in commands["todo"].subcommands],
            [("add", "/todo add <text>"), ("list", "")],
        )
        self.assertEqual(commands["notes"].description, "Personal notes")
        self.assertEqual([s.name for s in commands["notes"].subcommands], ["list"])

    async def test_the_subcommand_handler_sees_only_its_own_arguments(self) -> None:
        plugin = Features()
        response = await plugin.on_execute_command(request("todo", "add buy milk"))
        self.assertEqual(texts(response), ["added ['buy', 'milk']"])
        self.assertEqual(plugin.items, ["buy milk"])

    async def test_a_group_without_handler_answers_with_its_subcommands(self) -> None:
        for text in ("", "frobnicate"):
            with self.subTest(text=text):
                response = await Features().on_execute_command(request("todo", text))
                self.assertTrue(response.success)
                self.assertEqual(
                    texts(response),
                    ["/todo add <text> — Add a todo\n/todo list — List todos"],
                )

    async def test_a_group_handler_takes_what_no_subcommand_matches(self) -> None:
        response = await Features().on_execute_command(request("notes", "x y"))
        self.assertEqual(texts(response), ["notes root ['x', 'y']"])

    def test_subcommands_cannot_carry_group_level_settings(self) -> None:
        with self.assertRaises(ValueError):
            command("todo add", access="admins")


class TestTools(unittest.IsolatedAsyncioTestCase):
    def test_an_inferred_tool_is_described_by_its_signature(self) -> None:
        tools = {t.name: t for t in Features().meta().tools}
        self.assertEqual(tools["greet"].description, "Greets someone.")
        self.assertEqual(
            MessageToDict(tools["greet"].parameters),
            {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "times": {"type": "integer", "default": 1.0},
                },
                "required": ["name"],
            },
        )

    async def test_inferred_tools_get_keyword_arguments_and_the_event(self) -> None:
        result = await Features().on_call_tool(tool_call("greet", {"name": "Ann", "times": 2}))
        self.assertTrue(result.success, result.error_message)
        self.assertEqual(result.structured_result["result"], "hi hi Ann from u1")

        no_chat = await Features().on_call_tool(tool_call("greet", {"name": "Bo"}, False))
        self.assertEqual(no_chat.structured_result["result"], "hi Bo from nobody")

    async def test_unexpected_arguments_are_reported_to_the_model(self) -> None:
        result = await Features().on_call_tool(tool_call("greet", {"name": "A", "mood": "x"}))
        self.assertFalse(result.success)
        self.assertIn("unexpected arguments ['mood']", result.error_message)

    async def test_explicit_schemas_keep_the_dict_convention_and_lists_stay_lists(self) -> None:
        result = await Features().on_call_tool(tool_call("legacy", {"x": 2}))
        self.assertEqual(MessageToDict(result.structured_result), {"result": [2.0, 2.0]})

    async def test_runtime_tools_are_announced_and_rolled_back_when_the_node_refuses(self) -> None:
        plugin = Features()
        stub = connected(plugin)

        async def roll(sides: int) -> int:
            """Rolls a die."""
            return sides

        await plugin.add_tool(roll)
        self.assertIn("roll", {t.name for t in plugin.meta().tools})
        self.assertEqual(stub.calls[-1].host_id, "host_features")
        result = await plugin.on_call_tool(tool_call("roll", {"sides": 6}))
        self.assertEqual(result.structured_result["result"], 6)

        stub.refresh_error = grpc.aio.AioRpcError(
            grpc.StatusCode.UNAVAILABLE, grpc.aio.Metadata(), grpc.aio.Metadata()
        )
        with self.assertRaises(grpc.aio.AioRpcError):
            await plugin.remove_tool("roll")
        self.assertIn("roll", {t.name for t in plugin.meta().tools}, "a refused removal keeps it")
        with self.assertRaises(grpc.aio.AioRpcError):
            await plugin.add_tool(roll, name="roll2")
        self.assertNotIn("roll2", {t.name for t in plugin.meta().tools})


class TestHooksAndRoutes(unittest.IsolatedAsyncioTestCase):
    def test_meta_declares_the_rewriter_and_the_routes(self) -> None:
        meta = Features().meta()
        self.assertTrue(meta.rewrites_system_prompt)
        self.assertTrue(meta.serves_http)

    async def test_the_rewriter_replaces_or_keeps_the_prompt(self) -> None:
        context = request("x").context
        rewritten = await Features().on_llm_request(
            pb.LlmRequestHookRequest(context=context, session_id="s1", system_prompt="Be kind.")
        )
        self.assertEqual(rewritten.system_prompt, "Be kind.\nRules for group:g1.")
        kept = await Features().on_llm_request(
            pb.LlmRequestHookRequest(context=context, session_id="keep", system_prompt="Be kind.")
        )
        self.assertFalse(kept.HasField("system_prompt"))

    async def test_routes_answer_by_path_and_method(self) -> None:
        plugin = Features()

        ok = await plugin.on_http_request(pb.HttpRequest(method="GET", path="/stats", query="page=2"))
        self.assertEqual(ok.status, 200)
        self.assertEqual(json.loads(ok.body), {"page": "2", "items": 0})

        created = await plugin.on_http_request(
            pb.HttpRequest(method="POST", path="/echo", body=b'{"a": 1}')
        )
        self.assertEqual((created.status, json.loads(created.body)), (201, {"a": 1}))

        wrong = await plugin.on_http_request(pb.HttpRequest(method="GET", path="/echo"))
        self.assertEqual(wrong.status, 405)
        self.assertIn(pb.HttpHeader(name="allow", value="POST"), wrong.headers)

        missing = await plugin.on_http_request(pb.HttpRequest(method="GET", path="/nope"))
        self.assertEqual(missing.status, 404)

    async def test_a_failing_route_hides_the_error_from_the_caller(self) -> None:
        failed = await Features().on_http_request(pb.HttpRequest(method="GET", path="/broken"))
        self.assertEqual(failed.status, 500)
        self.assertNotIn(b"secret", failed.body)


class TestCoreCalls(unittest.IsolatedAsyncioTestCase):
    async def test_kv_stores_json_and_reports_foreign_bytes(self) -> None:
        plugin = Features()
        stub = connected(plugin)

        self.assertEqual(await plugin.kv.get("visits", 0), 0)
        await plugin.kv.set("visits", {"u1": 3}, ttl=60)
        self.assertEqual(await plugin.kv.get("visits"), {"u1": 3})
        self.assertEqual((stub.calls[-1].plugin_id, stub.calls[-1].ttl_seconds), ("test.features", 60))
        self.assertEqual(await plugin.kv.keys("vis"), ["visits"])
        self.assertTrue(await plugin.kv.delete("visits"))

        stub.kv["raw"] = b"\xff\x00"
        with self.assertRaises(ValueError):
            await plugin.kv.get("raw")
        with self.assertRaises(ValueError):
            await plugin.kv.set("x", 1, ttl=0)

    def test_kv_needs_a_node(self) -> None:
        with self.assertRaises(RuntimeError):
            Features().kv

    async def test_run_agent_sends_the_chat_and_returns_the_answer(self) -> None:
        plugin = Features()
        stub = connected(plugin)
        event = MessageEvent(request("x").context)
        result = await plugin.core.run_agent(
            "summarize", event=event, in_conversation=True, images=[MessageSegment.image_url("u")]
        )
        self.assertEqual((result.content, result.tools, result.session_id), ("done", ["search"], "s9"))
        sent = stub.calls[-1]
        self.assertEqual((sent.plugin_id, sent.context.sender_id), ("test.features", "u1"))
        self.assertTrue(sent.in_conversation and sent.use_tools)
        self.assertEqual(sent.images[0].url, "u")

    async def test_conversation_calls_name_the_chat(self) -> None:
        plugin = Features()
        stub = connected(plugin)
        event = MessageEvent(request("x").context)

        listed = await plugin.core.switch_conversation(event, "b")
        self.assertEqual([(c.session_id, c.current) for c in listed], [("a", False), ("b", True)])
        self.assertEqual(stub.calls[-1].context.channel_id, "group:g1")

        session = await plugin.core.append_conversation(event, [("user", "q"), ("assistant", "a")])
        self.assertEqual(session, "s1")
        self.assertEqual(
            [(m.role, m.text) for m in stub.calls[-1].messages],
            [(pb.LLM_ROLE_USER, "q"), (pb.LLM_ROLE_ASSISTANT, "a")],
        )
        with self.assertRaises(ValueError):
            await plugin.core.append_conversation(event, [("system", "no")])

    async def test_rendering_returns_a_sendable_image(self) -> None:
        plugin = Features()
        connected(plugin)
        segment = await plugin.core.render_text("# Title\nbody", width=400)
        self.assertEqual(segment.image.file_path, "/render/card.png")
        self.assertEqual(segment.image.mime_type, "image/png")


if __name__ == "__main__":
    unittest.main()
