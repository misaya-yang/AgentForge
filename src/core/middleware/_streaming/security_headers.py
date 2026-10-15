"""Pure-ASGI security headers that never await the full handler."""

from __future__ import annotations

from starlette.types import ASGIApp, Message, Receive, Scope, Send

_SECURITY_HEADERS: tuple[tuple[bytes, bytes], ...] = (
    (b"x-content-type-options", b"nosniff"),
    (b"x-xss-protection", b"1; mode=block"),
    (b"referrer-policy", b"strict-origin-when-cross-origin"),
    (b"strict-transport-security", b"max-age=31536000; includeSubDomains"),
)

_EMBED_PREFIX = "/embed/agents/"


class SecurityHeadersMiddleware:
    """Inject response headers on ``http.response.start`` without buffering.

    Function-style ``@app.middleware("http")`` wraps ``call_next`` and can
    hold the first SSE byte until the handler finishes. This wrapper only
    mutates the start message as it is sent.
    """

    def __init__(self, app: ASGIApp) -> None:
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        path = str(scope.get("path") or "")
        allow_embed_frame = path.startswith(_EMBED_PREFIX)

        async def send_with_headers(message: Message) -> None:
            if message["type"] == "http.response.start":
                headers = [
                    (key, value)
                    for key, value in list(message.get("headers") or [])
                    if key.lower() != b"x-frame-options"
                ]
                headers.extend(_SECURITY_HEADERS)
                if not allow_embed_frame:
                    headers.append((b"x-frame-options", b"DENY"))
                message = {**message, "headers": headers}
            await send(message)

        await self.app(scope, receive, send_with_headers)
