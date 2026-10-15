"""Shim: re-export Settings so `from ...config.settings import Settings` works."""
from knowledge_service.config import Settings

__all__ = ["Settings"]
