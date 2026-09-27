"""QQ Official platform adapter plugin for Kanon.

Provides full-featured integration with the Tencent QQ Open Platform:
- WebSocket gateway lifecycle management and persistent connection monitoring.
- Inbound event routing for Group @, unmentioned group, C2C private, Guild channels,
  Direct Messages, and bot/friend relationship notices.
- Native Markdown formatting with automatic plain-text fallback on permission denial.
- Outbound media delivery for images, voice, video, and files with chunked uploads (> 10MB).
- QR code quick binding and credentials management for the Kanon WebUI console.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional

# Ensure kanon_sdk and local modules are resolvable on sys.path
_current_dir = Path(__file__).resolve().parent
_repo_root = _current_dir.parents[1]
_sdk_path = str(_repo_root / "sdks" / "python")
for p in (str(_current_dir), _sdk_path):
    if p not in sys.path:
        sys.path.insert(0, p)

import botpy
from kanon_sdk import CoreHandle, Plugin, PluginContext, action, connect_core_channel
from kanon_sdk.host import KanonHost
from kanon_sdk.proto import pb, pb_grpc

from auth import (
    poll_qqofficial_login_once,
    request_qqofficial_login_qr,
)
from client import KanonBotClient
from delivery import QQDeliveryManager
from upload import QQOfficialChunkedUploader

# Re-export KanonBotClient for external consumers and backward compatibility
__all__ = ["KanonBotClient", "QQDeliveryManager", "QQOfficialAdapter"]


def _patch_qq_botpy_formdata() -> None:
    """Patches qq-botpy for aiohttp>=3.12 compatibility where _is_processed was removed."""
    try:
        from botpy.http import _FormData  # type: ignore

        if not hasattr(_FormData, "_is_processed"):
            setattr(_FormData, "_is_processed", False)
    except Exception:
        pass


_patch_qq_botpy_formdata()


class QQOfficialAdapter(Plugin):
    """QQ Official platform adapter plugin for Kanon."""

    id = "org.kanon.adapter.qqofficial"
    name = "QQ Official Adapter"
    version = "0.2.0"
    author = "Kanon Dev"
    description = "Comprehensive QQ Official platform adapter for Kanon"
    priority = 100

    def __init__(self) -> None:
        super().__init__()
        self.bot_client: Optional[KanonBotClient] = None
        self._bot_task: Optional[asyncio.Task] = None
        self._uploader: Optional[QQOfficialChunkedUploader] = None
        self.delivery_manager = QQDeliveryManager(self)

    @property
    def uploader(self) -> Optional[QQOfficialChunkedUploader]:
        """Returns the chunked uploader instance for large file assets."""
        return self._uploader

    # --- Lifecycle Management ------------------------------------------------

    async def on_load(self, ctx: PluginContext) -> None:
        """Initializes plugin context and starts background WebSocket client if configured."""
        await super().on_load(ctx)
        await self._start_bot_client()

    async def on_unload(self) -> None:
        """Closes active WebSocket connection gracefully upon host shutdown."""
        await self._stop_bot_client()
        print("[QQOfficial] QQ Official adapter unloaded gracefully.", flush=True)

    async def on_config_reload(self, config: Dict[str, Any]) -> None:
        """Hot-reloads configuration and restarts the bot connection with new credentials."""
        print("[QQOfficial] Configuration reloaded; restarting QQ bot client...", flush=True)
        await self._start_bot_client()

    async def _start_bot_client(self) -> None:
        """Starts or restarts the QQ Official bot client with current configuration."""
        await self._stop_bot_client()

        if not self.context:
            return

        appid = self.context.config.get("appid")
        secret = self.context.config.get("secret")
        is_sandbox = bool(self.context.config.get("is_sandbox", False))

        if not appid or not secret:
            print("[QQOfficial] Missing appid or secret; adapter is in idle/unconfigured mode.", flush=True)
            return

        intents = botpy.Intents.none()
        intents.public_messages = True
        intents.public_guild_messages = True

        if self.context.config.get("enable_guild_direct_message", False):
            intents.direct_message = True

        if self.context.config.get("enable_guild_messages", False):
            intents.guild_messages = True

        self.bot_client = KanonBotClient(adapter=self, intents=intents, is_sandbox=is_sandbox)
        self._uploader = QQOfficialChunkedUploader(self.bot_client.api._http)

        async def _bot_runner() -> None:
            try:
                print(
                    f"[QQOfficial] Connecting to QQ WebSocket Gateway (appid={appid}, sandbox={is_sandbox})...",
                    flush=True,
                )
                await self.bot_client.start(appid=str(appid), secret=str(secret))
            except asyncio.CancelledError:
                pass
            except Exception as exc:
                print(f"[QQOfficial] Bot gateway error: {exc}", flush=True)

        self._bot_task = asyncio.create_task(_bot_runner())
        print(f"[QQOfficial] Started QQ Bot background task for appid: {appid}", flush=True)

    async def _stop_bot_client(self) -> None:
        """Stops the active bot client and cancels the background task."""
        if self.bot_client and not self.bot_client.is_closed():
            try:
                await self.bot_client.close()
            except Exception as e:
                print(f"[QQOfficial] Error closing bot client: {e}", flush=True)
        if self._bot_task and not self._bot_task.done():
            self._bot_task.cancel()
            try:
                await self._bot_task
            except (asyncio.CancelledError, Exception):
                pass
        self.bot_client = None
        self._bot_task = None

    # --- Inbound Event Forwarding (Fast-ACK) ---------------------------------

    async def ingest_qq_message(
        self,
        channel_id: str,
        sender_id: str,
        content: str,
        msg_id: str,
        scene: str = "",
        extra: Optional[Dict[str, Any]] = None,
        segments: Optional[List[Dict[str, Any]]] = None,
    ) -> None:
        """Forwards one inbound QQ message to Core, including its typed segments.

        ``segments`` is the proto-JSON form of the message content (text,
        mentions, attachments). It is appended last so existing positional and
        keyword callers keep working unchanged; adapters that pass no segments
        keep the previous text-only behaviour.
        """
        if not self.context:
            print(f"[QQOfficial] Dropped message {msg_id}: no plugin context initialized", flush=True)
            return

        if not self.context.core:
            # Lazily probe Core socket if it was not ready at initial startup
            core_sock_str = os.environ.get("KANON_CORE_SOCK", "./run/core.sock")
            core_sock = Path(core_sock_str).resolve()
            if core_sock.exists():
                try:
                    # The SDK helper owns channel construction: it pins the HTTP/2 authority
                    # that Tonic's h2 server requires, without which every RPC is reset with
                    # an opaque "Stream removed (RST_STREAM ... error code 1)".
                    core_channel = connect_core_channel(core_sock)
                    core_stub = pb_grpc.BotApiServiceStub(core_channel)
                    self.context.core = CoreHandle(core_stub)
                    print(f"[QQOfficial] Lazily connected to Kanon Core at {core_sock}", flush=True)
                except Exception as e:
                    print(f"[QQOfficial] Lazy Core connection attempt failed: {e}", flush=True)

        if not self.context.core:
            print(
                f"[QQOfficial] Dropped message {msg_id}: Kanon Core is offline or unreachable "
                "(ctx.core is None; ensure Kanon Core backend is running)",
                flush=True,
            )
            return

        metadata: Dict[str, Any] = {"msg_id": msg_id}
        if scene:
            metadata["scene"] = scene
        if extra:
            metadata.update(extra)

        try:
            resp = await self.context.core.ingest_event(
                platform="qqofficial",
                channel_id=channel_id,
                sender_id=sender_id,
                text=content,
                event_id=msg_id,
                metadata=metadata,
                segments=segments,
            )
            if not resp.accepted:
                print(f"[QQOfficial] Core queue backpressure: dropped message {msg_id}", flush=True)
            else:
                print(f"[QQOfficial] Successfully ingested message {msg_id} into Kanon Core", flush=True)
        except Exception as e:
            print(f"[QQOfficial] Ingest RPC failed: {e}", flush=True)

    async def ingest_notice(self, channel_id: str, event_type: str, data: Dict[str, Any]) -> None:
        """Pushes platform notice events (e.g. bot added/removed) into Core."""
        if not self.context or not self.context.core:
            return
        try:
            await self.context.core.ingest_event(
                platform="qqofficial",
                channel_id=channel_id,
                sender_id="system",
                text=f"[{event_type}]",
                metadata={"event_type": event_type, "notice_data": data},
            )
        except Exception as e:
            print(f"[QQOfficial] Notice ingest failed: {e}", flush=True)

    # --- Outbound Message Delivery -------------------------------------------

    async def on_deliver_message(
        self,
        req: pb.DeliverMessageRequest,
    ) -> pb.DeliverMessageResponse:
        """Delegates outbound delivery to the delivery manager."""
        return await self.delivery_manager.deliver_message(req)

    # Delegation helpers for backward compatibility with existing tests
    async def _safe_post_group_message(self, **kwargs: Any) -> Any:
        return await self.delivery_manager.safe_post_group_message(**kwargs)

    async def _safe_post_c2c_message(self, **kwargs: Any) -> Any:
        return await self.delivery_manager.safe_post_c2c_message(**kwargs)

    # --- WebUI Management Tools ----------------------------------------------

    @action(
        name="qq_request_login_qr",
        parameters={
            "type": "object",
            "properties": {
                "bind_host": {"type": "string", "description": "Optional bind host override, defaults to q.qq.com"}
            },
        },
    )
    async def handle_request_login_qr(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """Creates a QR binding task and returns QR URL and bind key."""
        config = dict(self.context.config) if self.context else {}
        if params.get("bind_host"):
            config["qqofficial_bind_host"] = params["bind_host"]

        registration = await request_qqofficial_login_qr(config)
        return {
            "task_id": registration.task_id,
            "bind_key": registration.bind_key,
            "qrcode_url": registration.qrcode,
            "poll_interval_seconds": registration.interval,
        }

    @action(
        name="qq_poll_login_result",
        parameters={
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Active task identifier"},
                "bind_key": {"type": "string", "description": "AES-256 decryption key for credentials"},
            },
            "required": ["task_id", "bind_key"],
        },
    )
    async def handle_poll_login_result(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """Polls QR bind status and automatically updates credentials upon scan success."""
        config = dict(self.context.config) if self.context else {}
        result = await poll_qqofficial_login_once(
            platform_config=config,
            task_id=params["task_id"],
            bind_key=params["bind_key"],
        )

        if result.get("status") == "created" and "appid" in result and "secret" in result:
            if self.context:
                self.context.config["appid"] = result["appid"]
                self.context.config["secret"] = result["secret"]
                config_path = self.context.data_dir / "config.json"
                try:
                    config_path.write_text(
                        json.dumps(self.context.config, indent=2, ensure_ascii=False),
                        encoding="utf-8",
                    )
                except Exception as e:
                    print(f"[QQOfficial] Failed to persist config: {e}", flush=True)
                # Immediately launch bot client so mobile QQ detects online_state = 1
                await self._start_bot_client()

        return result


if __name__ == "__main__":
    KanonHost(QQOfficialAdapter()).run()
