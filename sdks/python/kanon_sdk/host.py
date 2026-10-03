"""Out-of-process gRPC plugin host for Kanon Python SDK."""

import asyncio
import os
import signal
import sys
from pathlib import Path
from typing import Any, Awaitable, Optional, Union

import grpc
from google.protobuf.json_format import MessageToDict

from kanon_sdk.context import CoreHandle, PluginContext
from kanon_sdk.ipc import CoreWatchdog, connect_core_channel, EndpointOwner, ServerAuth, ipc_token
from kanon_sdk.plugin import Plugin
from kanon_sdk.proto import pb, pb_grpc

# Deadline for one shutdown step. Long enough for a healthy teardown (a WebSocket close, a gRPC
# drain), short enough that a hung plugin cannot keep this host alive as a ghost.
SHUTDOWN_STEP_TIMEOUT = 5.0


class HostServiceImpl(pb_grpc.PluginHostServiceServicer):
    """Implementation of PluginHostService for host lifecycle management."""

    def __init__(self, plugin: Plugin):
        self.plugin = plugin
        self._config_version = 0

    async def Ping(
        self,
        request: pb.PingRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.PingResponse:
        return pb.PingResponse(timestamp=request.timestamp)

    async def ReloadPluginConfig(
        self,
        request: pb.ReloadPluginConfigRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.ReloadPluginConfigResponse:
        if request.version > 0 and request.version <= self._config_version:
            return pb.ReloadPluginConfigResponse(
                success=False,
                error_message=f"Stale config version {request.version}: current is {self._config_version}",
                applied_version=self._config_version,
            )
        self._config_version = request.version
        if request.HasField("config"):
            new_config = MessageToDict(request.config)
            if self.plugin.context is not None:
                self.plugin.context.config = new_config
            try:
                await self.plugin.on_config_reload(new_config)
            except Exception as exc:
                return pb.ReloadPluginConfigResponse(
                    success=False,
                    error_message=f"on_config_reload failed: {exc}",
                    applied_version=self._config_version,
                )
        return pb.ReloadPluginConfigResponse(
            success=True,
            error_message="",
            applied_version=request.version,
        )

    async def InvokeAction(
        self,
        request: pb.PluginActionRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.PluginActionResponse:
        """Serves a control-plane management action (never reachable from the LLM)."""
        return await self.plugin.on_invoke_action(
            request.plugin_id,
            request.action,
            MessageToDict(request.parameters) if request.HasField("parameters") else {},
        )

    async def GetPluginMeta(
        self,
        request: pb.GetPluginMetaRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.GetPluginMetaResponse:
        meta = self.plugin.meta()
        return pb.GetPluginMetaResponse(plugins=[meta])


class PipelineServiceImpl(pb_grpc.MessagePipelineServiceServicer):
    """Implementation of MessagePipelineService for dispatching pipeline events."""

    def __init__(self, plugin: Plugin):
        self.plugin = plugin

    async def OnPreFilter(
        self,
        request: pb.PipelineEventRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.PreFilterResult:
        result = await self.plugin.on_pre_filter(request)
        if result is None:
            return pb.PreFilterResult(
                action=pb.PreFilterResult.Action.PASS,
                modified_text="",
                reply_messages=[],
            )
        return result

    async def OnExecuteCommand(
        self,
        request: pb.CommandExecuteRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.CommandExecuteResponse:
        return await self.plugin.on_execute_command(request)

    async def OnCallTool(
        self,
        request: pb.ToolCallRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.ToolCallResponse:
        return await self.plugin.on_call_tool(request)

    async def OnEvent(
        self,
        request: pb.EventNotification,
        context: grpc.aio.ServicerContext,
    ) -> pb.EventAck:
        await self.plugin.on_event(request)
        return pb.EventAck(received=True)

    async def OnDecorateReply(
        self,
        request: pb.DecorateReplyRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.DecorateReplyResult:
        # A failing decorator surfaces as an RPC error; Core then keeps the reply unchanged.
        return await self.plugin.on_decorate_reply(request)

    async def OnPrepareTurn(
        self,
        request: pb.PrepareTurnRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.PrepareTurnResult:
        # A failing preparer surfaces as an RPC error; Core then answers without its context.
        return await self.plugin.on_prepare_turn(request)

    async def OnLlmRequest(
        self,
        request: pb.LlmRequestHookRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.LlmRequestHookResult:
        # A failing rewriter surfaces as an RPC error; Core then keeps the system prompt.
        return await self.plugin.on_llm_request(request)

    async def OnHttpRequest(
        self,
        request: pb.HttpRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.HttpResponse:
        return await self.plugin.on_http_request(request)

    async def OnDeliverMessage(
        self,
        request: pb.DeliverMessageRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.DeliverMessageResponse:
        return await self.plugin.on_deliver_message(request)


class KanonHost:
    """Out-of-process gRPC host for running a Kanon Python plugin."""

    def __init__(
        self,
        plugin: Plugin,
        socket_path: Optional[Union[str, Path]] = None,
        core_sock: Optional[Union[str, Path]] = None,
        host_id: Optional[str] = None,
        data_dir: Optional[Union[str, Path]] = None,
    ):
        self.plugin = plugin
        self.socket_path = (
            Path(socket_path).resolve()
            if socket_path
            else Path(os.environ.get("KANON_HOST_SOCK", "./run/host_py.sock")).resolve()
        )
        self.core_sock = (
            Path(core_sock).resolve()
            if core_sock
            else (
                Path(os.environ["KANON_CORE_SOCK"]).resolve()
                if os.environ.get("KANON_CORE_SOCK")
                else (Path("./run/core.sock").resolve() if Path("./run/core.sock").exists() else None)
            )
        )
        self.host_id = host_id or os.environ.get("KANON_HOST_ID", f"host_{plugin.id.replace('.', '_')}")
        self.data_dir = (
            Path(data_dir).resolve()
            if data_dir
            else Path(f"./data/plugins/{plugin.id}").resolve()
        )

    def run(self) -> None:
        """Runs the plugin host synchronously until interrupted."""
        asyncio.run(self.run_async())

    async def run_async(self, shutdown_event: Optional[asyncio.Event] = None) -> None:
        """Runs the plugin host asynchronously until shutdown is requested."""
        meta = self.plugin.meta()
        self.data_dir.mkdir(parents=True, exist_ok=True)

        owner = EndpointOwner(self.socket_path)

        server: Optional[grpc.aio.Server] = None
        core_channel: Optional[grpc.aio.Channel] = None
        watchdog: Optional[CoreWatchdog] = None
        plugin_loaded = False
        try:
            # Start host gRPC server first so that Core can reach it during RegisterHost
            server = grpc.aio.server(interceptors=[ServerAuth(ipc_token())] if os.name == "nt" or os.environ.get("KANON_IPC_TOKEN") else [])
            pb_grpc.add_PluginHostServiceServicer_to_server(HostServiceImpl(self.plugin), server)
            pb_grpc.add_MessagePipelineServiceServicer_to_server(PipelineServiceImpl(self.plugin), server)

            port = server.add_insecure_port("127.0.0.1:0" if os.name == "nt" else f"unix:{self.socket_path}")
            if not port:
                raise RuntimeError("Failed to bind host IPC endpoint")
            owner.publish(port)
            await server.start()
            print(f"Kanon Python Host running on {self.socket_path}", flush=True)

            core_stub: Optional[pb_grpc.BotApiServiceStub] = None
            core_handle: Optional[CoreHandle] = None

            if self.core_sock:
                if self.core_sock.exists():
                    try:
                        # Dial through the shared helper: it pins the valid HTTP/2 authority that
                        # Tonic's h2 server requires (see kanon_sdk.ipc for the full rationale).
                        core_channel = connect_core_channel(self.core_sock)
                        core_stub = pb_grpc.BotApiServiceStub(core_channel)
                        reg_req = pb.RegisterHostRequest(
                            host_id=self.host_id,
                            runtime="python",
                            endpoint=str(self.socket_path),
                            loaded_plugin_ids=[meta.id],
                        )
                        await core_stub.RegisterHost(reg_req, timeout=3.0)
                        core_handle = CoreHandle(core_stub, plugin_id=meta.id, host_id=self.host_id)
                    except Exception as exc:
                        if core_channel is not None:
                            try:
                                await core_channel.close()
                            except Exception:
                                pass
                        core_channel = None
                        print(
                            f"[kanon-host] Core at {self.core_sock} unreachable ({exc}); "
                            "running in standalone mode with ctx.core = None",
                            file=sys.stderr,
                            flush=True,
                        )
                else:
                    print(
                        f"[kanon-host] KANON_CORE_SOCK points to missing socket {self.core_sock}; "
                        "running in standalone mode with ctx.core = None",
                        file=sys.stderr,
                        flush=True,
                    )
            else:
                print(
                    "[kanon-host] KANON_CORE_SOCK is not set; "
                    "running in standalone mode with ctx.core = None",
                    file=sys.stderr,
                    flush=True,
                )

            config: dict = {}
            config_path = self.data_dir / "config.json"
            if config_path.exists():
                try:
                    import json
                    config = json.loads(config_path.read_text(encoding="utf-8"))
                except Exception as e:
                    print(f"[kanon-host] Failed to load config from {config_path}: {e}", file=sys.stderr, flush=True)

            ctx = PluginContext(data_dir=self.data_dir, config=config, core=core_handle)
            # Set before on_load: plugins commonly override on_load without calling super(), and the
            # SDK's event objects reach Core through plugin.context.
            self.plugin.context = ctx
            plugin_loaded = True
            await self.plugin.on_load(ctx)

            if shutdown_event is None:
                stop_event = asyncio.Event()

                def handle_signal():
                    stop_event.set()

                loop = asyncio.get_running_loop()
                for sig in (signal.SIGINT, signal.SIGTERM):
                    try:
                        loop.add_signal_handler(sig, handle_signal)
                    except (NotImplementedError, RuntimeError):
                        pass
            else:
                # The caller owns the stop event; a lost core must be able to trigger it too.
                stop_event = shutdown_event

            # A host whose core is gone must not keep serving its platform: otherwise it becomes a
            # ghost bot that double-handles messages once a new core starts. See CoreWatchdog.
            if core_stub is not None:
                def _core_lost(reason: str) -> None:
                    print(f"[kanon-host] {reason}", flush=True)
                    stop_event.set()

                watchdog = CoreWatchdog(core_stub, pb, on_lost=_core_lost)
                watchdog.start()

            await stop_event.wait()

        finally:
            # Keep endpoint ownership until the server has stopped, including startup errors
            # and cancellation. Otherwise another host could publish over a live listener.
            try:
                if watchdog is not None:
                    await watchdog.stop()
                if plugin_loaded:
                    await _bounded("plugin teardown", self.plugin.on_unload(), SHUTDOWN_STEP_TIMEOUT)
                if server is not None:
                    await _bounded("gRPC server stop", server.stop(grace=1.0), SHUTDOWN_STEP_TIMEOUT)
                if core_channel is not None:
                    await _bounded("core channel close", core_channel.close(), SHUTDOWN_STEP_TIMEOUT)
            finally:
                owner.close()
        print("[kanon-host] shutdown complete", flush=True)


async def _bounded(label: str, awaitable: Awaitable[Any], timeout: float) -> None:
    """Awaits one shutdown step, never longer than ``timeout`` seconds.

    Cancelling on timeout is deliberate: a half-closed platform connection is already gone from
    the core's point of view, and letting the step run forever would leave a ghost host behind.
    """
    try:
        await asyncio.wait_for(awaitable, timeout=timeout)
    except asyncio.TimeoutError:
        print(f"[kanon-host] {label} did not finish within {timeout}s; exiting anyway", flush=True)
    except Exception as exc:  # noqa: BLE001 - shutdown continues even if a step fails
        print(f"[kanon-host] {label} failed: {exc}", flush=True)
