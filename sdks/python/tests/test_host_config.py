"""Configuration rejection preserves the accepted state and permits a corrected retry."""

import asyncio
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

from google.protobuf.json_format import ParseDict
from google.protobuf.struct_pb2 import Struct

from kanon_sdk.host import HostServiceImpl
from kanon_sdk.proto import pb


class RejectingPlugin:
    """Rejects invalid values while reading the same candidate cache as a real plugin."""

    def __init__(self):
        self.context = SimpleNamespace(config={"mode": "original"})
        self.calls = 0

    async def on_config_reload(self, config):
        """Accepts only the corrected configuration."""
        self.calls += 1
        assert self.context.config == config
        if config["mode"] == "invalid":
            raise ValueError("invalid mode")


class TestHostConfiguration(unittest.IsolatedAsyncioTestCase):
    """Exercises the actual host handler without a transport or background process."""

    async def test_rejected_version_can_be_corrected_and_retried(self):
        """Failed validation changes neither the accepted cache nor its version."""
        plugin = RejectingPlugin()
        service = HostServiceImpl(plugin)
        original = plugin.context.config

        async def reload(mode):
            return await service.ReloadPluginConfig(
                pb.ReloadPluginConfigRequest(
                    plugin_id="test",
                    version=1,
                    config=ParseDict({"mode": mode}, Struct()),
                ),
                None,
            )

        rejected = await reload("invalid")
        self.assertFalse(rejected.success)
        self.assertEqual(rejected.applied_version, 0)
        self.assertIs(plugin.context.config, original)

        accepted = await reload("corrected")
        self.assertTrue(accepted.success)
        self.assertEqual(accepted.applied_version, 1)
        self.assertEqual(plugin.context.config, {"mode": "corrected"})
        self.assertEqual(plugin.calls, 2)

        stale = await reload("invalid")
        self.assertFalse(stale.success)
        self.assertIn("Stale config version", stale.error_message)
        self.assertEqual(plugin.context.config, {"mode": "corrected"})
        self.assertEqual(plugin.calls, 2)

    async def test_cancelled_reload_restores_the_cache_without_advancing_the_version(self):
        """A core deadline may cancel the hook before it accepts the configuration."""
        plugin = RejectingPlugin()
        started = asyncio.Event()

        async def wait_for_cancellation(config):
            started.set()
            await asyncio.Future()

        plugin.on_config_reload = wait_for_cancellation
        service = HostServiceImpl(plugin)
        original = plugin.context.config
        request = pb.ReloadPluginConfigRequest(
            plugin_id="test",
            version=1,
            config=ParseDict({"mode": "candidate"}, Struct()),
        )
        pending = asyncio.create_task(service.ReloadPluginConfig(request, None))
        await started.wait()
        pending.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await pending
        self.assertIs(plugin.context.config, original)
        self.assertEqual(service._config_version, 0)
