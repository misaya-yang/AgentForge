"""Async PostgreSQL connection pool backed by asyncpg.

Provides a thin wrapper around :mod:`asyncpg` that exposes ``execute``,
``fetch``, and ``fetchrow`` helpers for direct SQL usage.

Usage::

    pool = DatabasePool()
    await pool.init("postgresql://user:pass@host:5432/db")
    rows = await pool.fetch("SELECT id, name FROM datasets WHERE tenant_id = $1", tenant_id)
    await pool.close()
"""

from __future__ import annotations

from typing import Any

import asyncpg
import structlog

logger = structlog.get_logger()


class DatabasePool:
    """Managed asyncpg connection pool."""

    def __init__(self) -> None:
        self._pool: asyncpg.Pool | None = None

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def init(
        self,
        dsn: str,
        *,
        min_size: int = 2,
        max_size: int = 10,
    ) -> None:
        """Create the connection pool."""
        self._pool = await asyncpg.create_pool(
            dsn,
            min_size=min_size,
            max_size=max_size,
        )
        logger.info("db_pool_created", min_size=min_size, max_size=max_size)

    async def close(self) -> None:
        """Gracefully close all pooled connections."""
        if self._pool is not None:
            await self._pool.close()
            self._pool = None
            logger.info("db_pool_closed")

    # ------------------------------------------------------------------
    # Convenience helpers
    # ------------------------------------------------------------------

    @property
    def pool(self) -> asyncpg.Pool:
        if self._pool is None:
            raise RuntimeError("DatabasePool not initialised. Call init() first.")
        return self._pool

    async def execute(self, sql: str, *args: Any) -> str:
        """Execute a statement and return the status string."""
        return await self.pool.execute(sql, *args)

    async def fetch(self, sql: str, *args: Any) -> list[asyncpg.Record]:
        """Return all matching rows."""
        return await self.pool.fetch(sql, *args)

    async def fetchrow(self, sql: str, *args: Any) -> asyncpg.Record | None:
        """Return a single row or ``None``."""
        return await self.pool.fetchrow(sql, *args)

    async def fetchval(self, sql: str, *args: Any) -> Any:
        """Return a single scalar value."""
        return await self.pool.fetchval(sql, *args)
