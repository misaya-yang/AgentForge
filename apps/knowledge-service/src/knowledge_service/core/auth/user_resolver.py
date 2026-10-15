"""Shim: re-export UserContext from the KB service auth module."""
from knowledge_service.auth.user_context import UserContext

__all__ = ["UserContext"]
