"""Authentication helpers (gateway-delegated)."""

from .gateway_secret_mw import GatewaySecretAuthMiddleware
from .user_context import ANONYMOUS_CONTEXT, UserContext, get_user_context

__all__ = [
    "ANONYMOUS_CONTEXT",
    "GatewaySecretAuthMiddleware",
    "UserContext",
    "get_user_context",
]
