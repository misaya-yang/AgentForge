"""Shared exception hierarchy. All errors inherit from ``GatewayError``."""

from ._core import (
    AdapterNotFoundError,
    AuthenticationRequiredError,
    AuthError,
    CircuitBreakerOpenError,
    GatewayError,
    InvalidContentTypeError,
    NoHealthyInstanceError,
    NotFoundError,
    PermissionDeniedError,
    RateLimitExceededError,
    ServiceNotFoundError,
    SessionAlreadyExistsError,
    TaskCancelledError,
    TaskNotFoundError,
    ValidationFailedError,
)

__all__ = [
    "AdapterNotFoundError",
    "AuthError",
    "AuthenticationRequiredError",
    "CircuitBreakerOpenError",
    "GatewayError",
    "InvalidContentTypeError",
    "NoHealthyInstanceError",
    "NotFoundError",
    "PermissionDeniedError",
    "RateLimitExceededError",
    "ServiceNotFoundError",
    "SessionAlreadyExistsError",
    "TaskCancelledError",
    "TaskNotFoundError",
    "ValidationFailedError",
]
