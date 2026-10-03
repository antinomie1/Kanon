"""The plugin's namespace in the node's central key-value store.

Small state — counters, switches, tokens, per-user settings — belongs here: the node keeps it in
``data/kv.db``, so it survives restarts of the plugin and of the node, and nothing has to be set
up. Larger data, or data that needs queries, belongs in files or a database under
``context.data_dir``.

Values are stored as JSON (UTF-8), the convention every Kanon SDK follows, so any JSON value can be
stored and a value written by a plugin in another language reads back the same. A stored value
that is not JSON is reported, never guessed at.
"""

import json
import math
from typing import Any, List, Optional

from kanon_sdk.proto import pb, pb_grpc

#: Largest value the node accepts, in bytes of encoded JSON.
MAX_VALUE_BYTES = 1024 * 1024


def _finite_number(text: str) -> float:
    """Reject non-JSON constants and numbers that overflow the SDK's float representation."""
    number = float(text)
    if not math.isfinite(number):
        raise ValueError("JSON numbers must be finite")
    return number


class KV:
    """Reads and writes one plugin's keys in the node's KV store.

    Get one from :attr:`kanon_sdk.Plugin.kv` (or ``ctx.core.kv``)::

        visits = await self.kv.get(f"visits:{event.sender_id}", 0)
        await self.kv.set(f"visits:{event.sender_id}", visits + 1)
        await self.kv.set("login-token", token, ttl=3600)

    Every call is one RPC to the node; errors (an invalid key, a value over 1 MiB, the store being
    unavailable) raise ``grpc.aio.AioRpcError`` with the status the node chose.
    """

    def __init__(self, stub: pb_grpc.BotApiServiceStub, plugin_id: str) -> None:
        if not plugin_id:
            raise ValueError("the KV store needs the plugin id as its namespace")
        self._stub = stub
        self._plugin_id = plugin_id

    async def get(self, key: str, default: Any = None) -> Any:
        """Returns the value of ``key``, or ``default`` when it is missing or expired.

        Raises:
            ValueError: If the stored bytes are not JSON (written by something that did not
                follow the convention); the key is named so the culprit can be found.
        """
        response = await self._stub.GetStorage(
            pb.GetStorageRequest(plugin_id=self._plugin_id, key=key)
        )
        if not response.found:
            return default
        try:
            return json.loads(
                response.value.decode("utf-8"),
                parse_constant=_finite_number,
                parse_float=_finite_number,
            )
        except ValueError as exc:
            raise ValueError(f"KV value of {key!r} is not JSON: {exc}") from exc

    async def set(self, key: str, value: Any, *, ttl: Optional[int] = None) -> None:
        """Stores ``value`` (anything JSON can express) under ``key``.

        Args:
            key: 1–256 bytes.
            value: The value; tuples become lists and dict keys must be strings, as in JSON.
            ttl: Positive int64 seconds until the key expires; ``None`` keeps it until it is
                deleted. Setting a key again replaces both its value and its expiry.

        Raises:
            TypeError: If ``value`` cannot be encoded as JSON.
            ValueError: If ``ttl`` is outside the positive int64 range, a JSON number is not
                finite, or the encoded value exceeds :data:`MAX_VALUE_BYTES` (checked here so
                the mistake is reported before a round trip).
        """
        if ttl is not None and (
            isinstance(ttl, bool) or not isinstance(ttl, int) or not 1 <= ttl <= (1 << 63) - 1
        ):
            raise ValueError(f"ttl must be a positive int64 number of seconds, got {ttl!r}")
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
        if len(encoded) > MAX_VALUE_BYTES:
            raise ValueError(
                f"KV value of {key!r} is {len(encoded)} bytes; the limit is {MAX_VALUE_BYTES}"
            )
        await self._stub.SetStorage(
            pb.SetStorageRequest(
                plugin_id=self._plugin_id, key=key, value=encoded, ttl_seconds=ttl or 0
            )
        )

    async def delete(self, key: str) -> bool:
        """Removes ``key``; returns whether it existed (an expired key did not)."""
        response = await self._stub.DeleteStorage(
            pb.DeleteStorageRequest(plugin_id=self._plugin_id, key=key)
        )
        return response.deleted

    async def keys(self, prefix: str = "") -> List[str]:
        """Lists the live keys starting with ``prefix`` (taken literally), sorted."""
        response = await self._stub.ListStorage(
            pb.ListStorageRequest(plugin_id=self._plugin_id, prefix=prefix)
        )
        return list(response.keys)
