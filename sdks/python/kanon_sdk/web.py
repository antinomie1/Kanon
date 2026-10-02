"""HTTP routes a plugin serves through the node's management gateway.

The gateway forwards every request under ``/api/v1/plugins/<plugin id>/http/`` to the plugin
(``OnHttpRequest``); the SDK routes it to the method declared for its path::

    @http_route("/stats")
    async def stats(self, request: HttpRequest) -> dict:
        return {"visits": await self.kv.get("visits", 0)}

    @http_route("/webhook", methods=("POST",))
    async def webhook(self, request: HttpRequest) -> HttpResponse:
        if not valid_signature(request.headers.get("x-signature"), request.body):
            return HttpResponse(status=401)
        ...
        return HttpResponse(status=204)

A plugin page (``pages/index.html``) reaches these routes with relative URLs, e.g.
``fetch('../http/stats')``.

The gateway authenticates nothing: whoever can reach the node's console port can call these
routes, so a route that changes anything must check its caller (a webhook signature, a token
from the plugin's config). Bodies are limited to 3 MiB and the plugin must answer within 30 s.
"""

import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Sequence
from urllib.parse import parse_qs

from kanon_sdk.proto import pb

#: Methods a route may declare.
HTTP_METHODS = {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"}


@dataclass
class HttpRequest:
    """A request forwarded by the gateway.

    Attributes:
        method: Upper-case method, e.g. ``"GET"``.
        path: Path below the plugin's ``http/`` root, starting with ``/``.
        query: Query parameters; each name maps to all of its values, in order.
        headers: Request headers with lower-case names; repeated headers are joined with ``", "``.
        body: The raw body.
    """

    method: str
    path: str
    query: Dict[str, List[str]] = field(default_factory=dict)
    headers: Dict[str, str] = field(default_factory=dict)
    body: bytes = b""

    def arg(self, name: str, default: Optional[str] = None) -> Optional[str]:
        """The first value of query parameter ``name``, or ``default``."""
        values = self.query.get(name)
        return values[0] if values else default

    def text(self) -> str:
        """The body decoded as UTF-8 (invalid bytes raise ``UnicodeDecodeError``)."""
        return self.body.decode("utf-8")

    def json(self) -> Any:
        """The body parsed as JSON (``json.JSONDecodeError`` if it is not)."""
        return json.loads(self.body or b"null")

    @classmethod
    def from_proto(cls, request: pb.HttpRequest) -> "HttpRequest":
        headers: Dict[str, str] = {}
        for header in request.headers:
            name = header.name.lower()
            headers[name] = f"{headers[name]}, {header.value}" if name in headers else header.value
        return cls(
            method=request.method.upper(),
            path=request.path or "/",
            query=parse_qs(request.query, keep_blank_values=True),
            headers=headers,
            body=bytes(request.body),
        )


@dataclass
class HttpResponse:
    """A response with full control over status, headers and body.

    Route handlers may also return plain values (see :func:`to_http_response`).
    """

    status: int = 200
    body: bytes = b""
    headers: Dict[str, str] = field(default_factory=dict)

    @classmethod
    def json(cls, value: Any, status: int = 200) -> "HttpResponse":
        """A JSON response."""
        return cls(
            status=status,
            body=json.dumps(value, ensure_ascii=False).encode("utf-8"),
            headers={"content-type": "application/json; charset=utf-8"},
        )

    @classmethod
    def text(cls, value: str, status: int = 200) -> "HttpResponse":
        """A plain text response."""
        return cls(
            status=status,
            body=value.encode("utf-8"),
            headers={"content-type": "text/plain; charset=utf-8"},
        )

    @classmethod
    def html(cls, value: str, status: int = 200) -> "HttpResponse":
        """An HTML response. The gateway serves it sandboxed, away from the console's origin."""
        return cls(
            status=status,
            body=value.encode("utf-8"),
            headers={"content-type": "text/html; charset=utf-8"},
        )

    def to_proto(self) -> pb.HttpResponse:
        return pb.HttpResponse(
            status=self.status,
            headers=[pb.HttpHeader(name=name, value=value) for name, value in self.headers.items()],
            body=self.body,
        )


def to_http_response(result: Any) -> HttpResponse:
    """Turns a route handler's return value into a response.

    ``HttpResponse`` is used as is; ``None`` is ``204 No Content``; ``str`` is plain text;
    ``bytes`` is ``application/octet-stream``; anything else is encoded as JSON.
    """
    if isinstance(result, HttpResponse):
        return result
    if result is None:
        return HttpResponse(status=204)
    if isinstance(result, str):
        return HttpResponse.text(result)
    if isinstance(result, (bytes, bytearray)):
        return HttpResponse(
            body=bytes(result), headers={"content-type": "application/octet-stream"}
        )
    return HttpResponse.json(result)


def http_route(path: str, methods: Sequence[str] = ("GET",)) -> Callable:
    """Declares a handler for HTTP requests to ``path`` below the plugin's ``http/`` root.

    The handler receives an :class:`HttpRequest` and returns an :class:`HttpResponse` or a plain
    value (see :func:`to_http_response`). Paths match exactly; a known path with another method
    answers ``405``, an unknown path ``404``, and a handler that raises ``500`` (the error is
    printed to the host's stderr, never sent to the caller).

    Args:
        path: Path starting with ``/``, e.g. ``"/stats"``; ``"/"`` is the root itself.
        methods: Methods the handler accepts.
    """
    if not path.startswith("/"):
        raise ValueError(f"http_route path must start with '/', got {path!r}")
    wanted = {method.upper() for method in methods}
    unknown = wanted - HTTP_METHODS
    if unknown or not wanted:
        raise ValueError(f"unknown HTTP methods {sorted(unknown)}; expected some of {sorted(HTTP_METHODS)}")

    def decorator(fn: Callable) -> Callable:
        routes = list(getattr(fn, "_kanon_http", []))
        routes.append((path, frozenset(wanted)))
        fn._kanon_http = routes
        return fn

    return decorator
