"""Actual loopback calls verify authentication for unary and streaming core RPCs."""
import asyncio
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import grpc
from kanon_sdk.ipc import ServerAuth, _UnaryAuth, _StreamAuth, loopback_endpoint, EndpointOwner


class IpcAuthentication(unittest.IsolatedAsyncioTestCase):
    async def test_initial_metadata_authenticates_unary_and_streaming_calls(self):
        token = "ab" * 32
        server = grpc.aio.server(interceptors=[ServerAuth(token)])

        async def unary(request, context):
            return b"ok"

        async def stream(request, context):
            yield b"chunk"

        server.add_generic_rpc_handlers((grpc.method_handlers_generic_handler("test", {
            "Unary": grpc.unary_unary_rpc_method_handler(unary),
            "Stream": grpc.unary_stream_rpc_method_handler(stream),
        }),))
        port = server.add_insecure_port("127.0.0.1:0")
        await server.start()
        try:
            async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
                with self.assertRaises(grpc.aio.AioRpcError) as error:
                    await channel.unary_unary("/test/Unary")(b"")
                self.assertEqual(error.exception.code(), grpc.StatusCode.UNAUTHENTICATED)
            with patch.dict("os.environ", {"KANON_IPC_TOKEN": token}):
                async with grpc.aio.insecure_channel(f"127.0.0.1:{port}", interceptors=[_UnaryAuth(), _StreamAuth()]) as channel:
                    self.assertEqual(await channel.unary_unary("/test/Unary")(b""), b"ok")
                    self.assertEqual([part async for part in channel.unary_stream("/test/Stream")(b"")], [b"chunk"])
            with patch.dict("os.environ", {"KANON_IPC_TOKEN": "cd" * 32}):
                async with grpc.aio.insecure_channel(f"127.0.0.1:{port}", interceptors=[_UnaryAuth()]) as channel:
                    with self.assertRaises(grpc.aio.AioRpcError) as error:
                        await channel.unary_unary("/test/Unary")(b"")
                    self.assertEqual(error.exception.code(), grpc.StatusCode.UNAUTHENTICATED)
        finally:
            await server.stop(0)

    def test_endpoint_rejects_remote_address(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "core.sock"
            path.write_text("192.0.2.1:9000")
            with self.assertRaises(ValueError):
                loopback_endpoint(path)
            path.write_text("127.0.0.1:9000")
            self.assertEqual(loopback_endpoint(path), "127.0.0.1:9000")

    async def test_failed_startup_stops_server_and_releases_endpoint(self):
        from kanon_sdk.host import KanonHost
        from kanon_sdk.plugin import Plugin

        class BrokenPlugin(Plugin):
            """Allocates a host server but fails before plugin initialization completes."""
            id = "org.kanon.test.broken"
            name = "Broken"
            version = "0.1.0"

            async def on_load(self, context):
                raise RuntimeError("startup failed")

        with tempfile.TemporaryDirectory() as directory:
            endpoint = Path(directory) / "host.sock"
            host = KanonHost(BrokenPlugin(), socket_path=endpoint, data_dir=Path(directory) / "data")
            with self.assertRaisesRegex(RuntimeError, "startup failed"):
                await host.run_async()
            self.assertFalse(endpoint.exists())
            # A second owner proves the process lock was released on the exception path.
            owner = EndpointOwner(endpoint)
            owner.close()
