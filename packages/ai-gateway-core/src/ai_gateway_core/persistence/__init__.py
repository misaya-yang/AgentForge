"""Database-storage contract + shared concrete implementation.

Both services need a handle to persistent storage. The concrete asyncpg-
backed ``DatabaseStorage`` was historically per-service (``src/persistence/
database.py`` in gateway, copied into the AS container via ``COPY src/``).
Phase 5f Batch C moved the concrete here so the AS image no longer needs
the gateway's ``src/`` on PYTHONPATH for DB access.

- ``DatabaseStorageLike`` — minimal Protocol for type hints in callers.
- ``DatabaseStorage`` — full asyncpg-backed concrete (~7K LOC).
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

from .database import HAS_ASYNCPG, DatabaseStorage
from .redis import HAS_REDIS, RedisStorage


@runtime_checkable
class DatabaseStorageLike(Protocol):
    """Minimal async query surface used by assistant-owned callers."""

    async def fetch_one(self, query: str, *args: Any) -> dict[str, Any] | None: ...
    async def fetch_all(self, query: str, *args: Any) -> list[dict[str, Any]]: ...
    async def execute(self, query: str, *args: Any) -> Any: ...


__all__ = [
    "DatabaseStorage",
    "DatabaseStorageLike",
    "HAS_ASYNCPG",
    "HAS_REDIS",
    "RedisStorage",
]
