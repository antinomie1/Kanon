"""Unit tests for Kanon Python SDK Core IPC channel construction and host bootstrap.

These tests guard one concrete interop invariant: a Python gRPC client must never let
C-core derive the HTTP/2 ``:authority`` from a ``unix:<path>`` target, because C-core then
sends the percent-encoded socket path as the authority and Tonic's h2 server resets every
stream with ``RST_STREAM(PROTOCOL_ERROR)``. See :mod:`kanon_sdk.ipc` for the full analysis.
"""

import asyncio
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# Ensure sdks/python is importable when the test is run directly.
_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

import grpc

from kanon_sdk.host import KanonHost
from kanon_sdk.ipc import CORE_AUTHORITY, connect_core_channel
from kanon_sdk.plugin import Plugin
from kanon_sdk.proto import pb, pb_grpc


def _is_valid_authority(value: str) -> bool:
    """Returns whether ``value`` is a syntactically valid HTTP/2 authority.

    Mirrors the ``http`` crate's rule that ``h2`` enforces server-side: no percent escapes
    outside a userinfo/IPv6 zone identifier, no path separators, no whitespace.
    """
    if not value:
        return False
    if "%" in value or "/" in value or "?" in value or "#" in value:
        return False
    return not any(ch.isspace() for ch in value)


class TestConnectCoreChannel(unittest.IsolatedAsyncioTestCase):
    """Tests for the shared Core channel factory."""

    async def test_pins_default_authority(self) -> None:
        """Verifies the channel pins an explicit default authority for every RPC."""
        with patch.object(grpc.aio, "insecure_channel") as create_channel:
            connect_core_channel("/run/user/1000/kanon/run/core.sock")

        create_channel.assert_called_once()
        target = create_channel.call_args.args[0]
        options = dict(create_channel.call_args.kwargs["options"])

        self.assertEqual(target, "unix:/run/user/1000/kanon/run/core.sock")
        self.assertEqual(options.get("grpc.default_authority"), CORE_AUTHORITY)

    async def test_authority_is_valid_and_never_the_socket_path(self) -> None:
        """Verifies the pinned authority is a legal authority and leaks no path bytes."""
        self.assertTrue(_is_valid_authority(CORE_AUTHORITY))

        # A socket path is the one value that must never be promoted to an authority: it
        # contains '/' and, once C-core escapes it, '%', both of which h2 rejects.
        self.assertFalse(_is_valid_authority("run%2Fuser%2F1000%2Fkanon%2Frun%2Fcore.sock"))

    async def test_accepts_path_objects(self) -> None:
        """Verifies Path inputs are rendered into the ``unix:`` target unchanged."""
        with patch.object(grpc.aio, "insecure_channel") as create_channel:
            connect_core_channel(Path("/tmp/kanon-run-1000/core.sock"))

        self.assertEqual(
            create_channel.call_args.args[0],
            "unix:/tmp/kanon-run-1000/core.sock",
        )


class RegisterHostServicer(pb_grpc.BotApiServiceServicer):
    """Minimal Core stand-in that accepts host registration."""

    def __init__(self) -> None:
        self.registered_host_ids: list[str] = []

    async def RegisterHost(self, request, context):  # noqa: N802 - generated gRPC name
        self.registered_host_ids.append(request.host_id)
        return pb.RegisterHostResponse(success=True, message="ok")


class BootstrapProbePlugin(Plugin):
    """Plugin that records the context handed to it by the host."""

    id = "org.kanon.plugin.ipc_probe"
    name = "IPC Probe Plugin"
    version = "0.1.0"
    author = "Kanon Dev"
    description = "Test-only plugin used to observe host bootstrap"

    def __init__(self) -> None:
        super().__init__()
        self.loaded = asyncio.Event()

    async def on_load(self, ctx) -> None:
        await super().on_load(ctx)
        self.loaded.set()


class TestHostBootstrap(unittest.IsolatedAsyncioTestCase):
    """Tests that the host bootstraps its Core connection through the shared helper."""

    async def test_host_registers_over_core_socket(self) -> None:
        """Verifies RegisterHost succeeds and ctx.core is handed to the plugin."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            core_sock = tmp_dir / "core.sock"
            host_sock = tmp_dir / "host_probe.sock"

            servicer = RegisterHostServicer()
            server = grpc.aio.server()
            pb_grpc.add_BotApiServiceServicer_to_server(servicer, server)
            server.add_insecure_port(f"unix:{core_sock}")
            await server.start()

            calls: list[str] = []
            real_connect = connect_core_channel

            def spy_connect(path):
                # Spy that delegates to the real factory: the assertion below fails if the
                # host ever dials Core without going through the authority-pinning helper.
                calls.append(str(path))
                return real_connect(path)

            plugin = BootstrapProbePlugin()
            host = KanonHost(
                plugin,
                socket_path=host_sock,
                core_sock=core_sock,
                host_id="probe-host",
                data_dir=tmp_dir / "data",
            )

            shutdown = asyncio.Event()
            with patch("kanon_sdk.host.connect_core_channel", side_effect=spy_connect):
                runner = asyncio.create_task(host.run_async(shutdown_event=shutdown))
                try:
                    # The plugin's on_load only fires after registration succeeded, so waiting
                    # on it proves the whole bootstrap reached Core over the pinned channel.
                    await asyncio.wait_for(plugin.loaded.wait(), timeout=10)
                finally:
                    shutdown.set()
                    await asyncio.wait_for(runner, timeout=10)
                    await server.stop(grace=None)

            self.assertEqual(servicer.registered_host_ids, ["probe-host"])
            self.assertEqual(calls, [str(core_sock)])
            self.assertIsNotNone(plugin.context.core)


if __name__ == "__main__":
    unittest.main()


class StubCoreStub:
    """Fake BotApiService stub whose Ping either answers or raises."""

    def __init__(self, healthy: bool) -> None:
        self.healthy = healthy

    async def Ping(self, request, timeout=None):  # noqa: N802 - generated gRPC name
        if not self.healthy:
            raise RuntimeError("core is gone")
        return object()


class ProbingPb:
    """Minimal protobuf surface used by the watchdog."""

    @staticmethod
    def PingRequest(timestamp: int):
        return {"timestamp": timestamp}


class TestCoreWatchdog(unittest.IsolatedAsyncioTestCase):
    """A host must stop itself once the core it registered with is gone."""

    async def test_lost_core_triggers_the_stop_callback(self) -> None:
        from kanon_sdk.ipc import CoreWatchdog

        reasons: list[str] = []
        watchdog = CoreWatchdog(
            StubCoreStub(healthy=False),
            ProbingPb,
            interval=0.01,
            timeout=0.01,
            failures=2,
            on_lost=reasons.append,
        )
        watchdog.start()
        for _ in range(50):
            if reasons:
                break
            await asyncio.sleep(0.02)
        await watchdog.stop()

        self.assertEqual(len(reasons), 1, "the host must be told exactly once")
        self.assertIn("core unreachable", reasons[0])

    async def test_healthy_core_never_triggers_stop(self) -> None:
        from kanon_sdk.ipc import CoreWatchdog

        reasons: list[str] = []
        watchdog = CoreWatchdog(
            StubCoreStub(healthy=True),
            ProbingPb,
            interval=0.01,
            timeout=0.01,
            failures=2,
            on_lost=reasons.append,
        )
        watchdog.start()
        await asyncio.sleep(0.1)
        await watchdog.stop()

        self.assertEqual(reasons, [])


class TestBoundedShutdown(unittest.IsolatedAsyncioTestCase):
    """A hung plugin teardown must never keep a host alive as a ghost bot.

    The failure mode is real: closing a platform WebSocket can wait for a handshake that never
    arrives, and a host that stays alive after its core is gone keeps serving the platform — the
    next core then answers every message twice.
    """

    async def test_a_hung_step_is_abandoned_within_the_timeout(self) -> None:
        from kanon_sdk.host import _bounded

        cancelled = asyncio.Event()

        async def hangs() -> None:
            try:
                await asyncio.sleep(30)
            except asyncio.CancelledError:
                cancelled.set()
                raise

        await _bounded("hung step", hangs(), 0.05)
        self.assertTrue(cancelled.is_set(), "the hung step must be cancelled")

    async def test_a_failing_step_does_not_abort_shutdown(self) -> None:
        from kanon_sdk.host import _bounded

        async def fails() -> None:
            raise RuntimeError("gateway already closed")

        # Returning at all is the assertion: shutdown must continue past a broken step.
        await _bounded("failing step", fails(), 1.0)

    async def test_a_fast_step_completes(self) -> None:
        from kanon_sdk.host import _bounded

        done = asyncio.Event()

        async def quick() -> None:
            done.set()

        await _bounded("quick step", quick(), 1.0)
        self.assertTrue(done.is_set())
