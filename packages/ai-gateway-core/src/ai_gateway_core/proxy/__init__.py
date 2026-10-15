"""Shared streaming HTTP proxy for gateway → microservice hops.

This package is the single source of truth for:
- circuit breaker (half-open probe state machine)
- SSE stream-through (forced when ``content-type: text/event-stream``)
- identity-header strip + inject
- ``X-Gateway-Secret`` HMAC signing

Gateway microservice adapters import this shared implementation so their
streaming and identity semantics do not drift.
"""
from .base import (
    CircuitBreaker,
    CircuitBreakerState,
    InMemoryCounter,
    ProxyError,
    ServiceProxy,
    ServiceProxyConfig,
)
from .drain import DRAIN, DrainMiddleware, DrainState, install_signal_handlers
from .request_id_middleware import REQUEST_ID_CTX, RequestIDMiddleware

__all__ = [
    "CircuitBreaker",
    "CircuitBreakerState",
    "DRAIN",
    "DrainMiddleware",
    "DrainState",
    "InMemoryCounter",
    "ProxyError",
    "REQUEST_ID_CTX",
    "RequestIDMiddleware",
    "ServiceProxy",
    "ServiceProxyConfig",
    "install_signal_handlers",
]
