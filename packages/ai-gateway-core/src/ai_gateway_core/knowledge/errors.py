"""Shared Confluence integration errors."""

from __future__ import annotations


class ConfluenceSyncError(Exception):
    """Raised when a Confluence synchronization operation fails."""


class ConfluenceAccessDeniedError(Exception):
    """Raised when a caller cannot access a Confluence resource."""

    def __init__(self, resource_type: str, resource_id: str):
        self.resource_type = resource_type
        self.resource_id = resource_id
        super().__init__(f"Access denied to {resource_type}: {resource_id}")


__all__ = ["ConfluenceAccessDeniedError", "ConfluenceSyncError"]
