"""Cross-service ``X-Request-Id`` middleware.

The gateway emits ``X-Request-Id`` to every upstream microservice (via
``ServiceProxy._build_headers``). Each downstream service reads it via
this middleware and binds it to ``request.state.request_id`` + the
contextvar ``REQUEST_ID_CTX`` so log filters can include it.

When a service is hit DIRECTLY (no gateway in front — dev / k8s probe /
maintenance scripts), no header is present and we mint a fresh UUID
prefixed ``svc-<uuid>`` so it's distinguishable from gateway-minted ones
in log aggregations.

## Usage

```python
from fastapi import FastAPI
from ai_gateway_core.proxy.request_id_middleware import RequestIDMiddleware

app = FastAPI()
app.add_middleware(RequestIDMiddleware)
```

The header echoes back on the response so a curl from the gateway sees
the same ID it sent and downstream APIs (assistant → KB) can keep using it
when invoking each other.
"""

from __future__ import annotations

import contextvars
import uuid

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import Response

REQUEST_ID_CTX: contextvars.ContextVar[str] = contextvars.ContextVar(
    "request_id", default="",
)
"""Contextvar that holds the current request's ID. Log filters can read it
without having to thread the value through every function call."""


_MAX_LEN = 64
_SAFE_CHARS = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.")


def _is_safe_id(value: str) -> bool:
    """Reject malformed inbound request IDs to avoid log-injection / header-
    smuggling. Allows alphanumeric + ``-_.`` (UUID and ``svc-<uuid>`` styles)."""
    return 0 < len(value) <= _MAX_LEN and all(c in _SAFE_CHARS for c in value)


class RequestIDMiddleware(BaseHTTPMiddleware):
    """Reads incoming ``X-Request-Id``, falls back to a minted UUID, binds
    to ``request.state.request_id`` + ``REQUEST_ID_CTX`` contextvar, and
    echoes back on the response."""

    async def dispatch(self, request: Request, call_next) -> Response:
        incoming = (request.headers.get("x-request-id") or "").strip()
        if incoming and _is_safe_id(incoming):
            request_id = incoming
        else:
            # ``svc-`` prefix marks this as a service-minted ID so log
            # aggregators can tell direct calls from gateway-fronted ones.
            request_id = f"svc-{uuid.uuid4()}"

        request.state.request_id = request_id
        token = REQUEST_ID_CTX.set(request_id)
        try:
            response = await call_next(request)
        finally:
            REQUEST_ID_CTX.reset(token)

        response.headers["X-Request-Id"] = request_id
        return response
