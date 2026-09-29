"""Kanon Official Python Host Process.

Spawns a gRPC server hosting one or more Python plugins, serving
PluginHostService and MessagePipelineService over an IPC socket (Unix Domain Socket
or Windows Loopback TCP), communicating with the Kanon Core microkernel.
"""

import argparse
import asyncio
import importlib.util
import inspect
import os
import signal
import sys
from pathlib import Path
from typing import Any, Awaitable, Optional, Type

import grpc
from google.protobuf.json_format import MessageToDict

# Ensure sdks/python is on sys.path
_current_dir = Path(__file__).resolve().parent
_python_sdk_dir = _current_dir.parent
if str(_python_sdk_dir) not in sys.path:
    sys.path.insert(0, str(_python_sdk_dir))

from kanon_sdk.context import CoreHandle, PluginContext
from kanon_sdk.ipc import CoreWatchdog, connect_core_channel
from kanon_sdk.plugin import Plugin
from kanon_sdk.proto import pb, pb_grpc

# Deadline for one shutdown step. Long enough for a healthy teardown (a WebSocket close, a gRPC
# drain), short enough that a hung plugin cannot keep this host alive as a ghost.
SHUTDOWN_STEP_TIMEOUT = 5.0


class HostServiceImpl(pb_grpc.PluginHostServiceServicer):
    """Implementation of PluginHostService for host lifecycle management."""

    def __init__(self, plugin: Plugin):
        self.plugin = plugin

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
        current_version = getattr(self, "_config_version", 0)
        if request.version > 0 and request.version <= current_version:
            return pb.ReloadPluginConfigResponse(
                success=False,
                error_message=f"Stale config version {request.version}: current is {current_version}",
                applied_version=current_version,
            )
        self._config_version = request.version
        if request.HasField("config"):
            from google.protobuf.json_format import MessageToDict
            new_config = MessageToDict(request.config)
            if self.plugin.context is not None:
                self.plugin.context.config = new_config
            try:
                await self.plugin.on_config_reload(new_config)
            except Exception as e:
                return pb.ReloadPluginConfigResponse(
                    success=False,
                    error_message=f"on_config_reload hook failed: {e}",
                    applied_version=current_version,
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

    async def OnDeliverMessage(
        self,
        request: pb.DeliverMessageRequest,
        context: grpc.aio.ServicerContext,
    ) -> pb.DeliverMessageResponse:
        return await self.plugin.on_deliver_message(request)


def load_plugin_from_path(target_path: Path) -> Plugin:
    """Loads a Plugin subclass from a Python script or plugin.toml directory."""
    script_path = target_path
    if target_path.is_file() and target_path.name == "plugin.toml":
        import tomllib
        with open(target_path, "rb") as f:
            data = tomllib.load(f)
        entrypoint = data.get("plugin", {}).get("entrypoint", "main.py")
        script_path = target_path.parent / entrypoint
    elif target_path.is_dir():
        manifest = target_path / "plugin.toml"
        if manifest.exists():
            import tomllib
            with open(manifest, "rb") as f:
                data = tomllib.load(f)
            entrypoint = data.get("plugin", {}).get("entrypoint", "main.py")
            script_path = target_path / entrypoint
        else:
            script_path = target_path / "main.py"

    if not script_path.exists():
        raise FileNotFoundError(f"Plugin entrypoint script not found: {script_path}")

    # Add plugin directory to sys.path so it can import sibling modules
    plugin_dir = str(script_path.parent)
    if plugin_dir not in sys.path:
        sys.path.insert(0, plugin_dir)

    module_name = f"kanon_plugin_{script_path.stem}"
    spec = importlib.util.spec_from_file_location(module_name, script_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Cannot load module spec from {script_path}")

    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)

    # Search module for Plugin instance or subclass
    for _, attr in inspect.getmembers(module):
        if inspect.isclass(attr) and issubclass(attr, Plugin) and attr is not Plugin:
            return attr()
        if isinstance(attr, Plugin):
            return attr

    raise ValueError(f"No Plugin class or instance found in {script_path}")


async def main() -> None:
    parser = argparse.ArgumentParser(description="Kanon Python Plugin Host Runner")
    parser.add_argument("--plugin", type=str, default=None, help="Path to plugin manifest or script")
    parser.add_argument("--socket", type=str, default=None, help="Path to host IPC socket")
    args = parser.parse_args()

    socket_path_str = (
        args.socket
        or os.environ.get("KANON_HOST_SOCK")
        or "./run/host_py.sock"
    )
    socket_path = Path(socket_path_str).resolve()
    core_sock_str = os.environ.get("KANON_CORE_SOCK")
    host_id = os.environ.get("KANON_HOST_ID", "host_py")

    plugin_target = args.plugin

    if not plugin_target:
        print("Error: No plugin specified via --plugin", file=sys.stderr)
        sys.exit(1)

    plugin = load_plugin_from_path(Path(plugin_target).resolve())
    meta = plugin.meta()

    data_dir = Path(f"./data/plugins/{meta.id}").resolve()
    data_dir.mkdir(parents=True, exist_ok=True)

    # Clean up stale socket file if it exists
    if socket_path.exists():
        try:
            socket_path.unlink()
        except OSError:
            pass

    # Start the host gRPC server first so that Core can immediately dial the advertised
    # endpoint during RegisterHost (or during supervisor's wait_for_readiness).
    server = grpc.aio.server()
    pb_grpc.add_PluginHostServiceServicer_to_server(HostServiceImpl(plugin), server)
    pb_grpc.add_MessagePipelineServiceServicer_to_server(PipelineServiceImpl(plugin), server)

    server.add_insecure_port(f"unix:{socket_path}")
    await server.start()
    print(f"Kanon Python Host running on {socket_path}", flush=True)

    # Reach Core before building the PluginContext so adapter plugins can capture ctx.core in on_load.
    core_channel: Optional[grpc.aio.Channel] = None
    core_handle: Optional[CoreHandle] = None
    core_stub: Optional[pb_grpc.BotApiServiceStub] = None
    watchdog: Optional[CoreWatchdog] = None

    if core_sock_str:
        core_sock = Path(core_sock_str).resolve()
        if core_sock.exists():
            try:
                # One channel and one stub for the whole process lifetime; adapter
                # tasks share them through CoreHandle's internal lock. Dialing goes
                # through the SDK helper, which pins the HTTP/2 authority that
                # Tonic's h2 server requires (see kanon_sdk.ipc).
                core_channel = connect_core_channel(core_sock)
                core_stub = pb_grpc.BotApiServiceStub(core_channel)

                # Registration doubles as the reachability probe: only a Core that
                # answered this RPC earns a handle.
                reg_req = pb.RegisterHostRequest(
                    host_id=host_id,
                    runtime="python",
                    endpoint=str(socket_path),
                    loaded_plugin_ids=[meta.id],
                )
                await core_stub.RegisterHost(reg_req, timeout=3.0)
                core_handle = CoreHandle(core_stub)
            except Exception as exc:
                # No Core peer: drop the half-open channel so no descriptor or
                # connectivity task leaks, and continue in standalone mode.
                if core_channel is not None:
                    try:
                        await core_channel.close()
                    except Exception:
                        pass
                core_channel = None
                print(
                    f"[kanon-host] Core at {core_sock} unreachable ({exc}); "
                    "running in standalone mode with ctx.core = None",
                    file=sys.stderr,
                    flush=True,
                )
        else:
            print(
                f"[kanon-host] KANON_CORE_SOCK points to missing socket {core_sock}; "
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
    config_path = data_dir / "config.json"
    if config_path.exists():
        try:
            import json
            config = json.loads(config_path.read_text(encoding="utf-8"))
        except Exception as e:
            print(f"[kanon-host] Failed to load config from {config_path}: {e}", file=sys.stderr, flush=True)

    ctx = PluginContext(data_dir=data_dir, config=config, core=core_handle)
    await plugin.on_load(ctx)

    stop_event = asyncio.Event()

    def handle_signal():
        stop_event.set()

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, handle_signal)
        except (NotImplementedError, RuntimeError):
            pass

    # A host whose core is gone must stop instead of becoming a ghost bot that keeps serving its
    # platform (and double-handling messages once a new core starts).
    if core_stub is not None:
        def _core_lost(reason: str) -> None:
            print(f"[kanon-host] {reason}", flush=True)
            stop_event.set()

        watchdog = CoreWatchdog(core_stub, pb, on_lost=_core_lost)
        watchdog.start()

    await stop_event.wait()

    if watchdog is not None:
        await watchdog.stop()

    # Graceful shutdown, every step bounded.
    #
    # A plugin's own teardown can block indefinitely: closing a platform WebSocket may wait for a
    # close handshake that never arrives. That is precisely the situation this host must survive —
    # the core is already gone, and a host stuck in teardown keeps its platform connection open,
    # so the next core double-serves every message. Each step therefore has a deadline and the
    # process exits regardless of whether it completed.
    await _bounded("plugin teardown", plugin.on_unload(), SHUTDOWN_STEP_TIMEOUT)
    await _bounded("gRPC server stop", server.stop(grace=1.0), SHUTDOWN_STEP_TIMEOUT)
    if core_channel is not None:
        # Closed last, after on_unload() has had the chance to stop adapter tasks
        # that may still hold the shared handle for an in-flight ingest call.
        await _bounded("core channel close", core_channel.close(), SHUTDOWN_STEP_TIMEOUT)
    if socket_path.exists():
        try:
            socket_path.unlink()
        except OSError:
            pass

    print("[kanon-host] shutdown complete", flush=True)


async def _bounded(label: str, awaitable: Awaitable[Any], timeout: float) -> None:
    """Awaits one shutdown step, never longer than ``timeout`` seconds.

    Cancelling on timeout is deliberate: a half-closed platform connection is already gone from
    the core's point of view, and letting the step run forever would leave a ghost host behind.
    """
    try:
        await asyncio.wait_for(awaitable, timeout=timeout)
    except asyncio.TimeoutError:
        print(
            f"[kanon-host] {label} did not finish within {timeout}s; exiting anyway",
            flush=True,
        )
    except Exception as exc:  # noqa: BLE001 - shutdown continues even if a step fails
        print(f"[kanon-host] {label} failed: {exc}", flush=True)


if __name__ == "__main__":
    asyncio.run(main())
