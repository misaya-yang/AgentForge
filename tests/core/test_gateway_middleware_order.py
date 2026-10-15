"""
Gateway middleware order tests.

Ensures authentication runs before rate limiting so per-user/guest limits apply.
"""

from src.core.middleware.streaming import (
    StreamingAuthMiddleware,
    StreamingRateLimitMiddleware,
)
from src.main import create_app


def _find_middleware_index(app, middleware_cls: type) -> int:
    for index, item in enumerate(app.user_middleware):
        if item.cls is middleware_cls:
            return index
    raise AssertionError(f"Middleware {middleware_cls.__name__} not registered")


def test_gateway_auth_runs_before_rate_limit():
    app = create_app()
    rate_idx = _find_middleware_index(app, StreamingRateLimitMiddleware)
    auth_idx = _find_middleware_index(app, StreamingAuthMiddleware)

    # Middlewares execute in reverse order of addition. Auth must be added after
    # rate limit so it runs first and populates scope state for user limits.
    assert auth_idx < rate_idx
