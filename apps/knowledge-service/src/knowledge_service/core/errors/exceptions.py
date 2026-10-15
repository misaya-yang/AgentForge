"""Shim: re-export from core.exceptions for alternative import path."""
from knowledge_service.core.exceptions import (
    AuthenticationRequiredError,
    GatewayError,
    NotFoundError,
    PermissionDeniedError,
    RateLimitError,
    ResourceNotFoundError,
    ValidationFailedError,
)

__all__ = [
    "AuthenticationRequiredError",
    "GatewayError",
    "NotFoundError",
    "PermissionDeniedError",
    "ResourceNotFoundError",
    "ValidationFailedError",
    "RateLimitError",
]
