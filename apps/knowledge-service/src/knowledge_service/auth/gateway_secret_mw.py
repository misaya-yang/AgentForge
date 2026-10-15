"""Compatibility import for knowledge-service gateway-secret auth middleware."""

from __future__ import annotations

from ai_gateway_core.auth.gateway_secret_middleware import GatewaySecretAuthMiddleware

__all__ = ["GatewaySecretAuthMiddleware"]
