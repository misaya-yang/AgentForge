"""
PostgreSQL 数据库存储层

提供与 database/schema.sql 表结构对应的完整 CRUD 操作
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import re as _re
import time
from datetime import date, datetime
from pathlib import Path
from typing import Any

from ai_gateway_core.logging import record_internal_exception

try:
    import asyncpg

    HAS_ASYNCPG = True
except ImportError:
    HAS_ASYNCPG = False
    asyncpg = None

logger = logging.getLogger(__name__)

SCHEMA_AUTHORITY_MESSAGE = (
    "application database auto-init is retired; run "
    "`python -m database.authority migrate` before starting services"
)


def _resolve_schema_root() -> Path:
    """Locate the repo's ``database/`` directory containing schema.sql + migrations.

    The path was historically computed as ``Path(__file__).parent ** 3 /
    "database"`` because this module lived at ``src/persistence/database.py``.
    After Phase 5f Batch C the canonical location is
    ``packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py`` —
    a different depth — so the hard-coded parent count would resolve to the
    wrong directory.

    Resolution order:
      1. ``DATABASE_SCHEMA_DIR`` env var (operator override).
      2. Walk up from this file looking for a sibling ``database/schema.sql``.
      3. Check runtime working-directory candidates such as Docker's ``/app``.
      4. Legacy 3-parents fallback (only correct under the old src/ layout).
    """
    override = os.environ.get("DATABASE_SCHEMA_DIR")
    if override:
        return Path(override)
    here = Path(__file__).resolve()
    for ancestor in here.parents:
        if (ancestor / "database" / "schema.sql").is_file():
            return ancestor / "database"
    cwd = Path.cwd().resolve()
    for candidate in (cwd / "database", Path("/app/database")):
        if (candidate / "schema.sql").is_file():
            return candidate
    return here.parent.parent.parent / "database"


_SCHEMA_ROOT = _resolve_schema_root()

# Column name validation regex — only lowercase letters, digits, underscores allowed.
# Prevents SQL injection via dynamic field names in UPDATE SET clauses.
_SAFE_COLUMN_RE = _re.compile(r"^[a-z][a-z0-9_]*$")


def _validate_column_name(name: str) -> str:
    """Validate a SQL column name against a strict allowlist pattern.

    Raises ValueError if the name contains unsafe characters.
    """
    if not _SAFE_COLUMN_RE.match(name):
        raise ValueError(f"Unsafe SQL column name: {name!r}")
    return name


def _build_safe_set_clause(
    updates: list[str],
) -> str:
    """Build a SET clause from validated 'col = $N' fragments.

    Each fragment must have the form 'column_name = $N'.
    Validates the column name portion before joining.
    """
    for fragment in updates:
        col_name = fragment.split("=", 1)[0].strip()
        _validate_column_name(col_name)
    return ", ".join(updates)


def build_service_query(
    status: str | None = None,
    service_type: str | None = None,
    tags: list[str] | None = None,
) -> tuple[str, list[Any]]:
    """Build service query with safe parameterization.

    Uses len(params) for parameter indexing to avoid off-by-one errors.
    This is safer than manually tracking param_idx.

    Args:
        status: Filter by service status
        service_type: Filter by service type
        tags: Filter by tags (array overlap)

    Returns:
        Tuple of (query_string, params_list)
    """
    query_parts = ["SELECT * FROM services WHERE 1=1"]
    params: list[Any] = []

    if status:
        params.append(status)
        query_parts.append(f"AND status = ${len(params)}")

    if service_type:
        params.append(service_type)
        query_parts.append(f"AND service_type = ${len(params)}")

    if tags:
        params.append(tags)
        query_parts.append(f"AND tags && ${len(params)}")

    query_parts.append("ORDER BY created_at DESC")

    return " ".join(query_parts), params


class DatabaseStorage:
    """PostgreSQL 数据库存储

    Acts as a facade over domain-specific repositories.
    See ``src/persistence/repositories/`` for the extracted implementations.
    """

    def __init__(
        self,
        dsn: str | None = None,
        enabled: bool = False,
        auto_init: bool = False,
        schema_path: str | None = None,
        permission_cache_ttl_seconds: int = 60,
        pool_min_size: int = int(os.environ.get("DB_POOL_MIN_SIZE", "2")),
        pool_max_size: int = int(os.environ.get("DB_POOL_MAX_SIZE", "20")),
        command_timeout_s: int = int(os.environ.get("DB_COMMAND_TIMEOUT_S", "30")),
        api_key_usage_flush_interval_seconds: int = 2,
        api_key_usage_flush_batch_size: int = 100,
        bootstrap_admin_password_hash: str | None = None,
    ):
        self.dsn = dsn
        self.enabled = enabled and HAS_ASYNCPG and dsn
        self.auto_init = bool(auto_init)
        self._bootstrap_admin_password_hash = bootstrap_admin_password_hash or None
        self.schema_path = schema_path or str(
            _SCHEMA_ROOT / "schema.sql"
        )
        self._pool: Any | None = None
        self._pool_min_size = max(int(pool_min_size), 1)
        self._pool_max_size = max(int(pool_max_size), self._pool_min_size)
        # SPO-05 / D1: bound every pooled query so a stuck statement cannot
        # pin a connection forever.
        self._command_timeout_s = max(int(command_timeout_s or 0), 0)
        self._permission_cache: dict[str, tuple[list[str], float]] = {}
        self._permission_cache_max_size = 10000  # Prevent unbounded memory growth
        self._permission_cache_ttl_seconds = max(int(permission_cache_ttl_seconds or 0), 0)
        self._permission_cache_lock = asyncio.Lock()
        self._api_key_usage_flush_interval_seconds = max(
            int(api_key_usage_flush_interval_seconds or 0), 0
        )
        self._api_key_usage_flush_batch_size = max(int(api_key_usage_flush_batch_size or 0), 0)
        self._api_key_usage_buffer: dict[str, int] = {}
        self._api_key_usage_lock = asyncio.Lock()
        self._api_key_usage_task: asyncio.Task | None = None

        # Domain-specific repositories (Phase 2 refactoring)
        from .repositories.api_key_repository import DatabaseAPIKeyRepository
        from .repositories.service_repository import DatabaseServiceRepository
        from .repositories.session_repository import DatabaseSessionRepository
        from .repositories.task_repository import DatabaseTaskRepository
        from .repositories.user_repository import DatabaseUserRepository

        self.repos = {
            "services": DatabaseServiceRepository(self),
            "sessions": DatabaseSessionRepository(self),
            "tasks": DatabaseTaskRepository(self),
            "users": DatabaseUserRepository(self),
            "api_keys": DatabaseAPIKeyRepository(self),
        }

    async def _get_cached_permissions(self, user_id: str) -> list[str] | None:
        if self._permission_cache_ttl_seconds <= 0:
            return None
        async with self._permission_cache_lock:
            entry = self._permission_cache.get(user_id)
            if not entry:
                return None
            permissions, expires_at = entry
            if time.time() >= expires_at:
                self._permission_cache.pop(user_id, None)
                return None
            return list(permissions)

    async def _set_cached_permissions(self, user_id: str, permissions: list[str]) -> None:
        """Set cached permissions with size limit enforcement.

        Uses FIFO eviction when cache exceeds max_size.
        """
        if self._permission_cache_ttl_seconds <= 0:
            return
        async with self._permission_cache_lock:
            # Enforce size limit (simple FIFO eviction)
            while len(self._permission_cache) >= self._permission_cache_max_size:
                if self._permission_cache:
                    # Remove oldest entry (first inserted in dict)
                    oldest_key = next(iter(self._permission_cache))
                    del self._permission_cache[oldest_key]
                else:
                    break

            self._permission_cache[user_id] = (
                list(permissions),
                time.time() + self._permission_cache_ttl_seconds,
            )

    async def _invalidate_permission_cache(self, user_id: str | None = None) -> None:
        async with self._permission_cache_lock:
            if user_id:
                self._permission_cache.pop(user_id, None)
            else:
                self._permission_cache.clear()

    async def _update_api_key_usage_sync(self, key_hash: str) -> None:
        """Fallback path: update usage counters synchronously."""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE api_keys SET
                    last_used_at = NOW(),
                    use_count = use_count + 1
                WHERE key_hash = $1
            """,
                key_hash,
            )

    async def _flush_api_key_usage_buffer(self) -> None:
        """Flush buffered API key usage counters in batch."""
        if not self._pool:
            return

        async with self._api_key_usage_lock:
            if not self._api_key_usage_buffer:
                return
            pending = self._api_key_usage_buffer
            self._api_key_usage_buffer = {}

        try:
            rows = [(key_hash, count) for key_hash, count in pending.items() if count > 0]
            if not rows:
                return
            async with self._pool.acquire() as conn, conn.transaction():
                await conn.executemany(
                    """
                    UPDATE api_keys SET
                        last_used_at = NOW(),
                        use_count = use_count + $2
                    WHERE key_hash = $1
                """,
                    rows,
                )
        except (asyncpg.PostgresError, OSError):
            # Put counts back to buffer to avoid silently losing stats.
            async with self._api_key_usage_lock:
                for key_hash, count in pending.items():
                    self._api_key_usage_buffer[key_hash] = (
                        self._api_key_usage_buffer.get(key_hash, 0) + count
                    )
            raise

    async def _run_api_key_usage_flusher(self) -> None:
        """Background task to periodically flush buffered usage updates."""
        interval = self._api_key_usage_flush_interval_seconds
        if interval <= 0:
            return
        try:
            while True:
                await asyncio.sleep(interval)
                try:
                    await self._flush_api_key_usage_buffer()
                except (asyncpg.PostgresError, OSError) as exc:
                    record_internal_exception(logger, "assistant.database.api_key_usage.flush_failed", exc)
        except asyncio.CancelledError:
            raise

    async def _track_api_key_usage(self, key_hash: str) -> None:
        """Track API key usage with async batching; fallback to sync on errors."""
        if (
            self._api_key_usage_flush_interval_seconds <= 0
            or self._api_key_usage_flush_batch_size <= 1
        ):
            await self._update_api_key_usage_sync(key_hash)
            return

        try:
            should_flush = False
            async with self._api_key_usage_lock:
                self._api_key_usage_buffer[key_hash] = (
                    self._api_key_usage_buffer.get(key_hash, 0) + 1
                )
                should_flush = (
                    len(self._api_key_usage_buffer) >= self._api_key_usage_flush_batch_size
                )
            if should_flush:
                await self._flush_api_key_usage_buffer()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.api_key_usage.batch_failed", exc)
            detached_from_buffer = False
            async with self._api_key_usage_lock:
                buffered_count = self._api_key_usage_buffer.get(key_hash, 0)
                if buffered_count > 0:
                    detached_from_buffer = True
                    if buffered_count == 1:
                        self._api_key_usage_buffer.pop(key_hash, None)
                    else:
                        self._api_key_usage_buffer[key_hash] = buffered_count - 1

            try:
                await self._update_api_key_usage_sync(key_hash)
            except (asyncpg.PostgresError, OSError) as sync_exc:
                record_internal_exception(logger, "assistant.database.api_key_usage.sync_fallback_failed", sync_exc)
                if detached_from_buffer:
                    async with self._api_key_usage_lock:
                        self._api_key_usage_buffer[key_hash] = (
                            self._api_key_usage_buffer.get(key_hash, 0) + 1
                        )

    async def connect(self) -> None:
        """建立数据库连接池"""
        if not self.enabled:
            return
        if not HAS_ASYNCPG:
            raise RuntimeError("asyncpg is not installed. Run: pip install asyncpg")
        if self.auto_init:
            # Fail before opening a pool: application identities must never
            # acquire a connection with the intent to execute schema DDL.
            raise RuntimeError(SCHEMA_AUTHORITY_MESSAGE)
        self._pool = await asyncpg.create_pool(
            self.dsn,
            min_size=self._pool_min_size,
            max_size=self._pool_max_size,
            command_timeout=self._command_timeout_s,
        )
        logger.info(
            f"Database pool created: min_size={self._pool_min_size}, max_size={self._pool_max_size}"
        )
        if (
            self._api_key_usage_flush_interval_seconds > 0
            and self._api_key_usage_flush_batch_size > 1
            and self._api_key_usage_task is None
        ):
            self._api_key_usage_task = asyncio.create_task(self._run_api_key_usage_flusher())
        # Password bootstrap is application data, not schema DDL. It is an
        # update-only, fill-if-empty operation and remains separate from the
        # retired auto-migration surface.
        if getattr(self, "_bootstrap_admin_password_hash", None):
            await self._ensure_bootstrap_admin_password_hash()

    async def close(self) -> None:
        """关闭连接池"""
        if self._api_key_usage_task is not None:
            self._api_key_usage_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._api_key_usage_task
            self._api_key_usage_task = None
        with contextlib.suppress(Exception):
            await self._flush_api_key_usage_buffer()
        if self._pool:
            await self._pool.close()
            self._pool = None

    async def fetchrow(self, query: str, *args) -> Any | None:
        """执行查询并返回单行结果"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            return await conn.fetchrow(query, *args)

    async def fetch(self, query: str, *args) -> list[Any]:
        """执行查询并返回多行结果"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            return await conn.fetch(query, *args)

    async def execute(self, query: str, *args) -> str:
        """执行 SQL 语句（INSERT/UPDATE/DELETE）并返回状态字符串"""
        if not self._pool:
            return ""
        async with self._pool.acquire() as conn:
            return await conn.execute(query, *args)

    async def executemany(self, query: str, args_list: list[tuple[Any, ...]]) -> None:
        """Batch execute statements with different parameter tuples."""
        if not self._pool or not args_list:
            return
        async with self._pool.acquire() as conn:
            await conn.executemany(query, args_list)

    async def execute_schema(self, schema_path: str) -> None:
        """Retired DDL bypass retained only to fail old callers closed."""
        raise RuntimeError(f"{SCHEMA_AUTHORITY_MESSAGE}; refused file {schema_path!r}")

    async def _schema_is_missing(self) -> bool:
        """Detect whether core tables are missing (e.g., first run).

        Phase 6 schema split: gateway startup checks only gateway-owned
        storage. Knowledge schema readiness belongs to knowledge-service.
        """
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # Use to_regclass for a cheap existence check.
            services = await conn.fetchval("SELECT to_regclass('gateway.services')")
            return services is None

    async def _auto_initialize_schema(self) -> None:
        """Auto-run schema.sql when core tables are missing (idempotent)."""
        if not self._pool:
            return
        try:
            if not await self._schema_is_missing():
                return
        except (asyncpg.PostgresError, OSError) as exc:
            # If we cannot check, skip auto-init to avoid masking the real error.
            record_internal_exception(logger, "assistant.database.schema_state_check_failed", exc)
            return

        schema_path = self.schema_path
        if not schema_path or not Path(schema_path).exists():
            raise RuntimeError(f"Database schema not found: {schema_path}")

        await self.execute_schema(schema_path)

    async def _account_permission_schema_missing(self) -> bool:
        """Check if account/permission schema pieces are missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            password_col = await conn.fetchval(
                """
                SELECT 1
                FROM information_schema.columns
                WHERE table_name = 'users' AND column_name = 'password_hash'
                """
            )
            permissions_table = await conn.fetchval("SELECT to_regclass('public.permissions')")
            if password_col is None or permissions_table is None:
                return True

            # schema.sql already contains the account tables, so checking DDL
            # alone can incorrectly skip migration 005 on a fresh database.
            # Its seed rows are part of the usable account-system contract.
            admin_permission = await conn.fetchval(
                "SELECT 1 FROM permissions WHERE permission_code = 'admin:*' LIMIT 1"
            )
            bootstrap_admin = await conn.fetchval(
                "SELECT 1 FROM users WHERE user_id = 'admin' LIMIT 1"
            )
            return admin_permission is None or bootstrap_admin is None

    async def _auto_apply_account_permission_migration(self) -> None:
        """Apply account/permission migration when required."""
        if not self._pool:
            return
        try:
            missing = await self._account_permission_schema_missing()
        except (asyncpg.PostgresError, OSError) as exc:
            # If we cannot determine schema state, skip auto-migration.
            record_internal_exception(logger, "assistant.database.account_permission_check_failed", exc)
            return
        if not missing:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "005_account_permission_system.sql"
        )
        if not migration_path.exists():
            raise RuntimeError(f"Migration not found: {migration_path}")

        await self.execute_schema(str(migration_path))

    async def _platform_admin_schema_missing(self) -> bool:
        """Check the strict bootstrap platform-admin role and assignment."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            platform_role = await conn.fetchval(
                """
                SELECT 1
                FROM rbac_roles
                WHERE role_name = 'platform_admin'
                  AND is_system = TRUE
                  AND 'admin:*' = ANY(permissions)
                LIMIT 1
                """
            )
            bootstrap_assignment = await conn.fetchval(
                """
                SELECT 1
                FROM users AS u
                JOIN user_roles AS ur
                  ON ur.user_id = u.user_id
                 AND ur.role_name = 'platform_admin'
                WHERE u.user_id = 'admin'
                  AND u.created_by = 'system'
                  AND 'platform_admin' = ANY(u.roles)
                LIMIT 1
                """
            )
            return platform_role is None or bootstrap_assignment is None

    async def _auto_apply_platform_admin_migration(self) -> None:
        """Apply the platform-admin migration after bootstrap account setup."""
        if not self._pool:
            return
        try:
            missing = await self._platform_admin_schema_missing()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(
                logger,
                "assistant.database.platform_admin_check_failed",
                exc,
            )
            return
        if not missing:
            return

        migration_path = _SCHEMA_ROOT / "migrations" / "086_platform_admin_role.sql"
        if not migration_path.exists():
            raise RuntimeError(f"Migration not found: {migration_path}")

        await self.execute_schema(str(migration_path))

    async def _ensure_bootstrap_admin_password_hash(self) -> None:
        """Initialize the local admin password once without overwriting it.

        The account migration intentionally contains no known password. The
        open-source quickstart supplies a freshly generated bcrypt hash here;
        existing admin credentials are preserved on every later startup.
        """
        if not self._pool or not self._bootstrap_admin_password_hash:
            return

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE users
                SET password_hash = $1,
                    updated_at = NOW()
                WHERE user_id = 'admin'
                  AND (password_hash IS NULL OR password_hash = '')
                """,
                self._bootstrap_admin_password_hash,
            )

    async def _user_permissions_schema_missing(self) -> bool:
        """Check if user_permissions table is missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            table_exists = await conn.fetchval("SELECT to_regclass('public.user_permissions')")
            return table_exists is None

    async def _auto_apply_user_extra_permissions_migration(self) -> None:
        """Apply user extra permissions migration (006) when required."""
        if not self._pool:
            return
        try:
            missing = await self._user_permissions_schema_missing()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.user_permissions_check_failed", exc)
            return
        if not missing:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "006_user_extra_permissions.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 006 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 006_user_extra_permissions.sql")
        except asyncpg.PostgresError as e:
            # If table already exists or other non-critical error, log and continue
            if "already exists" in str(e).lower():
                logger.info("Migration 006 already applied (user_permissions table exists)")
            else:
                record_internal_exception(logger, "assistant.database.migration_006_failed", e)

    async def _api_keys_needs_migration(self) -> bool:
        """Check if api_keys table needs migration (missing key_id column)."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # Check if key_id column exists
            col_exists = await conn.fetchval("""
                SELECT column_name FROM information_schema.columns
                WHERE table_name = 'api_keys' AND column_name = 'key_id'
            """)
            return col_exists is None

    async def _auto_apply_api_keys_migration(self) -> None:
        """Apply api_keys migration (020) when required."""
        if not self._pool:
            return
        try:
            needs_migration = await self._api_keys_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.api_keys_check_failed", exc)
            return
        if not needs_migration:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "020_api_keys.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 020 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 020_api_keys.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 020 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_020_failed", e)

    async def _session_memory_schema_missing(self) -> bool:
        """Check if session_memory table is missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            table_exists = await conn.fetchval("""
                SELECT tablename FROM pg_tables
                WHERE schemaname = 'public' AND tablename = 'session_memory'
            """)
            return table_exists is None

    async def _auto_apply_assistant_memory_migration(self) -> None:
        """Apply assistant memory migration (024) when required."""
        if not self._pool:
            return
        try:
            missing = await self._session_memory_schema_missing()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.session_memory_check_failed", exc)
            return
        if not missing:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "024_assistant_memory.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 024 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 024_assistant_memory.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 024 already applied (session_memory table exists)")
            else:
                record_internal_exception(logger, "assistant.database.migration_024_failed", e)

    async def _observability_governance_needs_migration(self) -> bool:
        """Check whether observability/governance schema migration (033) is required."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            trace_table = await conn.fetchval("SELECT to_regclass('public.request_traces')")
            if trace_table is None:
                return True

            usage_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'usage_records' AND column_name = 'request_total_duration_ms'
                """
            )
            if usage_col is None:
                return True

            quota_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'user_quotas' AND column_name = 'overage_strategy'
                """
            )
            if quota_col is None:
                return True

            api_key_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'api_keys' AND column_name = 'allowed_models'
                """
            )
            return api_key_col is None

    async def _auto_apply_observability_governance_migration(self) -> None:
        """Apply migration 033 for observability/tracing/quota governance when required."""
        if not self._pool:
            return
        try:
            needs = await self._observability_governance_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.observability_governance_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "033_observability_and_quota_governance.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 033 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 033_observability_and_quota_governance.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 033 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_033_failed", e)

    async def _assistant_gateway_needs_migration(self) -> bool:
        """Check whether assistant gateway schema migration (034) is required."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            runs_table = await conn.fetchval("SELECT to_regclass('public.assistant_runs')")
            queue_table = await conn.fetchval(
                "SELECT to_regclass('public.assistant_command_queue')"
            )
            approvals_table = await conn.fetchval(
                "SELECT to_regclass('public.assistant_tool_approvals')"
            )
            if runs_table is None or queue_table is None or approvals_table is None:
                return True

            session_namespace_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'session_memory' AND column_name = 'namespace'
                """
            )
            user_namespace_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'user_memory' AND column_name = 'namespace'
                """
            )
            return session_namespace_col is None or user_namespace_col is None

    async def _auto_apply_assistant_gateway_migration(self) -> None:
        """Apply migration 034 for assistant gateway + memory v2 schema when required."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_gateway_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.assistant_gateway_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "034_assistant_gateway_foundation.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 034 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 034_assistant_gateway_foundation.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 034 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_034_failed", e)

    async def _assistant_memory_sot_needs_migration(self) -> bool:
        """Check whether assistant memory source-of-truth tables are missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            sources = await conn.fetchval("SELECT to_regclass('public.assistant_memory_sources')")
            chunks = await conn.fetchval("SELECT to_regclass('public.assistant_memory_chunks')")
            reflections = await conn.fetchval(
                "SELECT to_regclass('public.assistant_memory_reflections')"
            )
            return sources is None or chunks is None or reflections is None

    async def _auto_apply_assistant_memory_sot_migration(self) -> None:
        """Apply migration 035 for assistant memory SoT/index tables."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_memory_sot_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.memory_sot_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "035_assistant_memory_sot.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 035 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 035_assistant_memory_sot.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 035 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_035_failed", e)

    async def _assistant_queue_lane_needs_migration(self) -> bool:
        """Check whether command queue lane columns are missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            lane_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'assistant_command_queue' AND column_name = 'lane'
                """
            )
            mode_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'assistant_command_queue' AND column_name = 'queue_mode'
                """
            )
            priority_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'assistant_command_queue' AND column_name = 'priority'
                """
            )
            steer_col = await conn.fetchval(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'assistant_command_queue' AND column_name = 'steer_payload'
                """
            )
            return lane_col is None or mode_col is None or priority_col is None or steer_col is None

    async def _auto_apply_assistant_queue_lane_migration(self) -> None:
        """Apply migration 036 for queue lane/mode/priority columns."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_queue_lane_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.queue_lane_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "036_assistant_queue_lanes.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 036 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 036_assistant_queue_lanes.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 036 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_036_failed", e)

    async def _assistant_skills_needs_migration(self) -> bool:
        """Check whether skill registry tables are missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            skills = await conn.fetchval("SELECT to_regclass('public.assistant_skills')")
            versions = await conn.fetchval("SELECT to_regclass('public.assistant_skill_versions')")
            runs = await conn.fetchval("SELECT to_regclass('public.assistant_skill_runs')")
            return skills is None or versions is None or runs is None

    async def _auto_apply_assistant_skills_migration(self) -> None:
        """Apply migration 037 for dynamic skills."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_skills_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.runtime_skills_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "037_assistant_skills.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 037 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 037_assistant_skills.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 037 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_037_failed", e)

    async def _assistant_scheduler_audit_needs_migration(self) -> bool:
        """Check whether scheduler and audit tables are missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            jobs = await conn.fetchval("SELECT to_regclass('public.assistant_scheduler_jobs')")
            audit = await conn.fetchval("SELECT to_regclass('public.assistant_audit_events')")
            return jobs is None or audit is None

    async def _auto_apply_assistant_scheduler_audit_migration(self) -> None:
        """Apply migration 038 for scheduler/audit tables."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_scheduler_audit_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.scheduler_audit_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "038_assistant_scheduler_audit.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 038 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 038_assistant_scheduler_audit.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 038 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_038_failed", e)

    async def _assistant_context_metrics_needs_migration(self) -> bool:
        """Check whether context breakdown table is missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            table = await conn.fetchval("SELECT to_regclass('public.assistant_context_breakdown')")
            return table is None

    async def _auto_apply_assistant_context_metrics_migration(self) -> None:
        """Apply migration 039 for context detail metrics."""
        if not self._pool:
            return
        try:
            needs = await self._assistant_context_metrics_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.context_metrics_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "039_assistant_context_metrics.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 039 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 039_assistant_context_metrics.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 039 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_039_failed", e)

    # ------------------------------------------------------------------
    # Migration 044: Tenant Soft Isolation (ADR-002)
    # ------------------------------------------------------------------

    async def _tenant_isolation_needs_migration(self) -> bool:
        """Check whether tenant_tool_policies table is missing."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            table = await conn.fetchval("SELECT to_regclass('public.tenant_tool_policies')")
            return table is None

    async def _auto_apply_tenant_isolation_migration(self) -> None:
        """Apply migration 044 for tenant soft isolation."""
        if not self._pool:
            return
        try:
            needs = await self._tenant_isolation_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.tenant_isolation_check_failed", exc)
            return
        if not needs:
            return

        migration_path = (
            _SCHEMA_ROOT
            / "migrations"
            / "044_tenant_soft_isolation.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 044 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 044_tenant_soft_isolation.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 044 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_044_failed", e)

    # ------------------------------------------------------------------
    # Migration 045: Conversation Shares
    # ------------------------------------------------------------------

    async def _conversation_shares_needs_migration(self) -> bool:
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            table = await conn.fetchval("SELECT to_regclass('public.conversation_shares')")
            return table is None

    async def _auto_apply_conversation_shares_migration(self) -> None:
        if not self._pool:
            return
        try:
            needs = await self._conversation_shares_needs_migration()
        except (asyncpg.PostgresError, OSError) as exc:
            record_internal_exception(logger, "assistant.database.conversation_shares_check_failed", exc)
            return
        if not needs:
            return
        migration_path = (
            _SCHEMA_ROOT / "migrations" / "045_conversation_shares.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 045 not found: {migration_path}")
            return
        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 045_conversation_shares.sql")
        except asyncpg.PostgresError as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 045 already applied")
            else:
                record_internal_exception(logger, "assistant.database.migration_045_failed", e)

    # =========================================================================
    # 服务定义表 (services)
    # =========================================================================

    async def save_service(self, service: dict[str, Any]) -> None:
        """保存或更新服务"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO services (
                    service_id, name, description, version, service_type,
                    connector_type, connector_config, supported_modes,
                    accepted_content_types, output_content_types,
                    input_schema, output_schema, session_enabled, session_adapter,
                    timeout, max_retries, retry_delay, circuit_breaker_enabled,
                    failure_threshold, recovery_timeout, rate_limit, concurrency_limit,
                    service_config, async_config, status, tags, metadata
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20,
                    $21, $22, $23, $24, $25, $26, $27
                )
                ON CONFLICT (service_id) DO UPDATE SET
                    name = EXCLUDED.name,
                    description = EXCLUDED.description,
                    version = EXCLUDED.version,
                    service_type = EXCLUDED.service_type,
                    connector_type = EXCLUDED.connector_type,
                    connector_config = EXCLUDED.connector_config,
                    supported_modes = EXCLUDED.supported_modes,
                    accepted_content_types = EXCLUDED.accepted_content_types,
                    output_content_types = EXCLUDED.output_content_types,
                    input_schema = EXCLUDED.input_schema,
                    output_schema = EXCLUDED.output_schema,
                    session_enabled = EXCLUDED.session_enabled,
                    session_adapter = EXCLUDED.session_adapter,
                    timeout = EXCLUDED.timeout,
                    max_retries = EXCLUDED.max_retries,
                    retry_delay = EXCLUDED.retry_delay,
                    circuit_breaker_enabled = EXCLUDED.circuit_breaker_enabled,
                    failure_threshold = EXCLUDED.failure_threshold,
                    recovery_timeout = EXCLUDED.recovery_timeout,
                    rate_limit = EXCLUDED.rate_limit,
                    concurrency_limit = EXCLUDED.concurrency_limit,
                    service_config = EXCLUDED.service_config,
                    async_config = EXCLUDED.async_config,
                    status = EXCLUDED.status,
                    tags = EXCLUDED.tags,
                    metadata = EXCLUDED.metadata,
                    updated_at = NOW()
            """,
                service.get("service_id"),
                service.get("name"),
                service.get("description"),
                service.get("version", "1.0.0"),
                service.get("service_type", "custom"),
                service.get("connector_type", "http"),
                json.dumps(service.get("connector_config", {})),
                service.get("supported_modes", ["sync", "stream"]),
                service.get("accepted_content_types", ["text"]),
                service.get("output_content_types", ["text"]),
                json.dumps(service.get("input_schema", {})),
                json.dumps(service.get("output_schema", {})),
                service.get("session_enabled", False),
                service.get("session_adapter"),
                service.get("timeout", 60),
                service.get("max_retries", 3),
                service.get("retry_delay", 1.0),
                service.get("circuit_breaker_enabled", True),
                service.get("failure_threshold", 5),
                service.get("recovery_timeout", 30),
                json.dumps(service.get("rate_limit")) if service.get("rate_limit") else None,
                service.get("concurrency_limit"),
                json.dumps(service.get("service_config", {})),
                json.dumps(service.get("async_config")) if service.get("async_config") else None,
                service.get("status", "active"),
                service.get("tags", []),
                json.dumps(service.get("metadata", {})),
            )

    async def get_service(self, service_id: str) -> dict[str, Any] | None:
        """获取服务定义"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM services WHERE service_id = $1", service_id)
            return self._row_to_dict(row) if row else None

    async def list_services(
        self,
        status: str | None = None,
        service_type: str | None = None,
        tags: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        """获取服务列表

        Uses build_service_query() for safe parameterization.
        """
        if not self._pool:
            return []

        query, params = build_service_query(status=status, service_type=service_type, tags=tags)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def delete_service(self, service_id: str) -> bool:
        """删除服务"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute("DELETE FROM services WHERE service_id = $1", service_id)
            return result == "DELETE 1"

    async def update_service_status(self, service_id: str, status: str) -> None:
        """更新服务状态"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE services SET status = $1, updated_at = NOW() WHERE service_id = $2",
                status,
                service_id,
            )

    # =========================================================================
    # 会话表 (sessions)
    # =========================================================================

    async def save_session(self, session: dict[str, Any]) -> None:
        """保存或更新会话"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO assistant.sessions (
                    session_id, service_id, user_id, tenant_id,
                    state, history, metadata, config, status, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (session_id) DO UPDATE SET
                    service_id = EXCLUDED.service_id,
                    state = EXCLUDED.state,
                    history = EXCLUDED.history,
                    metadata = EXCLUDED.metadata,
                    config = EXCLUDED.config,
                    status = EXCLUDED.status,
                    expires_at = EXCLUDED.expires_at,
                    updated_at = NOW()
                WHERE assistant.sessions.user_id = EXCLUDED.user_id
                  AND assistant.sessions.tenant_id IS NOT DISTINCT FROM EXCLUDED.tenant_id
            """,
                session.get("session_id"),
                session.get("service_id"),
                session.get("user_id"),
                session.get("tenant_id"),
                json.dumps(session.get("state", {})),
                json.dumps(session.get("history", [])),
                json.dumps(session.get("metadata", {})),
                json.dumps(session.get("config", {})),
                session.get("status", "active"),
                session.get("expires_at"),
            )

    async def append_session_history(
        self,
        session_id: str,
        events: list[dict[str, Any]],
    ) -> bool:
        """Incrementally append events to session history via JSONB concatenation without full overwrite."""
        if not self._pool or not events:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE assistant.sessions
                SET history = COALESCE(history, '[]'::jsonb) || $2::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
                """,
                session_id,
                json.dumps(events),
            )
            return "UPDATE 1" in str(result)

    async def create_session_if_absent(self, session: dict[str, Any]) -> bool:
        """Insert a session without overwriting an existing global id."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO assistant.sessions (
                    session_id, service_id, user_id, tenant_id,
                    state, history, metadata, config, status, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                -- A concurrent V1 create can collide on both the primary key
                -- and the runtime-owner scope constraint. Any conflict means
                -- this caller did not create the row; the API then performs
                -- an owner-checked read before adopting it.
                ON CONFLICT DO NOTHING
                RETURNING session_id
                """,
                session.get("session_id"),
                session.get("service_id"),
                session.get("user_id"),
                session.get("tenant_id"),
                json.dumps(session.get("state", {})),
                json.dumps(session.get("history", [])),
                json.dumps(session.get("metadata", {})),
                json.dumps(session.get("config", {})),
                session.get("status", "active"),
                session.get("expires_at"),
            )
            return row is not None

    async def get_session(self, session_id: str) -> dict[str, Any] | None:
        """获取会话（已删除的会话视为不存在）"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM assistant.sessions "
                "WHERE session_id = $1 AND status <> 'deleted'",
                session_id,
            )
            return self._row_to_dict(row) if row else None

    async def bind_agent_runtime_session(
        self,
        *,
        session_id: str,
        service_id: str,
        user_id: str,
        tenant_id: str,
        agent_id: str,
        agent_version_id: str | None,
        agent_draft_revision: int | None,
        publication_id: str | None,
        channel: str,
        runtime_fingerprint: str,
        agent_spec_hash: str,
        expires_at: datetime | None,
    ) -> dict[str, Any] | None:
        """Create or verify one immutable session-to-Agent runtime binding."""

        if not self._pool:
            return None
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                INSERT INTO assistant.sessions AS existing_session (
                    session_id, service_id, user_id, tenant_id,
                    state, history, metadata, config, status, expires_at,
                    agent_id, agent_version_id, agent_draft_revision,
                    publication_id, channel, runtime_fingerprint, agent_spec_hash
                ) VALUES (
                    $1, $2, $3, $4,
                    '{}'::jsonb, '[]'::jsonb, '{}'::jsonb, '{}'::jsonb,
                    'active', $5,
                    $6::uuid, $7::uuid, $8, $9::uuid, $10, $11, $12
                )
                ON CONFLICT (session_id) DO UPDATE SET
                    expires_at = GREATEST(existing_session.expires_at, EXCLUDED.expires_at),
                    updated_at = NOW()
                WHERE existing_session.user_id = EXCLUDED.user_id
                  AND existing_session.tenant_id = EXCLUDED.tenant_id
                  AND existing_session.service_id = EXCLUDED.service_id
                  AND existing_session.status = 'active'
                  AND existing_session.agent_id = EXCLUDED.agent_id
                  AND existing_session.agent_version_id IS NOT DISTINCT FROM EXCLUDED.agent_version_id
                  AND existing_session.agent_draft_revision IS NOT DISTINCT FROM EXCLUDED.agent_draft_revision
                  AND existing_session.publication_id IS NOT DISTINCT FROM EXCLUDED.publication_id
                  AND existing_session.channel = EXCLUDED.channel
                  AND existing_session.runtime_fingerprint = EXCLUDED.runtime_fingerprint
                  AND existing_session.agent_spec_hash = EXCLUDED.agent_spec_hash
                RETURNING *
                """,
                session_id,
                service_id,
                user_id,
                tenant_id,
                expires_at,
                agent_id,
                agent_version_id,
                agent_draft_revision,
                publication_id,
                channel,
                runtime_fingerprint,
                agent_spec_hash,
            )
        return self._row_to_dict(row) if row else None

    async def append_session_message(
        self,
        session_id: str,
        message: dict[str, Any],
        metadata_update: dict[str, Any] | None = None,
    ) -> bool:
        """
        原子追加消息到会话历史（避免竞态条件）

        使用 PostgreSQL 的 JSONB 操作原子地追加消息，而不是读取-修改-写入模式。

        Args:
            session_id: 会话 ID
            message: 消息字典 {role, content, timestamp, metadata}
            metadata_update: 可选的 metadata 更新（如自动标题）

        Returns:
            是否成功
        """
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # 使用 JSONB || 操作符原子追加消息
            if metadata_update:
                result = await conn.execute(
                    """
                    UPDATE assistant.sessions
                    SET history = history || $2::jsonb,
                        metadata = metadata || $3::jsonb,
                        updated_at = NOW()
                    WHERE session_id = $1
                """,
                    session_id,
                    json.dumps([message]),
                    json.dumps(metadata_update),
                )
            else:
                result = await conn.execute(
                    """
                    UPDATE assistant.sessions
                    SET history = history || $2::jsonb,
                        updated_at = NOW()
                    WHERE session_id = $1
                """,
                    session_id,
                    json.dumps([message]),
                )
            return result == "UPDATE 1"

    async def list_sessions(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str = "active",
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """获取会话列表"""
        if not self._pool:
            return []

        query = "SELECT * FROM assistant.sessions WHERE 1=1"
        params = []
        param_idx = 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if service_id:
            query += f" AND service_id = ${param_idx}"
            params.append(service_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        # Intentionally NOT filtering by expires_at: the 7-day TTL silently
        # dropped old sessions from the sidebar (looks like data loss to the
        # user). Hard expiry is handled by `cleanup_expired_sessions`.

        query += f" ORDER BY updated_at DESC LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def list_session_summaries(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_ids: list[str] | None = None,
        include_null_service_id: bool = False,
        status: str = "active",
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """Lightweight session list — excludes history/state, slims metadata to sidebar-only fields."""
        if not self._pool:
            return []

        # Project only sidebar-needed metadata keys; ignore polluted large fields
        # (some sessions have 4-7MB metadata from tool results / images)
        cols = (
            "session_id, service_id, user_id, tenant_id, "
            "jsonb_build_object("
            "  'title', metadata->'title', "
            "  'langgraph_thread_id', metadata->'langgraph_thread_id', "
            "  'folder', metadata->'folder', "
            "  'pinned', metadata->'pinned'"
            ") as metadata, "
            "status, expires_at, created_at, updated_at"
        )
        query = f"SELECT {cols} FROM assistant.sessions WHERE 1=1"
        params: list[Any] = []
        param_idx = 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if service_ids is not None:
            if include_null_service_id:
                query += f" AND (service_id = ANY(${param_idx}::text[]) OR service_id IS NULL OR service_id = '')"
            else:
                query += f" AND service_id = ANY(${param_idx}::text[])"
            params.append(service_ids)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        # Same policy as list_sessions: don't silently hide expired sessions.

        query += f" ORDER BY updated_at DESC LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def update_session_history(self, session_id: str, history: list[dict[str, Any]]) -> None:
        """更新会话历史"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE assistant.sessions SET history = $1, updated_at = NOW() WHERE session_id = $2",
                json.dumps(history),
                session_id,
            )

    async def update_session_state(self, session_id: str, state: dict[str, Any]) -> None:
        """更新会话状态"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE assistant.sessions SET state = $1, updated_at = NOW() WHERE session_id = $2",
                json.dumps(state),
                session_id,
            )

    async def update_session_metadata(self, session_id: str, metadata: dict[str, Any]) -> bool:
        """原子更新会话 metadata，避免覆盖 history。"""
        if not self._pool:
            return False
        if not metadata:
            return True
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE assistant.sessions
                SET metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
            """,
                session_id,
                json.dumps(metadata),
            )
            return result == "UPDATE 1"

    async def update_session_config(self, session_id: str, config: dict[str, Any]) -> bool:
        """原子更新会话 config，避免覆盖 history。"""
        if not self._pool:
            return False
        if not config:
            return True
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE assistant.sessions
                SET config = COALESCE(config, '{}'::jsonb) || $2::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
            """,
                session_id,
                json.dumps(config),
            )
            return result == "UPDATE 1"

    async def delete_session(self, session_id: str) -> bool:
        """删除会话。

        Agent Runtime 的线程 / 条目 / 运行 / 能力执行审计链全部以 RESTRICT 外键
        指向 ``assistant.sessions``：清理会话时 Runtime 只会给线程打删除标记而不
        删行，所以对跑过任何一轮对话的会话执行硬删除必然违反外键并向用户抛 500。
        这类会话改为墓碑标记——它在所有列表（均按 ``status='active'`` 过滤）和
        ``get_session`` 中消失，而审计链保持完整。
        """
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            try:
                result = await conn.execute(
                    "DELETE FROM assistant.sessions WHERE session_id = $1",
                    session_id,
                )
            except asyncpg.exceptions.ForeignKeyViolationError:
                result = await conn.execute(
                    "UPDATE assistant.sessions "
                    "SET status = 'deleted', updated_at = NOW() "
                    "WHERE session_id = $1 AND status <> 'deleted'",
                    session_id,
                )
                return result == "UPDATE 1"
            return result == "DELETE 1"

    async def cleanup_expired_sessions(self) -> int:
        """清理过期会话"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM assistant.sessions "
                "WHERE expires_at IS NOT NULL AND expires_at < NOW()"
            )
            # 解析结果获取删除行数
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    # =========================================================================
    # 异步任务表 (tasks)
    # =========================================================================

    async def save_task(self, task: dict[str, Any], *, result_format: dict[str, Any] | None = None) -> None:
        """Legacy task save; codec format is explicit internal bookkeeping."""
        if not self._pool:
            return
        await self.repos["tasks"].save(task, result_format=result_format)

    async def get_task(self, task_id: str) -> dict[str, Any] | None:
        """Read task JSON through the task-owned decoder."""
        if not self._pool:
            return None
        return await self.repos["tasks"].get(task_id)

    async def list_tasks(
        self, user_id: str | None = None, tenant_id: str | None = None,
        service_id: str | None = None, status: str | None = None,
        limit: int = 100, offset: int = 0,
    ) -> list[dict[str, Any]]:
        """Retain legacy filters, created-at ordering and pagination."""
        if not self._pool:
            return []
        return await self.repos["tasks"].list(
            user_id=user_id, tenant_id=tenant_id, service_id=service_id,
            status=status, limit=limit, offset=offset,
        )

    async def update_task_status(
        self, task_id: str, status: str, progress: float = None,
        result: Any = None, error: str = None,
        *, result_format: dict[str, Any] | None = None,
    ) -> None:
        """Legacy direct update; None result remains omission, not JSON null."""
        if not self._pool:
            return
        await self.repos["tasks"].update_status(
            task_id, status, progress=progress, result=result, error=error,
            result_format=result_format,
        )

    async def mark_callback_sent(self, task_id: str) -> None:
        """标记回调已发送"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE tasks SET callback_sent = TRUE, updated_at = NOW() WHERE task_id = $1",
                task_id,
            )

    async def get_pending_tasks(self, limit: int = 10) -> list[dict[str, Any]]:
        """Retain legacy priority/created-at ordering for pending work."""
        if not self._pool:
            return []
        return await self.repos["tasks"].get_pending_tasks(limit)

    # =========================================================================
    # API Key 表 (api_keys)
    # =========================================================================

    async def save_api_key(
        self,
        key_hash: str,
        name: str = None,
        description: str = None,
        tenant_id: str = None,
        user_id: str = None,
        roles: list[str] = None,
        permissions: list[str] = None,
        tier: str = "normal",
        rate_limit: dict = None,
        allowed_services: list[str] = None,
        allowed_models: list[str] = None,
        expires_at: datetime = None,
    ) -> int:
        """保存 API Key"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO api_keys (
                    key_hash, name, description, tenant_id, user_id,
                    roles, permissions, tier, rate_limit, allowed_services, allowed_models, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                ON CONFLICT (key_hash) DO UPDATE SET
                    name = EXCLUDED.name,
                    description = EXCLUDED.description,
                    tenant_id = EXCLUDED.tenant_id,
                    user_id = EXCLUDED.user_id,
                    roles = EXCLUDED.roles,
                    permissions = EXCLUDED.permissions,
                    tier = EXCLUDED.tier,
                    rate_limit = EXCLUDED.rate_limit,
                    allowed_services = EXCLUDED.allowed_services,
                    allowed_models = EXCLUDED.allowed_models,
                    expires_at = EXCLUDED.expires_at,
                    updated_at = NOW()
                RETURNING id
            """,
                key_hash,
                name,
                description,
                tenant_id,
                user_id,
                roles or ["user"],
                permissions or [],
                tier,
                json.dumps(rate_limit) if rate_limit else None,
                allowed_services or [],
                allowed_models or [],
                expires_at,
            )
            return row["id"] if row else 0

    async def get_api_key(self, key_hash: str) -> dict[str, Any] | None:
        """通过哈希获取 API Key"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM api_keys
                WHERE key_hash = $1 AND enabled = TRUE
                AND (expires_at IS NULL OR expires_at > NOW())
            """,
                key_hash,
            )
        if row:
            with contextlib.suppress(Exception):
                await self._track_api_key_usage(key_hash)
        return self._row_to_dict(row) if row else None

    async def list_api_keys(
        self, tenant_id: str | None = None, user_id: str | None = None, enabled: bool = None
    ) -> list[dict[str, Any]]:
        """获取 API Key 列表（不返回哈希）"""
        if not self._pool:
            return []

        query = """
            SELECT id, name, description, tenant_id, user_id, roles,
                   permissions, tier, rate_limit, allowed_services, allowed_models,
                   expires_at, enabled, last_used_at, use_count, created_at, updated_at
            FROM api_keys WHERE 1=1
        """
        params = []
        param_idx = 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if enabled is not None:
            query += f" AND enabled = ${param_idx}"
            params.append(enabled)
            param_idx += 1

        query += " ORDER BY created_at DESC"

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def disable_api_key(self, key_id: int) -> bool:
        """禁用 API Key"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "UPDATE api_keys SET enabled = FALSE, updated_at = NOW() WHERE id = $1", key_id
            )
            return result == "UPDATE 1"

    async def delete_api_key(self, key_id: int) -> bool:
        """删除 API Key"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute("DELETE FROM api_keys WHERE id = $1", key_id)
            return result == "DELETE 1"

    # =========================================================================
    # 用户表 (users)
    # =========================================================================

    async def save_user(self, user: dict[str, Any]) -> None:
        """保存或更新用户"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO users (
                    user_id, username, email, display_name, tenant_id,
                    tier, roles, permissions, quota_config, status, metadata
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ON CONFLICT (user_id) DO UPDATE SET
                    username = EXCLUDED.username,
                    email = EXCLUDED.email,
                    display_name = EXCLUDED.display_name,
                    tier = EXCLUDED.tier,
                    roles = EXCLUDED.roles,
                    permissions = EXCLUDED.permissions,
                    quota_config = EXCLUDED.quota_config,
                    status = EXCLUDED.status,
                    metadata = EXCLUDED.metadata,
                    updated_at = NOW()
            """,
                user.get("user_id"),
                user.get("username"),
                user.get("email"),
                user.get("display_name"),
                user.get("tenant_id", "default"),
                user.get("tier", "normal"),
                user.get("roles", ["user"]),
                user.get("permissions", []),
                json.dumps(user.get("quota_config", {})),
                user.get("status", "active"),
                json.dumps(user.get("metadata", {})),
            )

    async def get_user(self, user_id: str) -> dict[str, Any] | None:
        """获取用户"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM users WHERE user_id = $1", user_id)
            return self._row_to_dict(row) if row else None

    async def get_user_for_tenant(
        self, user_id: str, tenant_id: str
    ) -> dict[str, Any] | None:
        """Return a user only when it belongs to the caller's tenant."""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM users WHERE user_id = $1 AND tenant_id = $2",
                user_id,
                tenant_id,
            )
            return self._row_to_dict(row) if row else None

    async def list_users(
        self, tenant_id: str | None = None, status: str = "active", limit: int = 100
    ) -> list[dict[str, Any]]:
        """获取用户列表"""
        if not self._pool:
            return []

        query = "SELECT * FROM users WHERE 1=1"
        params = []
        param_idx = 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY created_at DESC LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def update_user_last_active(self, user_id: str) -> None:
        """更新用户最后活跃时间"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE users SET last_active_at = NOW() WHERE user_id = $1", user_id
            )

    # =========================================================================
    # 租户表 (tenants)
    # =========================================================================

    async def save_tenant(self, tenant: dict[str, Any]) -> None:
        """保存或更新租户"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO tenants (
                    tenant_id, name, description, tier, quota_config,
                    rate_limit, allowed_services, status, metadata
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (tenant_id) DO UPDATE SET
                    name = EXCLUDED.name,
                    description = EXCLUDED.description,
                    tier = EXCLUDED.tier,
                    quota_config = EXCLUDED.quota_config,
                    rate_limit = EXCLUDED.rate_limit,
                    allowed_services = EXCLUDED.allowed_services,
                    status = EXCLUDED.status,
                    metadata = EXCLUDED.metadata,
                    updated_at = NOW()
            """,
                tenant.get("tenant_id"),
                tenant.get("name"),
                tenant.get("description"),
                tenant.get("tier", "normal"),
                json.dumps(tenant.get("quota_config", {})),
                json.dumps(tenant.get("rate_limit")) if tenant.get("rate_limit") else None,
                tenant.get("allowed_services", []),
                tenant.get("status", "active"),
                json.dumps(tenant.get("metadata", {})),
            )

    async def get_tenant(self, tenant_id: str) -> dict[str, Any] | None:
        """获取租户"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM tenants WHERE tenant_id = $1", tenant_id)
            return self._row_to_dict(row) if row else None

    async def list_tenants(self, status: str = "active") -> list[dict[str, Any]]:
        """获取租户列表"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT * FROM tenants WHERE status = $1 ORDER BY created_at DESC", status
            )
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # 限流配置表 (rate_limit_config)
    # =========================================================================

    async def save_rate_limit(
        self,
        scope: str,
        scope_id: str,
        requests: int,
        window_seconds: int,
        burst: int = 0,
        strategy: str = "sliding_window",
        priority: int = 0,
    ) -> None:
        """保存限流配置"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO rate_limit_config (
                    scope, scope_id, requests, window_seconds, burst, strategy, priority
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (scope, scope_id) DO UPDATE SET
                    requests = EXCLUDED.requests,
                    window_seconds = EXCLUDED.window_seconds,
                    burst = EXCLUDED.burst,
                    strategy = EXCLUDED.strategy,
                    priority = EXCLUDED.priority,
                    updated_at = NOW()
            """,
                scope,
                scope_id or "",
                requests,
                window_seconds,
                burst,
                strategy,
                priority,
            )

    async def get_rate_limits(
        self, scope: str | None = None, enabled: bool = True
    ) -> list[dict[str, Any]]:
        """获取限流配置"""
        if not self._pool:
            return []

        query = """
            SELECT id, scope, scope_id, requests, window_seconds,
                   burst, strategy, enabled, priority, created_at, updated_at
            FROM rate_limit_config WHERE 1=1
        """
        params = []
        param_idx = 1

        if scope:
            query += f" AND scope = ${param_idx}"
            params.append(scope)
            param_idx += 1

        if enabled is not None:
            query += f" AND enabled = ${param_idx}"
            params.append(enabled)
            param_idx += 1

        query += " ORDER BY priority DESC, scope"

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def delete_rate_limit(self, scope: str, scope_id: str) -> bool:
        """删除限流配置"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM rate_limit_config WHERE scope = $1 AND scope_id = $2",
                scope,
                scope_id or "",
            )
            return result == "DELETE 1"

    # =========================================================================
    # RBAC 角色表 (rbac_roles)
    # =========================================================================

    async def get_rbac_roles(self) -> list[dict[str, Any]]:
        """获取所有角色"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch("SELECT * FROM rbac_roles ORDER BY is_system DESC, role_name")
            return [self._row_to_dict(row) for row in rows]

    async def get_role_permissions(self, role_name: str) -> list[str]:
        """获取角色权限"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT permissions FROM rbac_roles WHERE role_name = $1", role_name
            )
            return row["permissions"] if row else []

    async def save_role(
        self,
        role_name: str,
        permissions: list[str],
        description: str = None,
        is_system: bool = False,
    ) -> None:
        """保存角色"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO rbac_roles (role_name, permissions, description, is_system)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (role_name) DO UPDATE SET
                    permissions = EXCLUDED.permissions,
                    description = EXCLUDED.description,
                    updated_at = NOW()
            """,
                role_name,
                permissions,
                description,
                is_system,
            )

    # =========================================================================
    # 审计日志表 (audit_logs)
    # =========================================================================

    async def log_audit(
        self,
        event_type: str,
        action: str,
        status: str,
        user_id: str = None,
        tenant_id: str = None,
        ip_address: str = None,
        user_agent: str = None,
        resource_type: str = None,
        resource_id: str = None,
        request_summary: dict = None,
        response_summary: dict = None,
        error_message: str = None,
        duration_ms: int = None,
    ) -> None:
        """记录审计日志"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO audit_logs (
                    event_type, user_id, tenant_id, ip_address, user_agent,
                    resource_type, resource_id, action, request_summary,
                    response_summary, status, error_message, duration_ms
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            """,
                event_type,
                user_id,
                tenant_id,
                ip_address,
                user_agent,
                resource_type,
                resource_id,
                action,
                json.dumps(request_summary) if request_summary else None,
                json.dumps(response_summary) if response_summary else None,
                status,
                error_message,
                duration_ms,
            )

    async def query_audit_logs(
        self,
        event_type: str | None = None,
        user_id: str | None = None,
        tenant_id: str | None = None,
        start_time: datetime | None = None,
        end_time: datetime | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """查询审计日志"""
        if not self._pool:
            return []

        query = "SELECT * FROM audit_logs WHERE 1=1"
        params = []
        param_idx = 1

        if event_type:
            query += f" AND event_type = ${param_idx}"
            params.append(event_type)
            param_idx += 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if start_time:
            query += f" AND created_at >= ${param_idx}"
            params.append(start_time)
            param_idx += 1

        if end_time:
            query += f" AND created_at <= ${param_idx}"
            params.append(end_time)
            param_idx += 1

        query += f" ORDER BY created_at DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # 服务健康记录表 (service_health_records)
    # =========================================================================

    async def record_health_check(
        self,
        service_id: str,
        status: str,
        response_time_ms: int = None,
        details: dict = None,
        error_message: str = None,
    ) -> None:
        """记录健康检查结果"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO service_health_records (
                    service_id, status, response_time_ms, details, error_message
                ) VALUES ($1, $2, $3, $4, $5)
            """,
                service_id,
                status,
                response_time_ms,
                json.dumps(details or {}),
                error_message,
            )

    async def get_health_history(self, service_id: str, limit: int = 100) -> list[dict[str, Any]]:
        """获取服务健康历史"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM service_health_records
                WHERE service_id = $1
                ORDER BY checked_at DESC
                LIMIT $2
            """,
                service_id,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # 使用统计表 (usage_statistics)
    # =========================================================================

    async def update_usage_stats(
        self,
        dimension: str,
        dimension_id: str,
        period_type: str,
        period_start: datetime,
        request_count: int = 1,
        success_count: int = 0,
        error_count: int = 0,
        input_tokens: int = 0,
        output_tokens: int = 0,
        response_time_ms: int = None,
    ) -> None:
        """更新使用统计"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO usage_statistics (
                    dimension, dimension_id, period_type, period_start,
                    request_count, success_count, error_count,
                    input_tokens, output_tokens,
                    avg_response_time_ms, max_response_time_ms, min_response_time_ms
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $10)
                ON CONFLICT (dimension, dimension_id, period_type, period_start) DO UPDATE SET
                    request_count = usage_statistics.request_count + EXCLUDED.request_count,
                    success_count = usage_statistics.success_count + EXCLUDED.success_count,
                    error_count = usage_statistics.error_count + EXCLUDED.error_count,
                    input_tokens = usage_statistics.input_tokens + EXCLUDED.input_tokens,
                    output_tokens = usage_statistics.output_tokens + EXCLUDED.output_tokens,
                    avg_response_time_ms = CASE
                        WHEN $10 IS NOT NULL THEN
                            (COALESCE(usage_statistics.avg_response_time_ms, 0) * usage_statistics.request_count + $10)
                            / (usage_statistics.request_count + 1)
                        ELSE usage_statistics.avg_response_time_ms
                    END,
                    max_response_time_ms = GREATEST(COALESCE(usage_statistics.max_response_time_ms, 0), COALESCE($10, 0)),
                    min_response_time_ms = LEAST(COALESCE(usage_statistics.min_response_time_ms, 999999999), COALESCE($10, 999999999)),
                    updated_at = NOW()
            """,
                dimension,
                dimension_id,
                period_type,
                period_start,
                request_count,
                success_count,
                error_count,
                input_tokens,
                output_tokens,
                response_time_ms,
            )

    async def get_usage_stats(
        self,
        dimension: str,
        dimension_id: str,
        period_type: str,
        start_time: datetime,
        end_time: datetime,
    ) -> list[dict[str, Any]]:
        """获取使用统计"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM usage_statistics
                WHERE dimension = $1 AND dimension_id = $2 AND period_type = $3
                AND period_start >= $4 AND period_start <= $5
                ORDER BY period_start
            """,
                dimension,
                dimension_id,
                period_type,
                start_time,
                end_time,
            )
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # Security event daily aggregates (auth failures, rate limits)
    # =========================================================================

    async def record_security_event(
        self,
        tenant_id: str,
        user_id: str | None,
        service_id: str | None,
        event_type: str,
        event_date: date | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> None:
        """Record a security event into daily aggregates."""
        if not self._pool:
            return
        if event_date is None:
            event_date = datetime.utcnow().date()

        # Current aggregate table does not persist metadata columns.
        # Keep parameter for forward compatibility and structured callsites.
        _ = metadata

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO security_event_daily_aggregates (
                    tenant_id, user_id, service_id, event_type, date, event_count
                ) VALUES (
                    $1, $2, $3, $4, $5, 1
                )
                ON CONFLICT (tenant_id, COALESCE(user_id, ''), COALESCE(service_id, ''), event_type, date)
                DO UPDATE SET
                    event_count = security_event_daily_aggregates.event_count + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                tenant_id,
                user_id or None,
                service_id or None,
                event_type,
                event_date,
            )

    async def get_security_event_breakdown(
        self,
        tenant_id: str,
        dimension: str,
        event_type: str,
        start_date: date,
        end_date: date,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Get security event breakdown by dimension."""
        if not self._pool:
            return []

        # Whitelist of valid dimension -> column mappings (SQL injection prevention)
        dimension_mapping = {
            "user": "user_id",
            "service": "service_id",
        }
        if dimension not in dimension_mapping:
            return []  # Invalid dimension, return empty result
        dimension_column = dimension_mapping[dimension]

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                SELECT
                    {dimension_column} as dimension_value,
                    SUM(event_count) as total_events
                FROM security_event_daily_aggregates
                WHERE tenant_id = $1
                  AND date >= $2
                  AND date <= $3
                  AND event_type = $4
                  AND {dimension_column} IS NOT NULL
                GROUP BY {dimension_column}
                ORDER BY total_events DESC
                LIMIT $5
                """,
                tenant_id,
                start_date,
                end_date,
                event_type,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_security_event_timeseries(
        self,
        tenant_id: str,
        event_type: str,
        start_date: date,
        end_date: date,
        user_id: str | None = None,
        service_id: str | None = None,
    ) -> list[dict[str, Any]]:
        """Get daily time series for security events."""
        if not self._pool:
            return []

        async with self._pool.acquire() as conn:
            query = """
                SELECT
                    date,
                    SUM(event_count) as total_events
                FROM security_event_daily_aggregates
                WHERE tenant_id = $1
                  AND date >= $2
                  AND date <= $3
                  AND event_type = $4
            """
            params: list[Any] = [tenant_id, start_date, end_date, event_type]

            if user_id:
                query += " AND user_id = $" + str(len(params) + 1)
                params.append(user_id)
            if service_id:
                query += " AND service_id = $" + str(len(params) + 1)
                params.append(service_id)

            query += " GROUP BY date ORDER BY date"

            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def get_security_event_last_ingested_at(
        self,
        tenant_id: str,
        event_type: str,
        start_date: date,
        end_date: date,
    ) -> datetime | None:
        """Get last ingestion time for security event aggregates."""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT MAX(updated_at) AS last_ingested
                FROM security_event_daily_aggregates
                WHERE tenant_id = $1
                  AND event_type = $2
                  AND date >= $3
                  AND date <= $4
                """,
                tenant_id,
                event_type,
                start_date,
                end_date,
            )
            return row["last_ingested"] if row else None

    async def get_usage_last_ingested_at(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        granularity: str = "day",
    ) -> datetime | None:
        """Get last ingestion time for usage aggregates."""
        if not self._pool:
            return None

        table = "usage_hourly_aggregates" if granularity == "hour" else "usage_daily_aggregates"
        query = f"""
            SELECT MAX(updated_at) AS last_ingested
            FROM {table}
            WHERE tenant_id = $1
        """
        params: list[Any] = [tenant_id]

        if start_date:
            query += f" AND date >= ${len(params) + 1}"
            params.append(start_date)
        if end_date:
            query += f" AND date <= ${len(params) + 1}"
            params.append(end_date)

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, *params)
            return row["last_ingested"] if row else None

    # =========================================================================
    # LangGraph Thread 映射表 (langgraph_threads)
    # =========================================================================

    async def save_langgraph_thread(
        self,
        thread_id: str,
        user_id: str,
        tenant_id: str = "",
        assistant_id: str = None,
        metadata: dict = None,
        is_anonymous: bool = False,
        expires_at: datetime = None,
    ) -> None:
        """保存 LangGraph Thread 映射"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO langgraph_threads (
                    thread_id, user_id, tenant_id, assistant_id,
                    metadata, is_anonymous, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (thread_id) DO UPDATE SET
                    assistant_id = EXCLUDED.assistant_id,
                    metadata = EXCLUDED.metadata,
                    expires_at = EXCLUDED.expires_at,
                    last_accessed_at = NOW(),
                    updated_at = NOW()
            """,
                thread_id,
                user_id,
                tenant_id or "",
                assistant_id,
                json.dumps(metadata or {}),
                is_anonymous,
                expires_at,
            )

    async def get_langgraph_thread(self, thread_id: str) -> dict[str, Any] | None:
        """获取 LangGraph Thread"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM langgraph_threads WHERE thread_id = $1 AND status = 'active'",
                thread_id,
            )
            if row:
                await conn.execute(
                    "UPDATE langgraph_threads SET last_accessed_at = NOW() WHERE thread_id = $1",
                    thread_id,
                )
            return self._row_to_dict(row) if row else None

    async def list_user_threads(
        self, user_id: str, tenant_id: str | None = None, limit: int = 100
    ) -> list[dict[str, Any]]:
        """获取用户的 LangGraph Threads"""
        if not self._pool:
            return []

        query = "SELECT * FROM langgraph_threads WHERE user_id = $1 AND status = 'active'"
        params = [user_id]
        param_idx = 2

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        query += f" ORDER BY last_accessed_at DESC NULLS LAST LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # 语义缓存表 (semantic_cache)
    # =========================================================================

    async def save_cache(
        self,
        service_id: str,
        input_hash: str,
        output_text: str,
        input_text: str = None,
        output_data: dict = None,
        metadata: dict = None,
        expires_at: datetime = None,
    ) -> None:
        """保存语义缓存"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO semantic_cache (
                    service_id, input_hash, input_text, output_text,
                    output_data, metadata, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (service_id, input_hash) DO UPDATE SET
                    output_text = EXCLUDED.output_text,
                    output_data = EXCLUDED.output_data,
                    metadata = EXCLUDED.metadata,
                    expires_at = EXCLUDED.expires_at,
                    hit_count = semantic_cache.hit_count + 1,
                    last_hit_at = NOW()
            """,
                service_id,
                input_hash,
                input_text,
                output_text,
                json.dumps(output_data) if output_data else None,
                json.dumps(metadata or {}),
                expires_at,
            )

    async def get_cache(self, service_id: str, input_hash: str) -> dict[str, Any] | None:
        """获取语义缓存"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM semantic_cache
                WHERE service_id = $1 AND input_hash = $2
                AND (expires_at IS NULL OR expires_at > NOW())
            """,
                service_id,
                input_hash,
            )
            if row:
                # 更新命中统计
                await conn.execute(
                    """
                    UPDATE semantic_cache SET
                        hit_count = hit_count + 1,
                        last_hit_at = NOW()
                    WHERE service_id = $1 AND input_hash = $2
                """,
                    service_id,
                    input_hash,
                )
            return self._row_to_dict(row) if row else None

    async def cleanup_expired_cache(self) -> int:
        """清理过期缓存"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM semantic_cache WHERE expires_at IS NOT NULL AND expires_at < NOW()"
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    # =========================================================================
    # 鉴权配置表 (auth_config)
    # =========================================================================

    async def get_auth_config(self, config_type: str) -> dict[str, Any] | None:
        """获取鉴权配置"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM auth_config WHERE config_type = $1 AND enabled = TRUE", config_type
            )
            return self._row_to_dict(row) if row else None

    async def save_auth_config(
        self, config_type: str, config: dict[str, Any], enabled: bool = True
    ) -> None:
        """保存鉴权配置"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO auth_config (config_type, config, enabled)
                VALUES ($1, $2, $3)
                ON CONFLICT (config_type) DO UPDATE SET
                    config = EXCLUDED.config,
                    enabled = EXCLUDED.enabled,
                    updated_at = NOW()
            """,
                config_type,
                json.dumps(config),
                enabled,
            )

    # =========================================================================
    # 用户认证增强方法 (Account Management)
    # =========================================================================

    async def get_user_by_email(self, email: str) -> dict[str, Any] | None:
        """通过邮箱获取用户"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM users WHERE LOWER(email) = LOWER($1)", email)
            return self._row_to_dict(row) if row else None

    async def save_user_with_password(self, user: dict[str, Any]) -> None:
        """保存或更新用户（包含密码字段）"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO users (
                    user_id, username, email, display_name, department, tenant_id,
                    tier, roles, permissions, quota_config, status,
                    password_hash, force_password_change, email_verified,
                    created_by, metadata
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                ON CONFLICT (user_id) DO UPDATE SET
                    username = EXCLUDED.username,
                    email = EXCLUDED.email,
                    display_name = EXCLUDED.display_name,
                    department = EXCLUDED.department,
                    tier = EXCLUDED.tier,
                    roles = EXCLUDED.roles,
                    permissions = EXCLUDED.permissions,
                    quota_config = EXCLUDED.quota_config,
                    status = EXCLUDED.status,
                    updated_at = NOW()
            """,
                user.get("user_id"),
                user.get("username") or user.get("email", "").split("@")[0],
                user.get("email"),
                user.get("display_name"),
                user.get("department"),
                user.get("tenant_id", "default"),
                user.get("tier", "normal"),
                user.get("roles", ["user"]),
                user.get("permissions", []),
                json.dumps(user.get("quota_config", {})),
                user.get("status", "active"),
                user.get("password_hash"),
                user.get("force_password_change", True),
                user.get("email_verified", False),
                user.get("created_by"),
                json.dumps(user.get("metadata", {})),
            )

    async def update_user_password(self, user_id: str, password_hash: str) -> None:
        """更新用户密码并清除强制修改标记"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE users SET
                    password_hash = $1,
                    force_password_change = FALSE,
                    password_changed_at = NOW(),
                    updated_at = NOW()
                WHERE user_id = $2
            """,
                password_hash,
                user_id,
            )

    async def reset_user_password(self, user_id: str, password_hash: str) -> None:
        """重置用户密码为默认值，需强制修改"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE users SET
                    password_hash = $1,
                    force_password_change = TRUE,
                    password_changed_at = NULL,
                    login_attempts = 0,
                    locked_until = NULL,
                    updated_at = NOW()
                WHERE user_id = $2
            """,
                password_hash,
                user_id,
            )

    async def reset_user_password_for_tenant(
        self, user_id: str, tenant_id: str, password_hash: str
    ) -> bool:
        """Reset a password only for a user in the specified tenant."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE users SET
                    password_hash = $1,
                    force_password_change = TRUE,
                    password_changed_at = NULL,
                    login_attempts = 0,
                    locked_until = NULL,
                    updated_at = NOW()
                WHERE user_id = $2 AND tenant_id = $3
                """,
                password_hash,
                user_id,
                tenant_id,
            )
        return result == "UPDATE 1"

    async def increment_login_attempts(self, user_id: str) -> None:
        """增加登录失败计数"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE users SET login_attempts = COALESCE(login_attempts, 0) + 1 WHERE user_id = $1",
                user_id,
            )

    async def reset_login_attempts(self, user_id: str) -> None:
        """重置登录失败计数"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE users SET login_attempts = 0, locked_until = NULL WHERE user_id = $1",
                user_id,
            )

    async def lock_user_account(self, user_id: str, minutes: int = 30) -> None:
        """锁定用户账户"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                f"""
                UPDATE users SET
                    locked_until = NOW() + INTERVAL '{minutes} minutes',
                    updated_at = NOW()
                WHERE user_id = $1
            """,
                user_id,
            )

    async def update_last_login(self, user_id: str, ip_address: str) -> None:
        """更新最后登录信息"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE users SET
                    last_login_at = NOW(),
                    last_login_ip = $1,
                    last_active_at = NOW()
                WHERE user_id = $2
            """,
                ip_address,
                user_id,
            )

    async def log_login_audit(
        self,
        user_id: str | None,
        email: str | None,
        action: str,
        ip_address: str,
        user_agent: str,
        details: dict[str, Any],
    ) -> None:
        """记录登录审计日志"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO login_audit (user_id, email, action, ip_address, user_agent, details)
                VALUES ($1, $2, $3, $4, $5, $6)
            """,
                user_id,
                email,
                action,
                ip_address,
                user_agent,
                json.dumps(details),
            )

    async def get_user_permissions(
        self, user_id: str, *, require_current: bool = False
    ) -> list[str]:
        """Resolve role and direct grants using the existing staged fallback policy.

        Sensitive admission can require uncached, fail-closed reads. The default
        retains legacy caching and missing-schema fallback behavior.
        """
        if not self._pool:
            if require_current:
                raise RuntimeError("Current permission storage is unavailable")
            return []
        cached = None if require_current else await self._get_cached_permissions(user_id)
        if cached is not None:
            return cached
        async with self._pool.acquire() as conn:
            permissions_set: set = set()

            # 1. 从 role_permissions 表获取角色权限
            try:
                rows = await conn.fetch(
                    """
                    SELECT DISTINCT rp.permission_code
                    FROM user_roles ur
                    JOIN role_permissions rp ON ur.role_name = rp.role_name
                    WHERE ur.user_id = $1
                      AND (ur.expires_at IS NULL OR ur.expires_at > NOW())
                """,
                    user_id,
                )
                for row in rows:
                    permissions_set.add(row["permission_code"])
            except asyncpg.PostgresError:
                if require_current:
                    raise
                pass

            # 2. 如果 role_permissions 为空，回退到 rbac_roles 的 permissions 数组
            if not permissions_set:
                try:
                    rows = await conn.fetch(
                        """
                        SELECT DISTINCT unnest(rr.permissions) as permission_code
                        FROM user_roles ur
                        JOIN rbac_roles rr ON ur.role_name = rr.role_name
                        WHERE ur.user_id = $1
                          AND (ur.expires_at IS NULL OR ur.expires_at > NOW())
                    """,
                        user_id,
                    )
                    for row in rows:
                        permissions_set.add(row["permission_code"])
                except asyncpg.PostgresError:
                    if require_current:
                        raise
                    pass

            # 3. 回退到用户自身的 permissions 字段/roles 映射
            if not permissions_set:
                user_row = await conn.fetchrow(
                    "SELECT roles, permissions FROM users WHERE user_id = $1", user_id
                )
                if user_row:
                    permissions_set.update(user_row["permissions"] or [])
                    roles = user_row["roles"] or []
                    if roles:
                        try:
                            role_rows = await conn.fetch(
                                "SELECT permissions FROM rbac_roles WHERE role_name = ANY($1)",
                                roles,
                            )
                            for role_row in role_rows:
                                role_perms = role_row["permissions"] or []
                                permissions_set.update(role_perms)
                        except asyncpg.PostgresError:
                            if require_current:
                                raise
                            pass

            # 4. 获取用户额外权限（直接分配）
            try:
                extra_rows = await conn.fetch(
                    """
                    SELECT permission_code
                    FROM user_permissions
                    WHERE user_id = $1
                      AND (expires_at IS NULL OR expires_at > NOW())
                """,
                    user_id,
                )
                for row in extra_rows:
                    permissions_set.add(row["permission_code"])
            except asyncpg.PostgresError:
                if require_current:
                    raise
                # user_permissions 表可能不存在
                pass

            permissions = list(permissions_set)
            if not require_current:
                await self._set_cached_permissions(user_id, permissions)
            return permissions

    async def list_users_paginated(
        self,
        status: str | None = None,
        search: str | None = None,
        tenant_id: str | None = None,
        limit: int = 20,
        offset: int = 0,
    ) -> tuple:
        """分页获取用户列表"""
        if not self._pool:
            return [], 0

        query = "SELECT * FROM users WHERE 1=1"
        count_query = "SELECT COUNT(*) FROM users WHERE 1=1"
        params = []
        param_idx = 1

        if status:
            query += f" AND status = ${param_idx}"
            count_query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            count_query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if search:
            query += f" AND (email ILIKE ${param_idx} OR display_name ILIKE ${param_idx} OR username ILIKE ${param_idx})"
            count_query += f" AND (email ILIKE ${param_idx} OR display_name ILIKE ${param_idx} OR username ILIKE ${param_idx})"
            params.append(f"%{search}%")
            param_idx += 1

        async with self._pool.acquire() as conn:
            total = await conn.fetchval(count_query, *params)

            query += f" ORDER BY created_at DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
            params.extend([limit, offset])

            rows = await conn.fetch(query, *params)
            users = [self._row_to_dict(row) for row in rows]

            return users, total or 0

    async def update_user(self, user_id: str, updates: dict[str, Any]) -> None:
        """更新用户字段"""
        if not self._pool or not updates:
            return

        allowed_fields = {
            "display_name",
            "username",
            "department",
            "tier",
            "roles",
            "permissions",
            "quota_config",
            "status",
            "metadata",
            "email_verified",
        }
        filtered = {k: v for k, v in updates.items() if k in allowed_fields}
        if not filtered:
            return

        set_clauses = ["updated_at = NOW()"]
        params = []
        param_idx = 1

        for key, value in filtered.items():
            if key in ("quota_config", "metadata") and isinstance(value, dict):
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(json.dumps(value))
            else:
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(value)
            param_idx += 1

        params.append(user_id)
        query = f"UPDATE users SET {', '.join(set_clauses)} WHERE user_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def update_user_for_tenant(
        self,
        user_id: str,
        tenant_id: str,
        updates: dict[str, Any],
        *,
        roles: list[str] | None = None,
        extra_permissions: list[str] | None = None,
        granted_by: str | None = None,
    ) -> bool:
        """Atomically update a user and optional access grants within one tenant.

        The user-management API must not authorize a target in Python and then
        issue unscoped writes.  Locking the tenant-qualified row before each
        dependent-table mutation keeps role and direct-permission changes tied
        to the same tenant identity.
        """
        if not self._pool:
            return False

        allowed_fields = {
            "display_name",
            "username",
            "department",
            "tier",
            "roles",
            "permissions",
            "quota_config",
            "status",
            "metadata",
            "email_verified",
        }
        filtered = {key: value for key, value in updates.items() if key in allowed_fields}

        async with self._pool.acquire() as conn, conn.transaction():
            exists = await conn.fetchval(
                "SELECT 1 FROM users WHERE user_id = $1 AND tenant_id = $2 FOR UPDATE",
                user_id,
                tenant_id,
            )
            if not exists:
                return False

            if filtered:
                set_clauses = ["updated_at = NOW()"]
                params: list[Any] = []
                param_idx = 1

                for key, value in filtered.items():
                    if key in ("quota_config", "metadata") and isinstance(value, dict):
                        set_clauses.append(f"{key} = ${param_idx}")
                        params.append(json.dumps(value))
                    else:
                        set_clauses.append(f"{key} = ${param_idx}")
                        params.append(value)
                    param_idx += 1

                params.extend([user_id, tenant_id])
                await conn.execute(
                    f"UPDATE users SET {', '.join(set_clauses)} "
                    f"WHERE user_id = ${param_idx} AND tenant_id = ${param_idx + 1}",
                    *params,
                )

            if roles is not None:
                await conn.execute("DELETE FROM user_roles WHERE user_id = $1", user_id)
                for role in roles:
                    await conn.execute(
                        """
                        INSERT INTO user_roles (user_id, role_name, granted_by)
                        VALUES ($1, $2, $3)
                        """,
                        user_id,
                        role,
                        granted_by,
                    )
                await conn.execute(
                    "UPDATE users SET roles = $1, updated_at = NOW() "
                    "WHERE user_id = $2 AND tenant_id = $3",
                    roles,
                    user_id,
                    tenant_id,
                )

            if extra_permissions is not None:
                await conn.execute("DELETE FROM user_permissions WHERE user_id = $1", user_id)
                for permission in extra_permissions:
                    await conn.execute(
                        """
                        INSERT INTO user_permissions (user_id, permission_code, granted_by)
                        VALUES ($1, $2, $3)
                        """,
                        user_id,
                        permission,
                        granted_by,
                    )

        if roles is not None or extra_permissions is not None:
            await self._invalidate_permission_cache(user_id)
        return True

    async def delete_user(self, user_id: str) -> bool:
        """删除用户"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # 先删除 user_roles
            await conn.execute("DELETE FROM user_roles WHERE user_id = $1", user_id)
            # 再删除用户
            result = await conn.execute("DELETE FROM users WHERE user_id = $1", user_id)
            return result == "DELETE 1"

    async def delete_user_for_tenant(self, user_id: str, tenant_id: str) -> bool:
        """Delete a user and dependent grants only when it belongs to a tenant."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn, conn.transaction():
            exists = await conn.fetchval(
                "SELECT 1 FROM users WHERE user_id = $1 AND tenant_id = $2 FOR UPDATE",
                user_id,
                tenant_id,
            )
            if not exists:
                return False
            await conn.execute("DELETE FROM user_roles WHERE user_id = $1", user_id)
            await conn.execute("DELETE FROM user_permissions WHERE user_id = $1", user_id)
            result = await conn.execute(
                "DELETE FROM users WHERE user_id = $1 AND tenant_id = $2",
                user_id,
                tenant_id,
            )

        deleted = result == "DELETE 1"
        if deleted:
            await self._invalidate_permission_cache(user_id)
        return deleted

    async def assign_user_role(self, user_id: str, role_name: str, granted_by: str) -> None:
        """为用户分配角色"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO user_roles (user_id, role_name, granted_by)
                VALUES ($1, $2, $3)
                ON CONFLICT (user_id, role_name) DO UPDATE SET
                    granted_at = NOW(),
                    granted_by = EXCLUDED.granted_by
            """,
                user_id,
                role_name,
                granted_by,
            )
        await self._invalidate_permission_cache(user_id)

    async def remove_user_role(self, user_id: str, role_name: str) -> bool:
        """移除用户角色"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM user_roles WHERE user_id = $1 AND role_name = $2", user_id, role_name
            )
        await self._invalidate_permission_cache(user_id)
        return result == "DELETE 1"

    async def update_user_roles(self, user_id: str, roles: list[str], granted_by: str) -> None:
        """更新用户的所有角色"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            # 先删除现有角色
            await conn.execute("DELETE FROM user_roles WHERE user_id = $1", user_id)
            # 插入新角色
            for role in roles:
                await conn.execute(
                    """
                    INSERT INTO user_roles (user_id, role_name, granted_by)
                    VALUES ($1, $2, $3)
                """,
                    user_id,
                    role,
                    granted_by,
                )
            # 同时更新 users 表的 roles 字段
            await conn.execute(
                "UPDATE users SET roles = $1, updated_at = NOW() WHERE user_id = $2", roles, user_id
            )
        await self._invalidate_permission_cache(user_id)

    async def get_user_roles(self, user_id: str) -> list[str]:
        """获取用户的所有角色"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT role_name FROM user_roles
                WHERE user_id = $1
                  AND (expires_at IS NULL OR expires_at > NOW())
                ORDER BY granted_at
            """,
                user_id,
            )
            return [row["role_name"] for row in rows]

    # =========================================================================
    # 角色和权限管理方法
    # =========================================================================

    async def list_roles(self) -> list[dict[str, Any]]:
        """获取所有角色"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch("SELECT * FROM rbac_roles ORDER BY is_system DESC, role_name")
            return [self._row_to_dict(row) for row in rows]

    async def get_role(self, role_name: str) -> dict[str, Any] | None:
        """获取角色详情"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM rbac_roles WHERE role_name = $1", role_name)
            return self._row_to_dict(row) if row else None

    async def create_role(
        self, role_name: str, description: str | None, permissions: list[str]
    ) -> None:
        """创建新角色"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO rbac_roles (role_name, description, permissions, is_system)
                VALUES ($1, $2, $3, FALSE)
            """,
                role_name,
                description,
                permissions,
            )

            # 同时插入 role_permissions
            for perm in permissions:
                await conn.execute(
                    """
                    INSERT INTO role_permissions (role_name, permission_code)
                    VALUES ($1, $2)
                    ON CONFLICT DO NOTHING
                """,
                    role_name,
                    perm,
                )
        await self._invalidate_permission_cache()

    async def update_role(
        self, role_name: str, description: str | None, permissions: list[str] | None
    ) -> None:
        """更新角色"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            if description is not None and permissions is not None:
                await conn.execute(
                    """
                    UPDATE rbac_roles SET
                        description = $1,
                        permissions = $2,
                        updated_at = NOW()
                    WHERE role_name = $3
                """,
                    description,
                    permissions,
                    role_name,
                )
            elif description is not None:
                await conn.execute(
                    """
                    UPDATE rbac_roles SET
                        description = $1,
                        updated_at = NOW()
                    WHERE role_name = $2
                """,
                    description,
                    role_name,
                )
            elif permissions is not None:
                await conn.execute(
                    """
                    UPDATE rbac_roles SET
                        permissions = $1,
                        updated_at = NOW()
                    WHERE role_name = $2
                """,
                    permissions,
                    role_name,
                )

            # 如果更新了权限，同步 role_permissions 表
            if permissions is not None:
                await conn.execute("DELETE FROM role_permissions WHERE role_name = $1", role_name)
                for perm in permissions:
                    await conn.execute(
                        """
                        INSERT INTO role_permissions (role_name, permission_code)
                        VALUES ($1, $2)
                        ON CONFLICT DO NOTHING
                    """,
                        role_name,
                        perm,
                    )
        await self._invalidate_permission_cache()

    async def delete_role(self, role_name: str) -> bool:
        """删除角色"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # 先删除 role_permissions
            await conn.execute("DELETE FROM role_permissions WHERE role_name = $1", role_name)
            # 删除 user_roles 中的引用
            await conn.execute("DELETE FROM user_roles WHERE role_name = $1", role_name)
            # 删除角色
            result = await conn.execute(
                "DELETE FROM rbac_roles WHERE role_name = $1 AND is_system = FALSE", role_name
            )
            await self._invalidate_permission_cache()
            return result == "DELETE 1"

    async def list_permissions(self, category: str | None = None) -> list[dict[str, Any]]:
        """获取所有权限定义"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            if category:
                rows = await conn.fetch(
                    "SELECT * FROM permissions WHERE category = $1 ORDER BY permission_code",
                    category,
                )
            else:
                rows = await conn.fetch(
                    "SELECT * FROM permissions ORDER BY category, permission_code"
                )
            return [self._row_to_dict(row) for row in rows]

    async def get_tenant_role_user_count(self, role_name: str, tenant_id: str) -> int:
        """Count current memberships within one tenant; not a global deletion guard."""
        if not tenant_id.strip():
            raise ValueError("Tenant scope is required")
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            count = await conn.fetchval(
                """
                SELECT COUNT(*) FROM users u
                JOIN user_roles ur ON u.user_id = ur.user_id
                WHERE ur.role_name = $1 AND u.tenant_id = $2
                  AND (ur.expires_at IS NULL OR ur.expires_at > clock_timestamp())
                """,
                role_name,
                tenant_id,
            )
            return count or 0

    async def get_tenant_users_by_role(
        self, role_name: str, tenant_id: str
    ) -> list[dict[str, Any]]:
        """List current memberships only in the caller's explicit tenant."""
        if not tenant_id.strip():
            raise ValueError("Tenant scope is required")
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT u.user_id, u.email, u.display_name, u.status, ur.granted_at
                FROM users u
                JOIN user_roles ur ON u.user_id = ur.user_id
                WHERE ur.role_name = $1 AND u.tenant_id = $2
                  AND (ur.expires_at IS NULL OR ur.expires_at > clock_timestamp())
                ORDER BY ur.granted_at DESC, u.user_id
                """,
                role_name,
                tenant_id,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_role_user_count(self, role_name: str) -> int:
        """获取角色的用户数量"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            count = await conn.fetchval(
                "SELECT COUNT(*) FROM user_roles WHERE role_name = $1", role_name
            )
            return count or 0

    async def get_users_by_role(self, role_name: str) -> list[dict[str, Any]]:
        """获取拥有指定角色的所有用户"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT u.user_id, u.email, u.display_name, u.status, ur.granted_at
                FROM users u
                JOIN user_roles ur ON u.user_id = ur.user_id
                WHERE ur.role_name = $1
                ORDER BY ur.granted_at DESC
            """,
                role_name,
            )
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # 用户额外权限管理 (User Extra Permissions)
    # =========================================================================

    async def get_user_extra_permissions(self, user_id: str) -> list[dict[str, Any]]:
        """获取用户的额外权限（直接分配，非角色）"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            try:
                rows = await conn.fetch(
                    """
                    SELECT permission_code, granted_by, granted_at, expires_at, note
                    FROM user_permissions
                    WHERE user_id = $1
                      AND (expires_at IS NULL OR expires_at > NOW())
                    ORDER BY granted_at DESC
                """,
                    user_id,
                )
                return [self._row_to_dict(row) for row in rows]
            except asyncpg.PostgresError:
                # Table might not exist yet
                return []

    async def add_user_extra_permission(
        self,
        user_id: str,
        permission_code: str,
        granted_by: str,
        note: str | None = None,
        expires_at: datetime | None = None,
    ) -> bool:
        """给用户添加额外权限"""
        if not self._pool:
            return False
        await self._invalidate_permission_cache(user_id)
        async with self._pool.acquire() as conn:
            try:
                await conn.execute(
                    """
                    INSERT INTO user_permissions (user_id, permission_code, granted_by, note, expires_at)
                    VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT (user_id, permission_code) DO UPDATE SET
                        granted_by = EXCLUDED.granted_by,
                        granted_at = NOW(),
                        note = EXCLUDED.note,
                        expires_at = EXCLUDED.expires_at
                """,
                    user_id,
                    permission_code,
                    granted_by,
                    note,
                    expires_at,
                )
                return True
            except asyncpg.PostgresError:
                return False

    async def remove_user_extra_permission(self, user_id: str, permission_code: str) -> bool:
        """移除用户的额外权限"""
        if not self._pool:
            return False
        await self._invalidate_permission_cache(user_id)
        async with self._pool.acquire() as conn:
            try:
                result = await conn.execute(
                    """
                    DELETE FROM user_permissions
                    WHERE user_id = $1 AND permission_code = $2
                """,
                    user_id,
                    permission_code,
                )
                return "DELETE" in result
            except asyncpg.PostgresError:
                return False

    async def update_user_extra_permissions(
        self, user_id: str, permissions: list[str], granted_by: str
    ) -> None:
        """更新用户的额外权限（替换所有）"""
        if not self._pool:
            return
        await self._invalidate_permission_cache(user_id)
        async with self._pool.acquire() as conn, conn.transaction():
            # 删除现有的额外权限
            await conn.execute("DELETE FROM user_permissions WHERE user_id = $1", user_id)
            # 添加新的额外权限
            for perm in permissions:
                await conn.execute(
                    """
                        INSERT INTO user_permissions (user_id, permission_code, granted_by)
                        VALUES ($1, $2, $3)
                    """,
                    user_id,
                    perm,
                    granted_by,
                )

    # =========================================================================
    # 辅助方法
    # =========================================================================

    def _row_to_dict(self, row) -> dict[str, Any]:
        """将数据库行转换为字典，正确处理 JSON 和 datetime 字段"""
        if not row:
            return {}
        result = dict(row)

        # JSON 字段列表 - 需要解析为 Python 对象（字典类型）
        json_dict_fields = {"metadata", "embedding_config", "index_config", "result", "config"}

        # JSON 字段列表 - 需要解析为 Python 对象（列表类型）
        json_list_fields = {
            "roles",
            "keywords",
            "include_patterns",
            "exclude_patterns",
            "labels",
            "events",
            "history",
        }

        for key, value in result.items():
            # 处理 datetime 类型
            if isinstance(value, datetime):
                result[key] = value.isoformat()
            # 处理 JSON 字典字段
            elif key in json_dict_fields and value is not None:
                if isinstance(value, str):
                    try:
                        result[key] = json.loads(value)
                    except (json.JSONDecodeError, TypeError):
                        result[key] = {}
                elif not isinstance(value, dict):
                    result[key] = {}
            # 处理 JSON 列表字段
            elif key in json_list_fields and value is not None:
                if isinstance(value, str):
                    try:
                        result[key] = json.loads(value)
                    except (json.JSONDecodeError, TypeError):
                        result[key] = []
                elif not isinstance(value, list):
                    result[key] = []
        return result

    # =========================================================================
    # Memory Storage Methods (Session and User Memory)
    # =========================================================================

    async def store_session_memory(
        self,
        tenant_id: str,
        session_id: str,
        key: str,
        value: Any,
        metadata: dict | None = None,
    ) -> None:
        """
        Store or update a session memory entry.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            session_id: Session ID the memory belongs to
            key: Unique key for the memory entry within the session
            value: Value to store (any JSON-serializable type)
            metadata: Optional metadata about the stored value
        """
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO session_memory (tenant_id, session_id, key, value, metadata)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (tenant_id, session_id, key)
                DO UPDATE SET value = $4, metadata = COALESCE($5, session_memory.metadata)
                """,
                tenant_id,
                session_id,
                key,
                json.dumps(value),
                json.dumps(metadata) if metadata else None,
            )

    async def get_session_memory(
        self,
        tenant_id: str,
        session_id: str,
        key: str,
    ) -> Any | None:
        """
        Retrieve a session memory entry.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            session_id: Session ID the memory belongs to
            key: Key to retrieve

        Returns:
            The stored value if found, None otherwise
        """
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT value FROM session_memory
                WHERE tenant_id = $1 AND session_id = $2 AND key = $3
                """,
                tenant_id,
                session_id,
                key,
            )
            if row:
                value = row["value"]
                return json.loads(value) if isinstance(value, str) else value
            return None

    async def search_session_memory(
        self,
        tenant_id: str,
        session_id: str,
        query: str,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        """
        Search session memory by key or value content.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            session_id: Session ID to search within
            query: Search query string (case-insensitive)
            limit: Maximum number of results to return

        Returns:
            List of matching memory entries with key, value, metadata, and timestamps
        """
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT key, value, metadata, created_at, updated_at
                FROM session_memory
                WHERE tenant_id = $1 AND session_id = $2
                  AND (key ILIKE $3 OR value::text ILIKE $3)
                ORDER BY updated_at DESC
                LIMIT $4
                """,
                tenant_id,
                session_id,
                f"%{query}%",
                limit,
            )
            return [self._memory_row_to_dict(row) for row in rows]

    async def delete_session_memory(
        self,
        tenant_id: str,
        session_id: str,
        key: str,
    ) -> bool:
        """
        Delete a session memory entry.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            session_id: Session ID the memory belongs to
            key: Key to delete

        Returns:
            True if the entry was deleted, False if not found
        """
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM session_memory
                WHERE tenant_id = $1 AND session_id = $2 AND key = $3
                """,
                tenant_id,
                session_id,
                key,
            )
            return result == "DELETE 1"

    async def clear_session_memory(
        self,
        tenant_id: str,
        session_id: str,
    ) -> None:
        """
        Clear all memory for a session.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            session_id: Session ID to clear memory for
        """
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                DELETE FROM session_memory
                WHERE tenant_id = $1 AND session_id = $2
                """,
                tenant_id,
                session_id,
            )

    async def store_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        key: str,
        value: Any,
        metadata: dict | None = None,
    ) -> None:
        """
        Store or update a user memory entry.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            user_id: User ID the memory belongs to
            key: Unique key for the memory entry within the user
            value: Value to store (any JSON-serializable type)
            metadata: Optional metadata about the stored value
        """
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO user_memory (tenant_id, user_id, key, value, metadata)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (tenant_id, user_id, key)
                DO UPDATE SET value = $4, metadata = COALESCE($5, user_memory.metadata)
                """,
                tenant_id,
                user_id,
                key,
                json.dumps(value),
                json.dumps(metadata) if metadata else None,
            )

    async def get_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        key: str,
    ) -> Any | None:
        """
        Retrieve a user memory entry and increment access count.

        This method also updates the access_count and last_accessed_at
        fields for tracking frequently accessed memory.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            user_id: User ID the memory belongs to
            key: Key to retrieve

        Returns:
            The stored value if found, None otherwise
        """
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            # Update access tracking and retrieve in one operation
            row = await conn.fetchrow(
                """
                UPDATE user_memory
                SET access_count = access_count + 1,
                    last_accessed_at = NOW()
                WHERE tenant_id = $1 AND user_id = $2 AND key = $3
                RETURNING value
                """,
                tenant_id,
                user_id,
                key,
            )
            if row:
                value = row["value"]
                return json.loads(value) if isinstance(value, str) else value
            return None

    async def search_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        query: str,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        """
        Search user memory by key or value content.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            user_id: User ID to search within
            query: Search query string (case-insensitive)
            limit: Maximum number of results to return

        Returns:
            List of matching memory entries with key, value, metadata, and timestamps
        """
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT key, value, metadata, access_count, last_accessed_at, created_at, updated_at
                FROM user_memory
                WHERE tenant_id = $1 AND user_id = $2
                  AND (key ILIKE $3 OR value::text ILIKE $3)
                ORDER BY updated_at DESC
                LIMIT $4
                """,
                tenant_id,
                user_id,
                f"%{query}%",
                limit,
            )
            return [self._memory_row_to_dict(row) for row in rows]

    async def delete_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        key: str,
    ) -> bool:
        """
        Delete a user memory entry.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            user_id: User ID the memory belongs to
            key: Key to delete

        Returns:
            True if the entry was deleted, False if not found
        """
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM user_memory
                WHERE tenant_id = $1 AND user_id = $2 AND key = $3
                """,
                tenant_id,
                user_id,
                key,
            )
            return result == "DELETE 1"

    async def get_frequently_accessed_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        """
        Get the most frequently accessed user memory entries.

        Args:
            tenant_id: Tenant ID for multi-tenancy isolation
            user_id: User ID to get frequently accessed memory for
            limit: Maximum number of results to return

        Returns:
            List of memory entries sorted by access count (descending)
        """
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT key, value, metadata, access_count, last_accessed_at, created_at, updated_at
                FROM user_memory
                WHERE tenant_id = $1 AND user_id = $2 AND access_count > 0
                ORDER BY access_count DESC
                LIMIT $3
                """,
                tenant_id,
                user_id,
                limit,
            )
            return [self._memory_row_to_dict(row) for row in rows]

    def _memory_row_to_dict(self, row) -> dict[str, Any]:
        """
        Convert a memory table row to a dictionary with proper JSON parsing.

        Args:
            row: Database row from session_memory or user_memory table

        Returns:
            Dictionary with parsed JSON fields and formatted timestamps
        """
        if not row:
            return {}
        result = dict(row)

        # Parse JSON fields
        for field in ("value", "metadata"):
            if field in result and result[field] is not None and isinstance(result[field], str):
                with contextlib.suppress(json.JSONDecodeError, TypeError):
                    result[field] = json.loads(result[field])

        # Convert datetime fields to ISO format strings
        for field in ("created_at", "updated_at", "last_accessed_at"):
            if field in result and isinstance(result[field], datetime):
                result[field] = result[field].isoformat()

        return result
