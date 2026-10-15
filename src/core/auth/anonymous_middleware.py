from __future__ import annotations

import uuid

from fastapi import Request
from starlette.middleware.base import BaseHTTPMiddleware, RequestResponseEndpoint
from starlette.responses import Response

from ...config.settings import Settings


def _is_valid_uuid(value: str) -> bool:
    try:
        uuid.UUID(str(value))
        return True
    except Exception:
        return False


def _request_is_https(request: Request) -> bool:
    proto = (request.headers.get("x-forwarded-proto") or "").split(",")[0].strip().lower()
    if proto:
        return proto == "https"
    return request.url.scheme == "https"


class AnonymousIdentityMiddleware(BaseHTTPMiddleware):
    """
    Ensure a stable anonymous identifier exists for guest users.

    - Reads from a header (for non-browser clients) or a first-party cookie.
    - If missing/invalid, mints a new UUID and sets it as an HttpOnly cookie.
    - Stores the resolved ID on `request.state.anonymous_id`.
    """

    def __init__(self, app, settings: Settings):
        super().__init__(app)
        self._cfg = settings.anonymous

    async def dispatch(self, request: Request, call_next: RequestResponseEndpoint) -> Response:
        if not getattr(self._cfg, "enabled", True):
            return await call_next(request)

        header_name = self._cfg.header_name
        cookie_name = self._cfg.cookie_name

        raw = request.headers.get(header_name) or request.cookies.get(cookie_name) or ""
        has_valid = bool(raw) and _is_valid_uuid(raw)
        anon_id = raw if has_valid else str(uuid.uuid4())

        request.state.anonymous_id = anon_id

        response = await call_next(request)

        if not has_valid:
            max_age = int(self._cfg.ttl_days) * 86400
            response.set_cookie(
                key=cookie_name,
                value=anon_id,
                max_age=max_age,
                httponly=True,
                secure=_request_is_https(request),
                samesite=str(self._cfg.same_site).lower(),
                path="/",
            )
            # Helpful for mobile/native clients that prefer headers.
            response.headers.setdefault(header_name, anon_id)

        return response
