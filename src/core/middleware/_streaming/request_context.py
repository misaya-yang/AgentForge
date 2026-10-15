"""Bind the Gateway request id to the shared outbound HTTP context."""

from __future__ import annotations

import uuid

from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX
from starlette.types import ASGIApp, Receive, Scope, Send


def _safe_request_id(value: object) -> str:
    normalized = str(value or "").strip()
    if not normalized or len(normalized) > 64:
        return ""
    if not all(character.isalnum() or character in "-_." for character in normalized):
        return ""
    return normalized


class RequestContextBridgeMiddleware:
    """Expose the pure-ASGI request id to tracing/internal clients.

    ``StreamingLoggingMiddleware`` owns response logging and normally creates
    ``scope.state.request_id``.  Health paths intentionally bypass that
    middleware, so this bridge also provides a safe fallback without logging
    or buffering the response.
    """

    def __init__(self, app: ASGIApp) -> None:
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        state = scope.setdefault("state", {})
        request_id = _safe_request_id(state.get("request_id"))
        if not request_id:
            for name, value in scope.get("headers", []):
                if name.lower() == b"x-request-id":
                    request_id = _safe_request_id(value.decode("ascii", errors="ignore"))
                    break
        if not request_id:
            request_id = f"svc-{uuid.uuid4()}"
        state["request_id"] = request_id

        token = REQUEST_ID_CTX.set(request_id)
        try:
            await self.app(scope, receive, send)
        finally:
            REQUEST_ID_CTX.reset(token)


__all__ = ["RequestContextBridgeMiddleware"]
