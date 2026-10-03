"""Core IPC channel construction for the Kanon Python SDK.

Why this module exists
----------------------
Every Python participant in the Kanon topology (plugin host, platform adapter, management
tooling) speaks gRPC to exactly one endpoint: the Core microkernel's Unix domain socket.
Dialing it is *not* a plain ``insecure_channel`` call, because gRPC's C-core derives the
HTTP/2 ``:authority`` pseudo-header from the channel target: for a target written as
``unix:/abs/path/core.sock`` it sends the **percent-encoded socket path** as the authority,
e.g. ``run%2Fuser%2F1000%2Fkanon%2Frun%2Fcore.sock``.

That value is not a legal HTTP/2 authority. Percent escapes are only tolerated inside a
userinfo component or an IPv6 zone identifier, so Rust's ``h2`` server -- the HTTP/2
implementation behind Tonic, which serves ``core.sock`` -- rejects the request headers and
resets the stream before any gRPC status can be produced::

    DEBUG h2::server: malformed headers: malformed authority
          (b"run%2Fuser%2F1000%2Fkanon%2Frun%2Fcore.sock"): invalid authority

The caller only observes the resulting opaque transport failure, which names neither the
authority nor the offending field and therefore looks like a Core crash::

    StatusCode.INTERNAL
    "Stream removed (RST_STREAM (Received RST_STREAM with error code 1))"

*Every* ``BotApiService`` RPC fails this way (``RegisterHost``, ``IngestEvent``,
``SendMessage``, ...): the plugin host silently degrades to standalone mode and the adapter
drops inbound messages, even though Core is perfectly healthy.

The fix is to pin an explicit, valid default authority on the channel, which is exactly what
this module exists to guarantee in one place. Note that rewriting the target as
``unix:///abs/path/core.sock`` does *not* help: C-core still percent-encodes the path into
the authority. Other SDKs are unaffected because they never expose the socket path as an
authority -- gRPC-js hard-codes ``localhost`` for its ``unix:`` resolver and the Rust SDK
dials through Tonic's ``http://localhost`` endpoint.
"""

from __future__ import annotations

import asyncio
import os
import time
from pathlib import Path
from typing import Callable, Optional, Union

import grpc

#: Fallback endpoint used when ``KANON_CORE_SOCK`` is not injected by the Supervisor.
#: The Supervisor always injects the real path, so this only serves manual/standalone runs.
DEFAULT_CORE_SOCK = "./run/core.sock"

#: Default HTTP/2 authority handed to Core for every RPC.
#:
#: Any syntactically valid authority works (Core never reads it: the peer identity is the
#: Unix socket credential, and Windows loopback authentication uses the ``x-kanon-auth-token``
#: metadata header instead). ``localhost`` is chosen to match what gRPC-js and the Rust SDK
#: already send, so all three runtimes present the same request shape to the same endpoint.
CORE_AUTHORITY = "localhost"


def connect_core_channel(core_sock: Union[str, Path]) -> grpc.aio.Channel:
    """Creates the long-lived gRPC channel to Kanon Core over its Unix domain socket.

    The returned channel is lazy: it dials on the first RPC and reconnects on its own after a
    Core restart, so callers own it for the whole process lifetime and must close it once.

    Args:
        core_sock: Path of the Core endpoint (``core.sock``), typically taken from
            ``KANON_CORE_SOCK``.

    Returns:
        A channel whose requests carry a valid HTTP/2 authority. See the module docstring for
        why the authority must never be left to C-core's target-derived default.
    """
    # ``grpc.default_authority`` is the only knob that suppresses C-core's percent-encoded
    # authority: it overrides the pseudo-header verbatim for every RPC on the channel.
    return grpc.aio.insecure_channel(
        loopback_endpoint(core_sock) if os.name == "nt" else f"unix:{core_sock}",
        interceptors=[_UnaryAuth(), _StreamAuth()] if os.name == "nt" or os.environ.get("KANON_IPC_TOKEN") else [],
        options=[("grpc.default_authority", CORE_AUTHORITY)],
    )


#: Interval between core liveness probes, in seconds.
CORE_WATCHDOG_INTERVAL = 15.0

#: Per-probe deadline, in seconds.
CORE_WATCHDOG_TIMEOUT = 5.0

#: Consecutive failed probes tolerated before the host stops itself.
CORE_WATCHDOG_FAILURES = 3


class CoreWatchdog:
    """Stops a plugin host once the core it registered with is gone.

    Why this exists
    ---------------
    A host is a child of the core, but ``SIGKILL`` and manual cleanups do not always reap it: the
    process survives, keeps its platform connection (a QQ gateway socket, a Telegram poller) and
    keeps pushing events into a core that no longer exists. When a *new* core starts, the orphan
    and the fresh host both serve the same platform, so every inbound message is handled twice —
    two replies, two model calls, two bills — while nothing looks broken in the console.

    The watchdog probes ``BotApiService.Ping`` on the core. Any answer proves the core is alive;
    when the probe fails :data:`CORE_WATCHDOG_FAILURES` times in a row the host is asked to stop,
    which unloads the plugin and closes its platform connection.
    """

    def __init__(
        self,
        stub: object,
        pb: object,
        *,
        interval: float = CORE_WATCHDOG_INTERVAL,
        timeout: float = CORE_WATCHDOG_TIMEOUT,
        failures: int = CORE_WATCHDOG_FAILURES,
        on_lost: Optional[Callable[[str], None]] = None,
    ) -> None:
        self._stub = stub
        self._pb = pb
        self._interval = interval
        self._timeout = timeout
        self._failures = failures
        self._on_lost = on_lost
        self._task: Optional[asyncio.Task] = None

    def start(self) -> asyncio.Task:
        """Starts the background probe loop."""
        self._task = asyncio.create_task(self._run())
        return self._task

    async def stop(self) -> None:
        """Stops the probe loop, if running."""
        if self._task is None:
            return
        self._task.cancel()
        try:
            await self._task
        except (asyncio.CancelledError, Exception):
            pass
        self._task = None

    async def _run(self) -> None:
        consecutive = 0
        while True:
            await asyncio.sleep(self._interval)
            try:
                request = self._pb.PingRequest(timestamp=int(time.time() * 1000))
                await self._stub.Ping(request, timeout=self._timeout)
                consecutive = 0
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 - any failure means "core not answering"
                consecutive += 1
                print(
                    f"[kanon-host] core liveness probe failed ({consecutive}/{self._failures}): {exc}",
                    flush=True,
                )
                if consecutive >= self._failures:
                    reason = (
                        f"core unreachable for {consecutive} consecutive probes; "
                        "stopping this host so it cannot serve the platform without a core"
                    )
                    print(f"[kanon-host] {reason}", flush=True)
                    if self._on_lost is not None:
                        self._on_lost(reason)
                    return


def ipc_token() -> str:
    """Reads the Windows host launch credential, rejecting missing or malformed secrets."""
    token = os.environ.get("KANON_IPC_TOKEN", "")
    if os.name == "nt" and (len(token) != 64 or any(c not in "0123456789abcdef" for c in token)):
        raise ValueError("Windows IPC requires KANON_IPC_TOKEN with 32 random bytes")
    return token


def loopback_endpoint(path: Union[str, Path]) -> str:
    """Resolves an address file without allowing remote hosts or DNS resolution."""
    import ipaddress
    address = Path(path).read_text().strip()
    host, port = address.rsplit(":", 1)
    if not ipaddress.ip_address(host.strip("[]")).is_loopback or not 0 < int(port) < 65536:
        raise ValueError("IPC endpoint must use a nonzero loopback port")
    return address


def _authenticated_details(details: grpc.aio.ClientCallDetails) -> grpc.aio.ClientCallDetails:
    return grpc.aio.ClientCallDetails(
        details.method, details.timeout,
        tuple(details.metadata or ()) + (("x-kanon-auth-token", ipc_token()),),
        details.credentials, details.wait_for_ready,
    )


class _UnaryAuth(grpc.aio.UnaryUnaryClientInterceptor):
    async def intercept_unary_unary(self, continuation, details, request):
        return await continuation(_authenticated_details(details), request)


class _StreamAuth(grpc.aio.UnaryStreamClientInterceptor):
    async def intercept_unary_stream(self, continuation, details, request):
        return await continuation(_authenticated_details(details), request)


class ServerAuth(grpc.aio.ServerInterceptor):
    """Checks every host RPC credential before invoking plugin code."""

    def __init__(self, token: str):
        self.token = token

    async def intercept_service(self, continuation, details):
        import hmac
        supplied = dict(details.invocation_metadata).get("x-kanon-auth-token", "")
        if len(self.token) == 64 and hmac.compare_digest(supplied, self.token):
            return await continuation(details)

        async def reject(request, context):
            await context.abort(grpc.StatusCode.UNAUTHENTICATED, "Invalid or missing IPC token")

        return grpc.unary_unary_rpc_method_handler(reject)


class EndpointOwner:
    """Keeps a stable file lock and removes only the endpoint created by this host."""

    def __init__(self, path: Path):
        import socket
        import stat
        self.path = path
        self.identity = None
        path.parent.mkdir(parents=True, exist_ok=True)
        self.lock = open(str(path) + ".lock", "a+b")
        try:
            if os.name == "nt":
                import msvcrt
                self.lock.seek(0)
                msvcrt.locking(self.lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                previous = path.lstat()
            except FileNotFoundError:
                return
            if stat.S_ISLNK(previous.st_mode) or (os.name != "nt" and not stat.S_ISSOCK(previous.st_mode)):
                raise FileExistsError(f"IPC endpoint is not a socket: {path}")
            probe = socket.socket(socket.AF_INET if os.name == "nt" else socket.AF_UNIX)
            probe.settimeout(0.1)
            try:
                target = loopback_endpoint(path).rsplit(":", 1) if os.name == "nt" else str(path)
                probe.connect((target[0], int(target[1])) if os.name == "nt" else target)
            except ConnectionRefusedError:
                current = path.lstat()
                if (current.st_dev, current.st_ino) != (previous.st_dev, previous.st_ino):
                    raise FileExistsError("IPC endpoint changed during stale check")
                path.unlink()
            else:
                raise FileExistsError(f"IPC endpoint is active: {path}")
            finally:
                probe.close()
        except BaseException:
            self.lock.close()
            raise

    def publish(self, port: int) -> None:
        """Records ownership only after bind; Windows writes an exclusive endpoint file."""
        if os.name == "nt":
            with self.path.open("x") as endpoint:
                endpoint.write(f"127.0.0.1:{port}")
        self.identity = self.path.lstat()

    def close(self) -> None:
        """Releases the owned endpoint while still holding its process lock."""
        try:
            current = self.path.lstat()
            if self.identity is not None and (current.st_dev, current.st_ino) == (self.identity.st_dev, self.identity.st_ino):
                self.path.unlink()
        except FileNotFoundError:
            pass
        finally:
            self.lock.close()
