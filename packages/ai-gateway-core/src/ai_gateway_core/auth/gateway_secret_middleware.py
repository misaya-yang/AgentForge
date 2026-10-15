"""Shared Starlette middleware for gateway-signed service-to-service calls."""

from __future__ import annotations

import logging

from fastapi.responses import JSONResponse
from starlette.datastructures import Headers

from .gateway_secret import GatewaySecret, InvalidGatewaySecret

logger = logging.getLogger(__name__)

UNAUTHORIZED_BODY = {
    "code": "AUTH_DENIED",
    "message": "Missing or invalid X-Gateway-Secret",
}


def validate_gateway_auth_configuration(
    *,
    secret: str,
    allow_anonymous: bool,
    allow_anonymous_setting: str,
    environment: str | None = None,
) -> None:
    """Reject a configured trust boundary that can silently bypass itself."""

    if secret.strip() and allow_anonymous:
        raise RuntimeError(
            f"{allow_anonymous_setting}=true cannot be combined with "
            "AI_PLATFORM_INTERNAL_TOKEN. Anonymous mode bypasses the "
            "gateway signature and must only be used when the shared secret "
            "is intentionally unset for local development."
        )
    if allow_anonymous and environment is not None:
        normalized_environment = environment.strip().lower()
        if normalized_environment not in {"local", "dev", "development", "test", "testing"}:
            raise RuntimeError(
                f"{allow_anonymous_setting}=true is restricted to local development/test "
                f"environments; got {normalized_environment or 'unset'}"
            )


class GatewaySecretAuthMiddleware:
    """Verify ``X-Gateway-Secret`` on every non-probe request.

    The same middleware protects internal Python services so their boundaries
    cannot drift in error shape, safe
    paths, or replay-verification behavior.
    """

    _SAFE_PATHS: frozenset[str] = frozenset(
        {"/health", "/health/live", "/health/ready", "/metrics"}
    )

    def __init__(
        self,
        app,
        *,
        gateway_secret: GatewaySecret,
        allow_anonymous: bool = False,
        separately_authenticated_paths: frozenset[str] = frozenset(),
        separately_authenticated_prefixes: frozenset[str] = frozenset(),
    ) -> None:
        self.app = app
        self._gateway_secret = gateway_secret
        self._allow_anonymous = allow_anonymous
        self._separately_authenticated_paths = separately_authenticated_paths
        self._separately_authenticated_prefixes = separately_authenticated_prefixes

    async def __call__(self, scope, receive, send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        path = scope.get("path", "")
        if (
            path in self._SAFE_PATHS
            or path in self._separately_authenticated_paths
            or any(path.startswith(prefix) for prefix in self._separately_authenticated_prefixes)
        ):
            await self.app(scope, receive, send)
            return

        headers = Headers(scope=scope)
        header = headers.get(self._gateway_secret.header_name)
        if header is None:
            if self._allow_anonymous:
                await self.app(scope, receive, send)
                return
            await _unauthorized()(scope, receive, send)
            return

        replay_receive = receive
        try:
            body = None
            if header.startswith("v2:"):
                body = await _read_body(receive)
                replay_receive = _make_replay_receive(body, receive)
            self._gateway_secret.verify(
                header,
                method=scope.get("method", ""),
                path=path,
                query=_query_string(scope),
                body=body,
                identity_headers=headers,
            )
        except InvalidGatewaySecret as exc:
            logger.warning(
                "gateway-secret verification failed: %s path=%s",
                exc,
                path,
            )
            await _unauthorized()(scope, receive, send)
            return

        scope.setdefault("state", {})["gateway_secret_verified"] = True
        await self.app(scope, replay_receive, send)


def _unauthorized() -> JSONResponse:
    return JSONResponse(status_code=401, content=UNAUTHORIZED_BODY)


async def _read_body(receive) -> bytes:
    chunks: list[bytes] = []
    while True:
        message = await receive()
        if message["type"] == "http.disconnect":
            break
        chunks.append(message.get("body", b""))
        if not message.get("more_body", False):
            break
    return b"".join(chunks)


def _make_replay_receive(body: bytes, upstream_receive):
    sent = False

    async def receive() -> dict:
        nonlocal sent
        if not sent:
            sent = True
            return {"type": "http.request", "body": body, "more_body": False}
        # Once the buffered body has been replayed, preserve the real client
        # receive channel. StreamingResponse listens here for disconnects; a
        # synthetic request or disconnect would either raise in Starlette or
        # cancel the response before its first SSE chunk is sent.
        return await upstream_receive()

    return receive


def _query_string(scope) -> str:
    raw = scope.get("query_string", b"")
    if isinstance(raw, bytes):
        return raw.decode("latin-1")
    return str(raw)


__all__ = [
    "GatewaySecretAuthMiddleware",
    "UNAUTHORIZED_BODY",
    "validate_gateway_auth_configuration",
]
