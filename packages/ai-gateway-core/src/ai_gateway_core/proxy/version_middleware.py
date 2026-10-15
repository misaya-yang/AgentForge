"""API version negotiation middleware."""

from __future__ import annotations

import re
from collections.abc import Mapping

_ACCEPT_RE = re.compile(r"application/vnd\.ai-gateway\.(v\d+)\+json")
_PATH_RE = re.compile(r"^/(?:api/)?(v\d+)(?:/|$)")


class APIVersionMiddleware:
    def __init__(
        self,
        app,
        *,
        default_version: str = "v1",
        deprecated_routes: Mapping[str, str] | None = None,
    ) -> None:
        self.app = app
        self.default_version = default_version
        self.deprecated_routes = dict(deprecated_routes or {})

    async def __call__(self, scope, receive, send) -> None:
        if scope.get("type") != "http":
            await self.app(scope, receive, send)
            return

        path = str(scope.get("path") or "")
        headers = scope.get("headers") or []
        accept = _header_value(headers, "accept")
        version = self._version_from_accept(accept) or self._version_from_path(path)
        scope["api_version"] = version or self.default_version

        sunset = self.deprecated_routes.get(path)

        async def wrapped_send(message):
            if message["type"] == "http.response.start" and sunset:
                response_headers = list(message.get("headers") or [])
                response_headers.append((b"deprecation", b"true"))
                response_headers.append((b"sunset", sunset.encode("latin-1")))
                message = {**message, "headers": response_headers}
            await send(message)

        await self.app(scope, receive, wrapped_send)

    @staticmethod
    def _version_from_accept(value: str) -> str | None:
        match = _ACCEPT_RE.search(value or "")
        return match.group(1) if match else None

    @staticmethod
    def _version_from_path(path: str) -> str | None:
        match = _PATH_RE.search(path or "")
        return match.group(1) if match else None


def _header_value(headers: list[tuple[bytes, bytes]], name: str) -> str:
    wanted = name.lower().encode("latin-1")
    for key, value in headers:
        if key.lower() == wanted:
            return value.decode("latin-1")
    return ""


__all__ = ["APIVersionMiddleware"]
