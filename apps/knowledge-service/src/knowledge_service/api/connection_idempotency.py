"""Connection setup owns immutable UUID admission, never a cached auth result."""

import re

from ai_gateway_core.comm import IdempotencyMiddleware

_CONTROL = re.compile(r"^(?:/api/v1)?/knowledge/[^/]+/sources/confluence/connections/[^/]+/control/?$")

_REPLACE = re.compile(r"^(?:/api/v1)?/knowledge/[^/]+/sources/confluence/connections/[^/]+/credentials/?$")

_CREATE = re.compile(r"^(?:/api/v1)?/knowledge/[^/]+/sources/confluence/connections/?$")


class ConnectionIdempotencyMiddleware(IdempotencyMiddleware):
    async def __call__(self, scope, receive, send):
        if (
            scope.get("type") == "http"
            and (
                (scope.get("method") == "POST" and (_CREATE.fullmatch(scope.get("path", "")) or _REPLACE.fullmatch(scope.get("path", ""))))
                or (scope.get("method") == "PATCH" and _CONTROL.fullmatch(scope.get("path", "")))
            )
        ):
            await self.app(scope, receive, send)
            return
        await super().__call__(scope, receive, send)
