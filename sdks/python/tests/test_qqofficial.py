"""Unit tests for Kanon QQ Official platform adapter plugin."""

import asyncio
from pathlib import Path
from types import SimpleNamespace
from unittest import IsolatedAsyncioTestCase
from unittest.mock import AsyncMock, MagicMock, patch

import sys
# Ensure sdks/python and plugins/qqofficial are on sys.path
_repo_root = Path(__file__).resolve().parents[3]
_python_sdk_dir = _repo_root / "sdks" / "python"
_plugin_dir = _repo_root / "plugins" / "qqofficial"
for p in (str(_python_sdk_dir), str(_plugin_dir)):
    if p not in sys.path:
        sys.path.insert(0, p)

from google.protobuf.json_format import MessageToDict

from kanon_sdk.context import MessageSegment, PluginContext
from kanon_sdk.proto import pb
from main import KanonBotClient, QQOfficialAdapter


class TestQQOfficialAdapter(IsolatedAsyncioTestCase):
    """Tests for QQOfficialAdapter and KanonBotClient."""

    def setUp(self) -> None:
        self.adapter = QQOfficialAdapter()

    def test_adapter_metadata(self) -> None:
        """Verifies static plugin metadata declarations."""
        self.assertEqual(self.adapter.id, "org.kanon.adapter.qqofficial")
        self.assertEqual(self.adapter.name, "QQ Official Adapter")
        meta = self.adapter.meta()
        self.assertEqual(meta.id, "org.kanon.adapter.qqofficial")
        self.assertEqual(meta.name, "QQ Official Adapter")

    async def test_on_load_missing_credentials(self) -> None:
        """Verifies idle state when appid/secret are omitted in config."""
        ctx = PluginContext(data_dir=Path("/tmp/kanon_test"), config={})
        await self.adapter.on_load(ctx)
        self.assertIsNone(self.adapter.bot_client)

    async def test_on_deliver_without_client(self) -> None:
        """Verifies explicit failure when delivering message while disconnected."""
        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:12345",
            recipient_id="user_1",
            segments=[MessageSegment.text("Hello")],
            event_id="msg_001",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertFalse(resp.success)
        self.assertIn("disconnected", resp.error_message)

    async def test_inbound_group_at_message_create(self) -> None:
        """Verifies inbound group @ mention event extraction and Fast-ACK ingestion."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True, event_id="evt_01"))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)

        # Mock GroupMessage structure
        mock_msg = MagicMock()
        mock_msg.group_openid = "test_group_openid_123"
        mock_msg.author.member_openid = "test_member_openid_456"
        mock_msg.content = "  /help command  "
        mock_msg.id = "qq_msg_group_999"
        mock_msg.mentions = []

        await client.on_group_at_message_create(mock_msg)

        mock_core.ingest_event.assert_awaited_once_with(
            platform="qqofficial",
            channel_id="group:test_group_openid_123",
            sender_id="test_member_openid_456",
            text="/help command",
            event_id="qq_msg_group_999",
            metadata={
                "msg_id": "qq_msg_group_999",
                "scene": "group",
                "mentions": [],
                "kanon.conversation_kind": "group",
                "kanon.bot_mentioned": True,
            },
            segments=[{"text": {"content": "/help command"}}],
        )

    async def test_inbound_c2c_message_create(self) -> None:
        """Verifies inbound private direct message (C2C) event extraction."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True, event_id="evt_02"))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)

        # Mock C2CMessage structure
        mock_msg = MagicMock()
        mock_msg.author.user_openid = "test_user_openid_789"
        mock_msg.content = "ping"
        mock_msg.id = "qq_msg_c2c_888"

        await client.on_c2c_message_create(mock_msg)

        mock_core.ingest_event.assert_awaited_once_with(
            platform="qqofficial",
            channel_id="c2c:test_user_openid_789",
            sender_id="test_user_openid_789",
            text="ping",
            event_id="qq_msg_c2c_888",
            metadata={
                "msg_id": "qq_msg_c2c_888",
                "scene": "c2c",
                "kanon.conversation_kind": "private",
                "kanon.bot_mentioned": False,
            },
            segments=[{"text": {"content": "ping"}}],
        )

    async def test_inbound_at_message_create_guild(self) -> None:
        """Verifies inbound guild channel @ mention event extraction."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True, event_id="evt_03"))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)

        # Mock guild Message structure
        mock_msg = MagicMock()
        mock_msg.channel_id = "test_guild_chan_111"
        mock_msg.guild_id = "test_guild_999"
        mock_msg.author.id = "test_guild_author_222"
        mock_msg.content = "hello guild"
        mock_msg.id = "qq_msg_guild_777"

        await client.on_at_message_create(mock_msg)

        mock_core.ingest_event.assert_awaited_once_with(
            platform="qqofficial",
            channel_id="guild:test_guild_chan_111",
            sender_id="test_guild_author_222",
            text="hello guild",
            event_id="qq_msg_guild_777",
            metadata={
                "msg_id": "qq_msg_guild_777",
                "scene": "guild",
                "guild_id": "test_guild_999",
                "kanon.conversation_kind": "channel",
                "kanon.bot_mentioned": True,
            },
            segments=[{"text": {"content": "hello guild"}}],
        )

    async def test_group_at_message_reports_policy_and_rich_segments(self) -> None:
        """Verifies rich inbound content reaches Core as typed segments.

        The core renders the model-visible message from ``segments``, so an
        attachment type the proto cannot express (video, arbitrary file) must
        still arrive as a named ``custom`` segment instead of being dropped.
        """
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.group_openid = "grp_rich"
        mock_msg.author.member_openid = "member_rich"
        mock_msg.content = "look !"
        mock_msg.id = "qq_msg_rich"
        mock_msg.mentions = [SimpleNamespace(member_openid="mention_1", username="Alice")]
        mock_msg.attachments = [
            SimpleNamespace(
                url="https://cdn.example/img.png",
                content_type="image/png",
                filename="img.png",
            ),
            SimpleNamespace(url="https://cdn.example/a.mp3", content_type="audio/mpeg", filename="a.mp3"),
            SimpleNamespace(url="https://cdn.example/v.mp4", content_type="video/mp4", filename="v.mp4"),
            SimpleNamespace(
                url="https://cdn.example/doc.pdf",
                content_type="application/pdf",
                filename="doc.pdf",
            ),
        ]

        await client.on_group_at_message_create(mock_msg)

        kwargs = mock_core.ingest_event.await_args.kwargs
        # The two platform-neutral keys are what make "only reply when @'d" work.
        self.assertEqual(kwargs["metadata"]["kanon.conversation_kind"], "group")
        self.assertTrue(kwargs["metadata"]["kanon.bot_mentioned"])
        self.assertEqual(kwargs["metadata"]["mentions"], ["mention_1"])
        self.assertEqual(
            kwargs["segments"],
            [
                {"text": {"content": "look !"}},
                {"mention": {"target_user_id": "mention_1", "display_name": "Alice"}},
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
                        "payload": {
                            "url": "https://cdn.example/v.mp4",
                            "content_type": "video/mp4",
                        },
                    }
                },
                {
                    "custom": {
                        "type_name": "qqofficial.file",
                        "payload": {
                            "url": "https://cdn.example/doc.pdf",
                            "content_type": "application/pdf",
                            "filename": "doc.pdf",
                        },
                    }
                },
            ],
        )

    async def test_guild_mentions_use_the_member_id_attribute(self) -> None:
        """Guild mention entries expose ``id`` where group entries expose ``member_openid``."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.channel_id = "chan_mentions"
        mock_msg.guild_id = "guild_mentions"
        mock_msg.author.id = "author_mentions"
        mock_msg.content = "hi"
        mock_msg.id = "qq_msg_mentions"
        mock_msg.mentions = [SimpleNamespace(id="guild_user_1", username="Bob")]
        mock_msg.attachments = []

        await client.on_at_message_create(mock_msg)

        kwargs = mock_core.ingest_event.await_args.kwargs
        self.assertEqual(kwargs["metadata"]["kanon.conversation_kind"], "channel")
        self.assertTrue(kwargs["metadata"]["kanon.bot_mentioned"])
        self.assertIn(
            {"mention": {"target_user_id": "guild_user_1", "display_name": "Bob"}},
            kwargs["segments"],
        )

    async def test_attachment_without_url_is_skipped_not_fatal(self) -> None:
        """One unreadable attachment must not cost the model the rest of the message."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.group_openid = "grp_empty_att"
        mock_msg.author.member_openid = "member_empty_att"
        mock_msg.content = "text survives"
        mock_msg.id = "qq_msg_empty_att"
        mock_msg.mentions = []
        mock_msg.attachments = [SimpleNamespace(url="", content_type="image/png", filename="")]

        await client.on_group_at_message_create(mock_msg)

        kwargs = mock_core.ingest_event.await_args.kwargs
        self.assertEqual(kwargs["segments"], [{"text": {"content": "text survives"}}])

    async def test_unmentioned_group_message_is_not_marked_as_mentioned(self) -> None:
        """Unmentioned callbacks must report the mention policy fact as false."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.group_openid = "grp_unmentioned"
        mock_msg.author.member_openid = "member_unmentioned"
        mock_msg.content = "chatter"
        mock_msg.id = "qq_msg_unmentioned"
        mock_msg.mentions = []
        mock_msg.attachments = []

        await client.on_group_message_create(mock_msg)

        metadata = mock_core.ingest_event.await_args.kwargs["metadata"]
        self.assertEqual(metadata["kanon.conversation_kind"], "group")
        self.assertFalse(metadata["kanon.bot_mentioned"])

    async def test_unmentioned_guild_message_is_channel_kind(self) -> None:
        """Unmentioned guild callbacks are channel conversations the policy governs."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.channel_id = "chan_unmentioned"
        mock_msg.guild_id = "guild_unmentioned"
        mock_msg.author.id = "author_unmentioned"
        mock_msg.content = "chatter"
        mock_msg.id = "qq_msg_guild_unmentioned"
        mock_msg.mentions = []
        mock_msg.attachments = []

        await client.on_message_create(mock_msg)

        metadata = mock_core.ingest_event.await_args.kwargs["metadata"]
        self.assertEqual(metadata["kanon.conversation_kind"], "channel")
        self.assertFalse(metadata["kanon.bot_mentioned"])

    async def test_guild_direct_message_is_private_kind(self) -> None:
        """A guild DM is one-to-one, so Core must always answer it."""
        mock_core = MagicMock()
        mock_core.ingest_event = AsyncMock(return_value=pb.IngestEventResponse(accepted=True))
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), core=mock_core)

        client = KanonBotClient(adapter=self.adapter)
        mock_msg = MagicMock()
        mock_msg.channel_id = "dm_chan"
        mock_msg.author.id = "dm_author"
        mock_msg.content = "dm text"
        mock_msg.id = "qq_msg_dm"
        mock_msg.mentions = []
        mock_msg.attachments = []

        await client.on_direct_message_create(mock_msg)

        metadata = mock_core.ingest_event.await_args.kwargs["metadata"]
        self.assertEqual(metadata["kanon.conversation_kind"], "private")
        self.assertFalse(metadata["kanon.bot_mentioned"])

    async def test_deliver_group_message(self) -> None:
        """Verifies outbound delivery to group channel."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_group_message = AsyncMock(return_value={"id": "qq_resp_1"})
        self.adapter.bot_client = mock_client
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={"use_markdown": False})

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:grp_abc",
            recipient_id="user_xyz",
            segments=[MessageSegment.text("Group reply text")],
            event_id="evt_reply_1",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)
        self.assertEqual(resp.message_id, "evt_reply_1")

        mock_client.api.post_group_message.assert_awaited_once_with(
            group_openid="grp_abc",
            msg_type=0,
            content="Group reply text",
            msg_id="evt_reply_1",
            msg_seq=1,
        )

    async def test_deliver_group_message_with_image(self) -> None:
        """Verifies outbound delivery to group with uploaded image."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        fake_media = MagicMock()
        mock_client.api.post_group_file = AsyncMock(return_value=fake_media)
        mock_client.api.post_group_message = AsyncMock(return_value={"id": "qq_resp_2"})
        self.adapter.bot_client = mock_client

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:grp_img",
            segments=[
                MessageSegment.text("Look at this image:"),
                MessageSegment.image_url("https://example.com/pic.jpg"),
            ],
            event_id="evt_reply_img",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)

        mock_client.api.post_group_file.assert_awaited_once_with(
            group_openid="grp_img",
            file_type=1,
            url="https://example.com/pic.jpg",
            srv_send_msg=False,
        )
        mock_client.api.post_group_message.assert_awaited_once_with(
            group_openid="grp_img",
            msg_type=7,
            media=fake_media,
            content="Look at this image:",
            msg_id="evt_reply_img",
            msg_seq=1,
        )

    async def test_local_image_is_chunk_uploaded_not_sent_as_a_url(self) -> None:
        """A local path must never reach the inline endpoint, which only accepts a public URL.

        QQ answers 40093010 上传URL错误 when it is handed a path, and the documented
        ``file_data`` alternative is still "暂未支持", so the chunked protocol is the only way to
        deliver a file the node generated itself.
        """
        import tempfile

        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_group_file = AsyncMock()
        mock_client.api.post_group_message = AsyncMock(return_value={"id": "qq_resp_local"})
        uploader = MagicMock()
        uploader.upload_group = AsyncMock(return_value={"file_info": "info", "file_uuid": "uuid"})
        self.adapter.bot_client = mock_client
        self.adapter._uploader = uploader

        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / "b50.png"
            image.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * 64)

            req = pb.DeliverMessageRequest(
                platform="qqofficial",
                channel_id="group:grp_local",
                segments=[
                    MessageSegment.text("这是你的 B50"),
                    MessageSegment.image_file(str(image)),
                ],
                event_id="evt_reply_local",
            )
            resp = await self.adapter.on_deliver_message(req)

        self.assertTrue(resp.success, resp.error_message)
        uploader.upload_group.assert_awaited_once_with(
            file_path=image,
            file_type=1,
            file_name="b50.png",
            group_openid="grp_local",
        )
        mock_client.api.post_group_file.assert_not_awaited()

        # The uploaded `file_info` must reach the message send as a rich-media payload; `Media` is
        # a TypedDict, so the uploader's dict is exactly the shape the API expects.
        _, kwargs = mock_client.api.post_group_message.await_args
        self.assertEqual(kwargs["msg_type"], 7)
        self.assertEqual(
            kwargs["media"], {"file_info": "info", "file_uuid": "uuid"}
        )
        self.assertEqual(kwargs["content"], "这是你的 B50")

    async def test_local_image_for_c2c_uses_the_user_upload_path(self) -> None:
        """C2C uploads must go to /v2/users/{openid}, not the group endpoint."""
        import tempfile

        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_c2c_file = AsyncMock()
        mock_client.api.post_c2c_message = AsyncMock(return_value={"id": "qq_resp_c2c_local"})
        uploader = MagicMock()
        uploader.upload_c2c = AsyncMock(return_value={"file_info": "info", "file_uuid": "uuid"})
        self.adapter.bot_client = mock_client
        self.adapter._uploader = uploader

        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / "chart.png"
            image.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * 64)

            req = pb.DeliverMessageRequest(
                platform="qqofficial",
                channel_id="c2c:user_openid_1",
                segments=[MessageSegment.image_file(str(image))],
                event_id="evt_reply_c2c_local",
            )
            resp = await self.adapter.on_deliver_message(req)

        self.assertTrue(resp.success, resp.error_message)
        uploader.upload_c2c.assert_awaited_once_with(
            file_path=image,
            file_type=1,
            file_name="chart.png",
            user_openid="user_openid_1",
        )
        mock_client.api.post_c2c_file.assert_not_awaited()

        _, kwargs = mock_client.api.post_c2c_message.await_args
        self.assertEqual(kwargs["msg_type"], 7)
        self.assertEqual(
            kwargs["media"], {"file_info": "info", "file_uuid": "uuid"}
        )

    async def test_unreadable_media_source_is_reported(self) -> None:
        """A source that is neither a file nor an http(s) URL fails loudly, never silently."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_group_message = AsyncMock()
        self.adapter.bot_client = mock_client

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:grp_bad",
            segments=[MessageSegment.image_file("/nonexistent/path/pic.png")],
            event_id="evt_reply_bad_media",
        )
        resp = await self.adapter.on_deliver_message(req)

        self.assertFalse(resp.success)
        self.assertIn("neither a readable file nor an http(s) URL", resp.error_message)

    async def test_deliver_c2c_message(self) -> None:
        """Verifies outbound delivery to direct c2c conversation."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_c2c_message = AsyncMock(return_value={"id": "qq_resp_3"})
        self.adapter.bot_client = mock_client
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={"use_markdown": False})

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="c2c:user_direct_1",
            segments=[MessageSegment.text("Direct message hello")],
            event_id="evt_c2c_1",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)
        mock_client.api.post_c2c_message.assert_awaited_once_with(
            openid="user_direct_1",
            msg_type=0,
            content="Direct message hello",
            msg_id="evt_c2c_1",
            msg_seq=1,
        )

    async def test_deliver_guild_message(self) -> None:
        """Verifies outbound delivery to guild channel."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_message = AsyncMock(return_value={"id": "qq_resp_4"})
        self.adapter.bot_client = mock_client

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="guild:chan_guild_1",
            segments=[
                MessageSegment.text("Guild response"),
                MessageSegment.image_url("https://example.com/guild.png"),
            ],
            event_id="evt_guild_1",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)
        mock_client.api.post_message.assert_awaited_once_with(
            channel_id="chan_guild_1",
            content="Guild response",
            image="https://example.com/guild.png",
            msg_id="evt_guild_1",
        )

    async def test_deliver_unsupported_channel_scheme(self) -> None:
        """Verifies rejection of unsupported channel prefix."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        self.adapter.bot_client = mock_client

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="unknown_scheme:12345",
            segments=[MessageSegment.text("Test")],
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertFalse(resp.success)
        self.assertIn("Unsupported channel scheme", resp.error_message)

    def test_adapter_exposes_no_llm_tools(self) -> None:
        """Verifies the adapter declares no tools: binding is an operator action, not a tool.

        A tool advertised by an adapter is offered to the model, which then tries to bind
        credentials mid-conversation; the adapter must stay a transport, so the QR flows live in
        the management-action registry instead.
        """
        meta = self.adapter.meta()
        self.assertEqual([t.name for t in meta.tools], [])
        self.assertEqual(
            sorted(self.adapter._action_handlers),
            ["qq_poll_login_result", "qq_request_login_qr"],
        )

    async def test_action_request_login_qr(self) -> None:
        """Verifies the qq_request_login_qr management action."""
        from auth import QQOfficialLoginRegistration

        fake_reg = QQOfficialLoginRegistration(
            task_id="task_qr_123",
            bind_key="bind_key_456",
            qrcode="https://q.qq.com/connect?task_id=task_qr_123",
            interval=2,
        )
        with patch("main.request_qqofficial_login_qr", AsyncMock(return_value=fake_reg)):
            resp = await self.adapter.on_invoke_action(
                "org.kanon.adapter.qqofficial", "qq_request_login_qr", {}
            )
            self.assertTrue(resp.success, resp.error_message)
            res = MessageToDict(resp.result)
            self.assertEqual(res["task_id"], "task_qr_123")
            self.assertEqual(res["bind_key"], "bind_key_456")
            self.assertEqual(res["qrcode_url"], "https://q.qq.com/connect?task_id=task_qr_123")
            self.assertEqual(res["poll_interval_seconds"], 2)

    async def test_action_poll_login_result(self) -> None:
        """Verifies the qq_poll_login_result management action and credential sync."""
        from unittest.mock import patch

        fake_poll = {
            "status": "created",
            "qr_status": 2,
            "appid": "102888123",
            "secret": "decrypted_secret_val",
        }
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={})
        with patch("main.poll_qqofficial_login_once", AsyncMock(return_value=fake_poll)):
            resp = await self.adapter.on_invoke_action(
                "org.kanon.adapter.qqofficial",
                "qq_poll_login_result",
                {"task_id": "t1", "bind_key": "k1"},
            )
            self.assertTrue(resp.success, resp.error_message)
            res = MessageToDict(resp.result)
            self.assertEqual(res["status"], "created")
            self.assertEqual(res["appid"], "102888123")
            self.assertEqual(res["secret"], "decrypted_secret_val")
            self.assertEqual(self.adapter.context.config["appid"], "102888123")
            self.assertEqual(self.adapter.context.config["secret"], "decrypted_secret_val")

    async def test_deliver_markdown_auto_fallback_on_40054005(self) -> None:
        """Verifies automatic fallback to plain text when QQ API rejects native Markdown (40054005)."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        # Markdown post fails with 40054005 permission error
        mock_client.api.post_group_message = AsyncMock(side_effect=[
            RuntimeError("QQ API Error: 40054005 不允许发送原生 markdown"),
            {"id": "qq_fallback_ok"},
        ])
        self.adapter.bot_client = mock_client
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={"use_markdown": True})

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:grp_fallback",
            recipient_id="user_xyz",
            segments=[MessageSegment.text("**Bold message**")],
            event_id="evt_fb_1",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)
        self.assertEqual(mock_client.api.post_group_message.await_count, 2)
        # Second call is plain text msg_type=0
        fallback_call = mock_client.api.post_group_message.await_args_list[1]
        self.assertEqual(fallback_call.kwargs["msg_type"], 0)
        self.assertEqual(fallback_call.kwargs["content"], "**Bold message**")

    async def test_on_config_reload_starts_bot_client(self) -> None:
        """Verifies hot reload starts or restarts the bot client when credentials are provided."""
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={})
        self.assertIsNone(self.adapter.bot_client)

        with patch.object(self.adapter, "_start_bot_client", AsyncMock()) as mock_start:
            await self.adapter.on_config_reload({"appid": "12345", "secret": "abcde"})
            mock_start.assert_awaited_once()

    async def test_deliver_group_msg_id_rejected_fallback_active(self) -> None:
        """Verifies automatic fallback to active message when passive msg_id is rejected by QQ."""
        mock_client = MagicMock()
        mock_client.is_closed.return_value = False
        mock_client.api.post_group_message = AsyncMock(side_effect=[
            RuntimeError("botpy.errors.ServerError: 请求参数msg_id无效或越权 (code: 304023)"),
            {"id": "qq_active_ok"},
        ])
        self.adapter.bot_client = mock_client
        self.adapter.context = PluginContext(data_dir=Path("/tmp"), config={"use_markdown": False})

        req = pb.DeliverMessageRequest(
            platform="qqofficial",
            channel_id="group:grp_test",
            recipient_id="user_xyz",
            segments=[MessageSegment.text("Reply after timeout")],
            event_id="expired_msg_id",
        )
        resp = await self.adapter.on_deliver_message(req)
        self.assertTrue(resp.success)
        self.assertEqual(mock_client.api.post_group_message.await_count, 2)
        # Second call should retry with msg_id=None
        second_call = mock_client.api.post_group_message.await_args_list[1]
        self.assertIsNone(second_call.kwargs["msg_id"])
        self.assertEqual(second_call.kwargs["content"], "Reply after timeout")

    async def test_bot_client_on_ready(self) -> None:
        """Verifies on_ready callback executes and logs bot online status without crashing."""
        client = KanonBotClient(adapter=self.adapter)
        client._connection = MagicMock()
        client._connection.state.robot.name = "TestRobot"
        await client.on_ready()

    async def test_unknown_action_is_reported_not_raised(self) -> None:
        """Verifies an unknown action answers a structured error instead of raising."""
        resp = await self.adapter.on_invoke_action(
            "org.kanon.adapter.qqofficial", "not_a_real_action", {}
        )
        self.assertFalse(resp.success)
        self.assertIn("Unknown action", resp.error_message)
