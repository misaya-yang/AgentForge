"""ASGI request-body limits for high-cost JSON endpoints."""

from __future__ import annotations

from collections.abc import Iterable

from starlette.responses import JSONResponse


class RequestBodyLimitExceeded(Exception):
    """Raised when a request body exceeds the configured byte budget."""


class RequestBodyLimitMiddleware:
    """Reject oversized bodies before FastAPI buffers or parses them."""

    def __init__(
        self,
        app,
        *,
        max_bytes: int,
        paths: Iterable[str],
        path_prefixes: Iterable[str] = (),
    ) -> None:
        self.app = app
        self.max_bytes = max_bytes
        self.paths = frozenset(paths)
        self.path_prefixes = tuple(path_prefixes)

    def _matches(self, path: str) -> bool:
        if path in self.paths:
            return True
        return any(path.startswith(prefix) for prefix in self.path_prefixes)

    async def __call__(self, scope, receive, send) -> None:
        if scope.get("type") != "http" or not self._matches(str(scope.get("path") or "")):
            await self.app(scope, receive, send)
            return

        content_length = next(
            (
                value.decode("latin-1")
                for key, value in scope.get("headers", [])
                if key.lower() == b"content-length"
            ),
            "",
        )
        try:
            if content_length and int(content_length) > self.max_bytes:
                await self._reject(scope, receive, send)
                return
        except ValueError:
            await self._reject(scope, receive, send)
            return

        size = 0

        async def limited_receive():
            nonlocal size
            message = await receive()
            if message.get("type") == "http.request":
                size += len(message.get("body", b""))
                if size > self.max_bytes:
                    raise RequestBodyLimitExceeded
            return message

        try:
            await self.app(scope, limited_receive, send)
        except RequestBodyLimitExceeded:
            await self._reject(scope, receive, send)

    async def _reject(self, scope, receive, send) -> None:
        response = JSONResponse(
            {"detail": "Request body too large"},
            status_code=413,
            headers={"Retry-After": "0"},
        )
        await response(scope, receive, send)
