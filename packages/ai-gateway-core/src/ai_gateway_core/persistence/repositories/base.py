"""Base repository with shared asyncpg pool access."""
from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)

try:
    import asyncpg
    HAS_ASYNCPG = True
except ImportError:
    HAS_ASYNCPG = False
    asyncpg = None


class BaseRepository:
    """Provides shared pool access for domain-specific repositories."""

    def __init__(self, pool_holder: Any):
        """Args:
            pool_holder: Object with a ``_pool`` attribute (typically DatabaseStorage).
        """
        self._holder = pool_holder

    @property
    def _pool(self) -> Any:
        return self._holder._pool

    @property
    def enabled(self) -> bool:
        return getattr(self._holder, "enabled", False) and self._pool is not None

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if not self.enabled:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, *args)
            return dict(row) if row else None

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        if not self.enabled:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *args)
            return [dict(r) for r in rows]

    async def execute(self, query: str, *args: Any) -> str:
        if not self.enabled:
            return ""
        async with self._pool.acquire() as conn:
            return await conn.execute(query, *args)

    async def executemany(self, query: str, args: list[tuple]) -> None:
        if not self.enabled:
            return
        async with self._pool.acquire() as conn:
            await conn.executemany(query, args)
