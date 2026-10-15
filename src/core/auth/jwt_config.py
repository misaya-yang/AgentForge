"""
Unified JWT configuration module.

This module provides centralized JWT configuration to ensure consistency
between token signing (auth.py) and verification (deps.py).
"""

import logging
import os

logger = logging.getLogger(__name__)

DEFAULT_JWT_ALGORITHM = "HS256"

_INSECURE_SECRETS = frozenset({
    "",
    "default-secret-change-me",
    "your-jwt-secret-key",
    "secret",
    "changeme",
})

_warned_insecure = False


def get_jwt_secret(configured_secret: str | None) -> str:
    """
    Get JWT secret. Raises in production if not properly configured.

    Args:
        configured_secret: The secret from settings.authentication.jwt.secret

    Returns:
        The actual secret to use for JWT operations

    Raises:
        RuntimeError: If no secret configured and not in dev mode
    """
    global _warned_insecure

    secret = configured_secret or os.environ.get("JWT_SECRET", "")

    if not secret or secret in _INSECURE_SECRETS:
        # Allow insecure secret only in explicit dev mode
        if os.environ.get("GATEWAY_DEV_MODE", "").lower() in ("1", "true"):
            if not _warned_insecure:
                logger.warning(
                    "Using insecure JWT secret in dev mode! "
                    "Set GATEWAY_AUTHENTICATION__JWT__SECRET for production."
                )
                _warned_insecure = True
            return secret or "dev-only-insecure-secret"
        raise RuntimeError(
            "JWT secret not configured. Set GATEWAY_AUTHENTICATION__JWT__SECRET "
            "or JWT_SECRET environment variable."
        )

    return secret


def get_jwt_algorithms(configured_algorithms: list[str] | None) -> list[str]:
    """
    Get JWT algorithms with fallback.

    Args:
        configured_algorithms: The algorithms from settings.authentication.jwt.algorithms

    Returns:
        List of algorithms to use for JWT operations
    """
    if configured_algorithms and len(configured_algorithms) > 0:
        return configured_algorithms
    return [DEFAULT_JWT_ALGORITHM]
