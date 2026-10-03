"""A Python host publishes readiness only after loading valid configuration and its plugin."""

import asyncio
import os
import sys
import tempfile
import unittest
from pathlib import Path

_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

from kanon_sdk import Plugin
from kanon_sdk.host import KanonHost
from kanon_sdk.ipc import connect_core_channel
from kanon_sdk.proto import pb, pb_grpc


class StartupPlugin(Plugin):
    """Exposes a controllable on_load boundary to the host's actual lifecycle."""

    id = "test.startup"

    def __init__(self):
        super().__init__()
        self.started = asyncio.Event()
        self.proceed = asyncio.Event()
        self.loaded = False
        self.unloaded = False

    async def on_load(self, ctx):
        """Waits for the test to permit initialization to finish."""
        self.started.set()
        await self.proceed.wait()
        self.loaded = True

    async def on_unload(self):
        """Records cleanup after either startup failure or normal shutdown."""
        self.unloaded = True


@unittest.skipIf(os.name == "nt", "uses Unix sockets")
class TestHostStartup(unittest.IsolatedAsyncioTestCase):
    """Exercises the host startup path rather than isolated service methods."""

    async def test_endpoint_is_not_published_until_on_load_finishes(self):
        """The supervisor cannot handshake or route requests into half-loaded state."""
        with tempfile.TemporaryDirectory(prefix="kanon-startup-") as directory:
            root = Path(directory)
            data = root / "data"
            data.mkdir()
            (data / "config.json").write_text('{"city":"Paris"}', encoding="utf-8")
            socket = root / "host.sock"
            plugin = StartupPlugin()
            stop = asyncio.Event()
            host = KanonHost(
                plugin, socket_path=socket, core_sock=root / "missing.sock", data_dir=data
            )
            task = asyncio.create_task(host.run_async(stop))
            channel = None
            try:
                await asyncio.wait_for(plugin.started.wait(), 3.0)
                self.assertEqual(plugin.context.config, {"city": "Paris"})
                self.assertFalse(socket.exists(), "on_load has not finished")
                plugin.proceed.set()
                channel = connect_core_channel(socket)
                await asyncio.wait_for(channel.channel_ready(), 5.0)
                response = await pb_grpc.PluginHostServiceStub(channel).GetPluginMeta(
                    pb.GetPluginMetaRequest(), timeout=2.0
                )
                self.assertEqual(response.plugins[0].id, plugin.id)
                self.assertTrue(plugin.loaded)
            finally:
                plugin.proceed.set()
                stop.set()
                await asyncio.wait_for(task, 5.0)
                if channel is not None:
                    await channel.close()
            self.assertTrue(plugin.unloaded)
            self.assertFalse(socket.exists())

    async def test_invalid_saved_configuration_fails_before_on_load(self):
        """Malformed JSON and non-object JSON must never become an empty/default config."""
        with tempfile.TemporaryDirectory(prefix="kanon-config-") as directory:
            root = Path(directory)
            data = root / "data"
            data.mkdir()
            socket = root / "host.sock"
            for saved in ["{", "null", "[]", "42", '"text"']:
                with self.subTest(saved=saved):
                    (data / "config.json").write_text(saved, encoding="utf-8")
                    plugin = StartupPlugin()
                    plugin.proceed.set()
                    stop = asyncio.Event()
                    stop.set()
                    host = KanonHost(
                        plugin, socket_path=socket, core_sock=root / "missing.sock", data_dir=data
                    )
                    with self.assertRaises(ValueError):
                        await host.run_async(stop)
                    self.assertFalse(plugin.started.is_set())
                    self.assertFalse(socket.exists())

    async def test_failed_on_load_never_publishes_and_releases_endpoint_ownership(self):
        """A failed startup must not prevent the corrected plugin from starting."""
        with tempfile.TemporaryDirectory(prefix="kanon-failed-load-") as directory:
            root = Path(directory)
            socket = root / "host.sock"
            plugin = StartupPlugin()

            async def fail_load(ctx):
                self.assertFalse(socket.exists())
                raise RuntimeError("load failed")

            plugin.on_load = fail_load
            host = KanonHost(
                plugin, socket_path=socket, core_sock=root / "missing.sock", data_dir=root / "data"
            )
            with self.assertRaisesRegex(RuntimeError, "load failed"):
                await host.run_async(asyncio.Event())
            self.assertTrue(plugin.unloaded)
            self.assertFalse(socket.exists())

            replacement = StartupPlugin()
            replacement.proceed.set()
            stop = asyncio.Event()
            stop.set()
            await KanonHost(
                replacement,
                socket_path=socket,
                core_sock=root / "missing.sock",
                data_dir=root / "data",
            ).run_async(stop)
            self.assertTrue(replacement.loaded)
