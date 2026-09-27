"""Unit tests for the SDK CoreHandle inbound-event primitive.

These tests pin the wire contract adapters rely on: text stays in ``raw_text``,
platform-neutral policy facts travel in metadata, and rich content is expressed as
typed proto-JSON segments instead of flattened strings.
"""

import sys
import unittest
from pathlib import Path
from typing import List

# Ensure sdks/python is importable when the test is run directly.
_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

from google.protobuf.json_format import MessageToDict

from kanon_sdk.context import CoreHandle
from kanon_sdk.proto import pb


class CapturingStub:
    """Fake ``BotApiService`` stub that records the request CoreHandle builds.

    The handle only ever calls ``IngestEvent`` on the shared channel, so a stub
    that captures the request is enough to assert the full wire payload without
    standing up a gRPC server.
    """

    def __init__(self) -> None:
        self.requests: List[pb.IngestEventRequest] = []

    async def IngestEvent(self, request: pb.IngestEventRequest) -> pb.IngestEventResponse:  # noqa: N802
        self.requests.append(request)
        return pb.IngestEventResponse(accepted=True, event_id=request.event.event_id)


class TestIngestEventSegments(unittest.IsolatedAsyncioTestCase):
    """Tests that CoreHandle converts proto-JSON segments into typed protobuf."""

    async def test_proto_json_segments_become_typed_segments(self) -> None:
        """A segment dict for every variant the QQ adapter emits must round-trip."""
        stub = CapturingStub()
        handle = CoreHandle(stub)  # type: ignore[arg-type]

        await handle.ingest_event(
            platform="qqofficial",
            channel_id="group:g1",
            sender_id="u1",
            text="hello",
            event_id="evt_1",
            metadata={
                "kanon.conversation_kind": "group",
                "kanon.bot_mentioned": True,
            },
            segments=[
                {"text": {"content": "hello"}},
                {"mention": {"target_user_id": "m1", "display_name": "Alice"}},
                {
                    "image": {
                        "url": "https://cdn.example/img.png",
                        "mime_type": "image/png",
                        "filename": "img.png",
                    }
                },
                {"audio": {"url": "https://cdn.example/a.mp3"}},
                {
                    "custom": {
                        "type_name": "qqofficial.video",
                        "payload": {"url": "https://cdn.example/v.mp4", "content_type": "video/mp4"},
                    }
                },
            ],
        )

        self.assertEqual(len(stub.requests), 1)
        event = stub.requests[0].event

        # Text stays in raw_text and is also the first model-visible segment.
        self.assertEqual(event.raw_text, "hello")
        self.assertEqual(
            [segment.WhichOneof("segment") for segment in event.segments],
            ["text", "mention", "image", "audio", "custom"],
        )
        self.assertEqual(event.segments[0].text.content, "hello")
        self.assertEqual(event.segments[1].mention.target_user_id, "m1")
        self.assertEqual(event.segments[1].mention.display_name, "Alice")
        self.assertEqual(event.segments[2].image.url, "https://cdn.example/img.png")
        self.assertEqual(event.segments[2].image.mime_type, "image/png")
        self.assertEqual(event.segments[2].image.filename, "img.png")
        self.assertEqual(event.segments[3].audio.url, "https://cdn.example/a.mp3")
        self.assertEqual(event.segments[4].custom.type_name, "qqofficial.video")
        self.assertEqual(
            MessageToDict(event.segments[4].custom.payload),
            {"url": "https://cdn.example/v.mp4", "content_type": "video/mp4"},
        )

        # Metadata must carry the policy facts as the JSON types Core reads:
        # a string kind and an actual boolean, not a stringified one.
        fields = event.metadata.fields
        self.assertEqual(fields["kanon.conversation_kind"].string_value, "group")
        self.assertTrue(fields["kanon.bot_mentioned"].bool_value)

    async def test_omitting_segments_keeps_text_only_behaviour(self) -> None:
        """Existing callers that pass only text must be unaffected by the new argument."""
        stub = CapturingStub()
        handle = CoreHandle(stub)  # type: ignore[arg-type]

        await handle.ingest_event(
            platform="telegram",
            channel_id="chat_1",
            sender_id="user_1",
            text="plain text",
            event_id="evt_2",
        )

        event = stub.requests[0].event
        self.assertEqual(event.raw_text, "plain text")
        self.assertEqual(list(event.segments), [])
        self.assertFalse(event.HasField("metadata"))

    async def test_segment_without_a_variant_is_rejected_before_the_rpc(self) -> None:
        """An empty dict parses silently but would ship a valueless segment."""
        stub = CapturingStub()
        handle = CoreHandle(stub)  # type: ignore[arg-type]

        with self.assertRaises(ValueError) as raised:
            await handle.ingest_event(
                platform="qqofficial",
                channel_id="group:g1",
                sender_id="u1",
                text="hello",
                segments=[{"text": {"content": "hello"}}, {}],
            )

        self.assertIn("segments[1]", str(raised.exception))
        self.assertEqual(stub.requests, [], "the malformed event must not reach Core")


class TestReplyContext(unittest.IsolatedAsyncioTestCase):
    """Replies retain the original native event and propagate delivery failures."""

    async def test_reply_preserves_context_and_waits_for_platform(self):
        import asyncio
        entered, release = asyncio.Event(), asyncio.Event()
        captured = []
        class Stub:
            async def ReplyMessage(self, request, *, timeout):
                captured.append((request, timeout))
                entered.set()
                await release.wait()
                return pb.DeliverMessageResponse(success=False, error_message="platform rejected")
        event = pb.PipelineEventRequest(platform="qqofficial", channel_id="group:test",
            sender_id="sender", event_id="native-message")
        call = asyncio.create_task(CoreHandle(Stub()).reply_to(event, []))
        await entered.wait()
        self.assertFalse(call.done())
        self.assertEqual(captured[0][0].event_id, "native-message")
        self.assertEqual(captured[0][0].recipient_id, "sender")
        self.assertEqual(captured[0][1], 35.0)
        release.set()
        self.assertFalse((await call).success)


if __name__ == "__main__":
    unittest.main()
