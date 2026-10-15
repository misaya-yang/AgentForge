"""
PostgreSQL 数据库存储层

提供与 database/schema.sql 表结构对应的完整 CRUD 操作
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import re as _re
import time
import uuid
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any

from . import datasets as _datasets
from .knowledge_artifacts import KnowledgeArtifactPersistenceMixin
from .schema_compatibility import require_compatible_schema

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

# Keep the original module-level import surface while implementation lives in
# the dataset-focused mixin module.
DatasetPersistenceMixin = _datasets.DatasetPersistenceMixin
IndexLeaseUnavailableError = _datasets.IndexLeaseUnavailableError
INDEX_DELETION_FENCE_KEY = _datasets.INDEX_DELETION_FENCE_KEY
INDEX_DELETION_FENCE_VERSION = _datasets.INDEX_DELETION_FENCE_VERSION
DOCUMENT_LIFECYCLE_REINDEX_KEY = _datasets.DOCUMENT_LIFECYCLE_REINDEX_KEY
DOCUMENT_UPLOAD_GENERATION_KEY = _datasets.DOCUMENT_UPLOAD_GENERATION_KEY
DOCUMENT_UPLOAD_FAILED_KEY = _datasets.DOCUMENT_UPLOAD_FAILED_KEY
CONFLUENCE_SYNC_GENERATION_KEY = _datasets.CONFLUENCE_SYNC_GENERATION_KEY
DOCUMENT_INGEST_ACTION_KEY = _datasets.DOCUMENT_INGEST_ACTION_KEY
DOCUMENT_RECOVER_STAGE_KEY = _datasets.DOCUMENT_RECOVER_STAGE_KEY
DOCUMENT_PIPELINE_EXECUTION_KEY = _datasets.DOCUMENT_PIPELINE_EXECUTION_KEY
DOCUMENT_PENDING_RESTORE_VERSION_KEY = _datasets.DOCUMENT_PENDING_RESTORE_VERSION_KEY
DOCUMENT_RESTORED_SOURCE_VERSION_KEY = _datasets.DOCUMENT_RESTORED_SOURCE_VERSION_KEY
INGEST_ACTION_VOCABULARY = _datasets.INGEST_ACTION_VOCABULARY
PRIORITY_INGEST_ACTIONS = _datasets.PRIORITY_INGEST_ACTIONS
RECOVER_STAGE_VOCABULARY = _datasets.RECOVER_STAGE_VOCABULARY
_INDEX_DELETION_OPERATIONS = _datasets._INDEX_DELETION_OPERATIONS
_json_object = _datasets._json_object
make_dataset_index_deletion_fence = _datasets.make_dataset_index_deletion_fence


def _escape_like_pattern(value: str) -> str:
    """Escape LIKE/ILIKE metacharacters so user text matches literally.

    Without this, ``%`` / ``_`` / ``\\`` in search input act as wildcards and
    change what a chunk search returns (PRD addendum §1-T2-7: CJK-safe chunk
    search recipe = escaped LIKE patterns + JSONB keywords). Values are still
    bound as query parameters; this only fixes pattern semantics.
    """
    return value.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
dataset_index_deletion_fence = _datasets.dataset_index_deletion_fence
index_config_has_reserved_deletion_fence = _datasets.index_config_has_reserved_deletion_fence
dataset_ingestion_identity = _datasets.dataset_ingestion_identity

_SAFE_COLUMN_RE = _re.compile(r"^[a-z][a-z0-9_]*$")
CONFLUENCE_SYNC_STALE_SECONDS = 3600
# One publication can execute up to MAX_CHUNK_OUTPUTS (10k) ON CONFLICT
# statements, and migration 076 advances content_revision from a statement
# trigger, including unrelated document changes during a stalled generation.
# Leave headroom for a prolonged recovery window without reaching zero.
INDEX_PUBLICATION_REVISION_RESERVE = 1_000_000_000
SOURCE_OWNED_DOCUMENT_METADATA_KEYS = frozenset(
    {
        DOCUMENT_LIFECYCLE_REINDEX_KEY,
        DOCUMENT_UPLOAD_GENERATION_KEY,
        DOCUMENT_UPLOAD_FAILED_KEY,
        CONFLUENCE_SYNC_GENERATION_KEY,
        # Dual-verb queue markers are owned by the enqueue/claim path; a user
        # metadata update mid-generation must not reroute the queued verb.
        DOCUMENT_INGEST_ACTION_KEY,
        DOCUMENT_RECOVER_STAGE_KEY,
        DOCUMENT_PIPELINE_EXECUTION_KEY,
        DOCUMENT_PENDING_RESTORE_VERSION_KEY,
        DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
        "_special_publication_generation_id",
        "_special_source_manifest",
        "_confluence_image_source_generation",
        "_confluence_attachment_manifest",
        "skipped_confluence_attachments",
        "original_file_key",
        "original_filename",
        "original_mime_type",
        "processing_mode",
        "extracted_images",
        "image_count",
        "images_embedded",
        "embedded_image_count",
        "ocr_processed",
        "structured_parsing",
    }
)


@dataclass(frozen=True)
class IndexPublicationLease:
    """Connection plus durable revision for one serialized publication."""

    connection: Any
    revision: int
    recovered: bool


def _validate_column_name(name: str) -> str:
    if not _SAFE_COLUMN_RE.match(name):
        raise ValueError(f"Unsafe SQL column name: {name!r}")
    return name


def _build_safe_set_clause(updates: list[str]) -> str:
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


class DatabaseStorage(KnowledgeArtifactPersistenceMixin, DatasetPersistenceMixin):
    """PostgreSQL 数据库存储"""

    def __init__(
        self,
        dsn: str | None = None,
        enabled: bool = False,
        auto_init: bool = False,
        schema_path: str | None = None,
        permission_cache_ttl_seconds: int = 60,
        pool_min_size: int = 2,
        pool_max_size: int = 10,
        api_key_usage_flush_interval_seconds: int = 2,
        api_key_usage_flush_batch_size: int = 100,
    ):
        self.dsn = dsn
        self.enabled = enabled and HAS_ASYNCPG and dsn
        self.auto_init = bool(auto_init)
        self.schema_path = schema_path or str(
            Path(__file__).resolve().parent.parent.parent / "database" / "schema.sql"
        )
        self._pool: Any | None = None
        self._pool_min_size = max(int(pool_min_size), 1)
        self._pool_max_size = max(int(pool_max_size), self._pool_min_size)
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
        except Exception:
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
                except Exception as exc:
                    logger.warning("Failed to flush API key usage batch: %s", exc)
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
        except Exception as exc:
            logger.warning(
                "API key usage batching failed for %s, falling back to sync update: %s",
                key_hash[:8],
                exc,
            )
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
            except Exception as sync_exc:
                logger.warning(
                    "Fallback sync API key usage update failed for %s: %s",
                    key_hash[:8],
                    sync_exc,
                )
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
            raise RuntimeError(SCHEMA_AUTHORITY_MESSAGE)
        self._pool = await asyncpg.create_pool(
            self.dsn,
            min_size=self._pool_min_size,
            max_size=self._pool_max_size,
        )
        try:
            async with self._pool.acquire() as conn:
                await require_compatible_schema(conn)
        except BaseException:
            await self._pool.close()
            self._pool = None
            raise
        logger.info(
            f"Database pool created: min_size={self._pool_min_size}, max_size={self._pool_max_size}"
        )
        if (
            self._api_key_usage_flush_interval_seconds > 0
            and self._api_key_usage_flush_batch_size > 1
            and self._api_key_usage_task is None
        ):
            self._api_key_usage_task = asyncio.create_task(self._run_api_key_usage_flusher())

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

    async def fetchval(self, query: str, *args) -> Any | None:
        """Execute a query and return its first scalar value."""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            return await conn.fetchval(query, *args)

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
        """Detect whether core tables are missing (e.g., first run)."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            # Phase 6: check per-service schemas, not public.
            services = await conn.fetchval("SELECT to_regclass('gateway.services')")
            datasets = await conn.fetchval("SELECT to_regclass('knowledge.datasets')")
            return services is None or datasets is None

    async def _auto_initialize_schema(self) -> None:
        """Auto-run schema.sql when core tables are missing (idempotent)."""
        if not self._pool:
            return
        try:
            if not await self._schema_is_missing():
                return
        except Exception:
            # If we cannot check, skip auto-init to avoid masking the real error.
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
            return password_col is None or permissions_table is None

    async def _auto_apply_account_permission_migration(self) -> None:
        """Apply account/permission migration when required."""
        if not self._pool:
            return
        try:
            missing = await self._account_permission_schema_missing()
        except Exception:
            # If we cannot determine schema state, skip auto-migration.
            return
        if not missing:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "005_account_permission_system.sql"
        )
        if not migration_path.exists():
            raise RuntimeError(f"Migration not found: {migration_path}")

        await self.execute_schema(str(migration_path))

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
        except Exception as e:
            logger.warning(f"Could not check user_permissions schema: {e}")
            return
        if not missing:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "006_user_extra_permissions.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 006 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 006_user_extra_permissions.sql")
        except Exception as e:
            # If table already exists or other non-critical error, log and continue
            if "already exists" in str(e).lower():
                logger.info("Migration 006 already applied (user_permissions table exists)")
            else:
                logger.error(f"Failed to apply migration 006: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check api_keys schema: {e}")
            return
        if not needs_migration:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "020_api_keys.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 020 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 020_api_keys.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 020 already applied")
            else:
                logger.error(f"Failed to apply migration 020: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check session_memory schema: {e}")
            return
        if not missing:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "024_assistant_memory.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 024 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 024_assistant_memory.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 024 already applied (session_memory table exists)")
            else:
                logger.error(f"Failed to apply migration 024: {e}")

    async def _fts_needs_migration(self) -> bool:
        """Check if segments table is missing the text_search tsvector column."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            col = await conn.fetchval("""
                SELECT column_name FROM information_schema.columns
                WHERE table_name = 'segments' AND column_name = 'text_search'
            """)
            return col is None

    async def _auto_apply_fts_migration(self) -> None:
        """Apply full-text search migration (028) — adds tsvector + GIN index to segments."""
        if not self._pool:
            return
        try:
            needs = await self._fts_needs_migration()
        except Exception as e:
            logger.warning(f"Could not check FTS schema: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "028_segments_fulltext_search.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 028 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 028_segments_fulltext_search.sql (FTS GIN index)")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 028 already applied")
            else:
                logger.error(f"Failed to apply migration 028: {e}")

    async def _auto_apply_source_metadata_migration(self) -> None:
        """Retired inline DDL path retained only to fail old callers closed."""
        raise RuntimeError(SCHEMA_AUTHORITY_MESSAGE)

    async def _openai_embedding_needs_migration(self) -> bool:
        """Check whether OpenAI embedding migration is needed."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            col_exists = await conn.fetchval(
                """
                SELECT column_name FROM information_schema.columns
                WHERE table_name = 'datasets' AND column_name = 'needs_reindex'
                """
            )
            if col_exists is None:
                return True
            openai_exists = await conn.fetchval(
                """
                SELECT 1 FROM datasets
                WHERE embedding_provider = 'openai'
                   OR index_config->>'embedding_provider' = 'openai'
                LIMIT 1
                """
            )
            return openai_exists is not None

    async def _auto_apply_openai_embedding_migration(self) -> None:
        """Apply migration 029 to move OpenAI embeddings to Gemini (marks needs_reindex)."""
        if not self._pool:
            return
        try:
            needs = await self._openai_embedding_needs_migration()
        except Exception as e:
            logger.warning(f"Could not check OpenAI embedding migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "029_migrate_openai_to_gemini.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 029 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 029_migrate_openai_to_gemini.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 029 already applied")
            else:
                logger.error(f"Failed to apply migration 029: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check observability governance migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "033_observability_and_quota_governance.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 033 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 033_observability_and_quota_governance.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 033 already applied")
            else:
                logger.error(f"Failed to apply migration 033: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check assistant gateway schema: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "034_assistant_gateway_foundation.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 034 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 034_assistant_gateway_foundation.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 034 already applied")
            else:
                logger.error(f"Failed to apply migration 034: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check assistant memory SoT migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "035_assistant_memory_sot.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 035 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 035_assistant_memory_sot.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 035 already applied")
            else:
                logger.error(f"Failed to apply migration 035: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check assistant runtime queue lane migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "036_assistant_queue_lanes.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 036 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 036_assistant_queue_lanes.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 036 already applied")
            else:
                logger.error(f"Failed to apply migration 036: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check runtime skills migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "037_assistant_skills.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 037 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 037_assistant_skills.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 037 already applied")
            else:
                logger.error(f"Failed to apply migration 037: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check assistant runtime scheduler/audit migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "038_assistant_scheduler_audit.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 038 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 038_assistant_scheduler_audit.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 038 already applied")
            else:
                logger.error(f"Failed to apply migration 038: {e}")

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
        except Exception as e:
            logger.warning(f"Could not check assistant runtime context metrics migration: {e}")
            return
        if not needs:
            return

        migration_path = (
            Path(__file__).resolve().parent.parent.parent
            / "database"
            / "migrations"
            / "039_assistant_context_metrics.sql"
        )
        if not migration_path.exists():
            logger.warning(f"Migration 039 not found: {migration_path}")
            return

        try:
            await self.execute_schema(str(migration_path))
            logger.info("Applied migration 039_assistant_context_metrics.sql")
        except Exception as e:
            if "already exists" in str(e).lower():
                logger.info("Migration 039 already applied")
            else:
                logger.error(f"Failed to apply migration 039: {e}")

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
                INSERT INTO sessions (
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

    async def get_session(self, session_id: str) -> dict[str, Any] | None:
        """获取会话"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM sessions WHERE session_id = $1", session_id)
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
                    UPDATE sessions
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
                    UPDATE sessions
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

        query = "SELECT * FROM sessions WHERE 1=1"
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

        # By default, hide expired sessions from list views.
        if status == "active":
            query += " AND (expires_at IS NULL OR expires_at > NOW())"

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
                "UPDATE sessions SET history = $1, updated_at = NOW() WHERE session_id = $2",
                json.dumps(history),
                session_id,
            )

    async def update_session_state(self, session_id: str, state: dict[str, Any]) -> None:
        """更新会话状态"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                "UPDATE sessions SET state = $1, updated_at = NOW() WHERE session_id = $2",
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
                UPDATE sessions
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
                UPDATE sessions
                SET config = COALESCE(config, '{}'::jsonb) || $2::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
            """,
                session_id,
                json.dumps(config),
            )
            return result == "UPDATE 1"

    async def delete_session(self, session_id: str) -> bool:
        """删除会话"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute("DELETE FROM sessions WHERE session_id = $1", session_id)
            return result == "DELETE 1"

    async def cleanup_expired_sessions(self) -> int:
        """清理过期会话"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < NOW()"
            )
            # 解析结果获取删除行数
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    # =========================================================================
    # 异步任务表 (tasks)
    # =========================================================================

    async def save_task(self, task: dict[str, Any]) -> None:
        """保存或更新任务"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO tasks (
                    task_id, request_id, service_id, user_id, tenant_id,
                    status, progress, request_data, result, error,
                    callback_url, callback_sent, priority, retry_count, max_retries,
                    metadata, started_at, completed_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18
                )
                ON CONFLICT (task_id) DO UPDATE SET
                    status = EXCLUDED.status,
                    progress = EXCLUDED.progress,
                    result = EXCLUDED.result,
                    error = EXCLUDED.error,
                    callback_sent = EXCLUDED.callback_sent,
                    retry_count = EXCLUDED.retry_count,
                    metadata = EXCLUDED.metadata,
                    started_at = EXCLUDED.started_at,
                    completed_at = EXCLUDED.completed_at,
                    updated_at = NOW()
            """,
                task.get("task_id"),
                task.get("request_id"),
                task.get("service_id"),
                task.get("user_id"),
                task.get("tenant_id"),
                task.get("status", "pending"),
                task.get("progress", 0),
                json.dumps(task.get("request_data")) if task.get("request_data") else None,
                json.dumps(task.get("result")) if task.get("result") else None,
                task.get("error"),
                task.get("callback_url"),
                task.get("callback_sent", False),
                task.get("priority", 0),
                task.get("retry_count", 0),
                task.get("max_retries", 3),
                json.dumps(task.get("metadata", {})),
                task.get("started_at"),
                task.get("completed_at"),
            )

    async def get_task(self, task_id: str) -> dict[str, Any] | None:
        """获取任务"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM tasks WHERE task_id = $1", task_id)
            return self._row_to_dict(row) if row else None

    async def list_tasks(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """获取任务列表"""
        if not self._pool:
            return []

        query = "SELECT * FROM tasks WHERE 1=1"
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

        query += (
            f" ORDER BY created_at DESC, document_id DESC"
            f" LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        )
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def update_task_status(
        self,
        task_id: str,
        status: str,
        progress: float = None,
        result: Any = None,
        error: str = None,
    ) -> None:
        """更新任务状态"""
        if not self._pool:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params = [status]
        param_idx = 2

        if progress is not None:
            updates.append(f"progress = ${param_idx}")
            params.append(progress)
            param_idx += 1

        if result is not None:
            updates.append(f"result = ${param_idx}")
            params.append(json.dumps(result))
            param_idx += 1

        if error is not None:
            updates.append(f"error = ${param_idx}")
            params.append(error)
            param_idx += 1

        if status == "processing":
            updates.append(f"started_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1
        elif status in ("completed", "failed", "cancelled"):
            updates.append(f"completed_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1

        params.append(task_id)
        query = f"UPDATE tasks SET {_build_safe_set_clause(updates)} WHERE task_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

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
        """获取待处理任务"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM tasks
                WHERE status = 'pending'
                ORDER BY priority DESC, created_at ASC
                LIMIT $1
            """,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    # =========================================================================
    # Knowledge Base (KBMS)
    # =========================================================================

    @staticmethod
    def _document_write_values(document: dict[str, Any]) -> tuple[Any, ...]:
        return (
            document.get("document_id"),
            document.get("dataset_id"),
            document.get("title"),
            document.get("source_type", "upload"),
            document.get("source_uri"),
            document.get("mime_type"),
            document.get("size_bytes"),
            document.get("status", "waiting"),
            float(document.get("progress", 0) or 0),
            document.get("error"),
            document.get("content"),
            json.dumps(document.get("metadata", {})),
            document.get("started_at"),
            document.get("completed_at"),
        )

    @staticmethod
    async def _insert_document_row(
        conn: Any,
        document: dict[str, Any],
        *,
        upsert: bool,
    ) -> None:
        conflict_clause = ""
        if upsert:
            conflict_clause = """
                ON CONFLICT (document_id) DO UPDATE SET
                    dataset_id = EXCLUDED.dataset_id,
                    title = EXCLUDED.title,
                    source_type = EXCLUDED.source_type,
                    source_uri = EXCLUDED.source_uri,
                    mime_type = EXCLUDED.mime_type,
                    size_bytes = EXCLUDED.size_bytes,
                    status = EXCLUDED.status,
                    progress = EXCLUDED.progress,
                    error = EXCLUDED.error,
                    content = EXCLUDED.content,
                    metadata = EXCLUDED.metadata,
                    started_at = EXCLUDED.started_at,
                    completed_at = EXCLUDED.completed_at,
                    updated_at = NOW()
            """
        await conn.execute(
            """
                INSERT INTO documents (
                    document_id, dataset_id, title, source_type, source_uri,
                    mime_type, size_bytes, status, progress, error, content,
                    metadata, started_at, completed_at
                ) VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8, $9, $10, $11,
                    $12, $13, $14
                )
            """
            + conflict_clause,
            *DatabaseStorage._document_write_values(document),
        )

    async def insert_document(
        self,
        document: dict[str, Any],
        *,
        expected_ingestion_identity: str | None = None,
    ) -> None:
        """Create one document generation without conflict resurrection."""

        if not self._pool:
            return
        dataset_id = str(document.get("dataset_id") or "").strip()
        if not dataset_id:
            raise ValueError("dataset_id is required for fenced document insert")

        async with self._pool.acquire() as conn, conn.transaction():
            await conn.fetchval(
                "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 0))",
                self._dataset_index_lock_name(dataset_id),
            )
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                expected_ingestion_identity,
            )
            await self._insert_document_row(conn, document, upsert=False)

    async def save_document(
        self,
        document: dict[str, Any],
        *,
        expected_ingestion_identity: str | None = None,
    ) -> None:
        """保存或更新文档 Document。

        New document creation may provide the dataset identity observed by the
        caller. The shared lifecycle lock and identity check then execute in
        the same transaction as the INSERT, serializing creation with an
        identity-changing dataset patch on every replica.
        """
        if not self._pool:
            return
        dataset_id = str(document.get("dataset_id") or "").strip()
        if not dataset_id:
            raise ValueError("dataset_id is required for fenced document save")

        async with self._pool.acquire() as conn, conn.transaction():
            await conn.fetchval(
                "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 0))",
                self._dataset_index_lock_name(dataset_id),
            )
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                expected_ingestion_identity,
            )
            await self._insert_document_row(conn, document, upsert=True)

    async def finalize_document_upload(
        self,
        document: dict[str, Any],
        *,
        upload_generation: str,
        expected_ingestion_identity: str | None = None,
        connection: Any | None = None,
    ) -> bool:
        """CAS-complete the exact upload generation without inserting a row.

        The document owner lease serializes this update with deletion and worker
        ownership. A missing row, changed status, hidden document, or generation
        mismatch returns ``False`` and must never be retried as an upsert.
        """

        if not self._pool:
            return False
        document_id = str(document.get("document_id") or "").strip()
        dataset_id = str(document.get("dataset_id") or "").strip()
        generation = str(upload_generation or "").strip()
        if not document_id or not dataset_id or not generation:
            raise ValueError("document_id, dataset_id, and upload_generation are required")
        metadata = _json_object(document.get("metadata"))
        metadata.pop(DOCUMENT_UPLOAD_GENERATION_KEY, None)

        async def _finalize(conn: Any) -> bool:
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                expected_ingestion_identity,
            )
            row = await conn.fetchrow(
                f"""
                UPDATE documents
                SET title = $3,
                    source_type = $4,
                    source_uri = $5,
                    mime_type = $6,
                    size_bytes = $7,
                    status = $8,
                    progress = $9,
                    error = $10,
                    content = $11,
                    metadata = $12::jsonb,
                    started_at = $13,
                    completed_at = $14,
                    updated_at = NOW()
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status IN (
                        'uploading', 'waiting', 'uploading_images'
                  )
                  AND COALESCE(enabled, TRUE) = TRUE
                  AND COALESCE(archived, FALSE) = FALSE
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                  )
                  AND metadata ->> $15 = $16
                RETURNING document_id
                """,
                document_id,
                dataset_id,
                document.get("title"),
                document.get("source_type", "upload"),
                document.get("source_uri"),
                document.get("mime_type"),
                document.get("size_bytes"),
                document.get("status", "waiting"),
                float(document.get("progress", 0) or 0),
                document.get("error"),
                document.get("content"),
                json.dumps(metadata),
                document.get("started_at"),
                document.get("completed_at"),
                DOCUMENT_UPLOAD_GENERATION_KEY,
                generation,
            )
            return row is not None

        if connection is not None:
            return await _finalize(connection)
        async with self.document_index_update_lease(dataset_id, document_id) as conn:
            return await _finalize(conn)

    async def begin_confluence_document_sync(
        self,
        document_id: str,
        dataset_id: str,
        *,
        generation: str,
        source_metadata: dict[str, Any],
        connection: Any,
    ) -> bool:
        """Claim an active Confluence document for one durable source generation."""

        normalized_generation = str(generation or "").strip()
        if not normalized_generation:
            raise ValueError("Confluence sync generation is required")
        source = _json_object(source_metadata)
        reserved = {
            DOCUMENT_LIFECYCLE_REINDEX_KEY,
            DOCUMENT_UPLOAD_FAILED_KEY,
            DOCUMENT_UPLOAD_GENERATION_KEY,
            CONFLUENCE_SYNC_GENERATION_KEY,
        }.intersection(source)
        if reserved:
            raise ValueError("Confluence source metadata contains a reserved key")
        source_owner = _json_object(source.get("source_owner"))
        owner_kind = str(source_owner.get("kind") or "").strip()
        owner_id = str(source_owner.get("id") or "").strip()
        if owner_kind not in {"binding", "direct"} or not owner_id:
            raise ValueError("Confluence source metadata requires a valid source_owner")
        source["source_owner"] = {"kind": owner_kind, "id": owner_id}
        marker = {
            "generation": normalized_generation,
            "source": source,
            "started_at": datetime.now(timezone.utc).isoformat(),
            "status": "syncing",
            "version": 1,
        }
        row = await connection.fetchrow(
            f"""
            UPDATE documents
            SET status = 'syncing',
                progress = 0,
                error = NULL,
                metadata = COALESCE(metadata, '{{}}'::jsonb)
                    || jsonb_build_object(
                        '{CONFLUENCE_SYNC_GENERATION_KEY}',
                        $4::jsonb
                    ),
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND (
                    (
                        status IN ('completed', 'error', 'waiting')
                        AND NOT (
                            COALESCE(metadata, '{{}}'::jsonb)
                            ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                        )
                    )
                    OR (
                        status = 'syncing'
                        AND (
                            metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                                ->> 'generation' = $3
                            OR (
                                metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                                    -> 'source' -> 'source_owner' = $5::jsonb
                                AND metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                                    ->> 'generation' IS DISTINCT FROM $3
                                AND COALESCE(
                                    NULLIF(
                                        metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                                            ->> 'started_at',
                                        ''
                                    )::timestamptz,
                                    '-infinity'::timestamptz
                                ) <= NOW() - make_interval(secs => $6)
                            )
                        )
                    )
              )
              AND COALESCE(enabled, TRUE) = TRUE
              AND COALESCE(archived, FALSE) = FALSE
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
              )
            RETURNING document_id
            """,
            document_id,
            dataset_id,
            normalized_generation,
            json.dumps(marker),
            json.dumps(source["source_owner"]),
            CONFLUENCE_SYNC_STALE_SECONDS,
        )
        return row is not None

    async def abort_confluence_document_sync(
        self,
        document_id: str,
        dataset_id: str,
        *,
        generation: str,
        error: str,
        connection: Any,
    ) -> bool:
        """Abort only the exact failed source generation under its owner lease."""

        normalized_generation = str(generation or "").strip()
        if not normalized_generation:
            raise ValueError("Confluence sync generation is required")
        row = await connection.fetchrow(
            f"""
            UPDATE documents
            SET status = 'error',
                progress = 100,
                error = $4,
                metadata = COALESCE(metadata, '{{}}'::jsonb)
                    - '{CONFLUENCE_SYNC_GENERATION_KEY}',
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status = 'syncing'
              AND metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                    ->> 'generation' = $3
            RETURNING document_id
            """,
            document_id,
            dataset_id,
            normalized_generation,
            str(error or "Confluence source generation failed")[:2000],
        )
        return row is not None

    async def prepare_confluence_document_update(
        self,
        document_id: str,
        dataset_id: str,
        *,
        generation: str,
        title: str,
        content: str,
        confluence_version: int,
        source_metadata: dict[str, Any],
        page_record: dict[str, Any] | None = None,
        connection: Any,
    ) -> bool:
        """Prepare one active Confluence document while its owner lease is held."""

        metadata_patch = _json_object(source_metadata)
        normalized_generation = str(generation or "").strip()
        if not normalized_generation:
            raise ValueError("Confluence sync generation is required")
        reserved = {
            DOCUMENT_LIFECYCLE_REINDEX_KEY,
            DOCUMENT_UPLOAD_FAILED_KEY,
            DOCUMENT_UPLOAD_GENERATION_KEY,
            CONFLUENCE_SYNC_GENERATION_KEY,
            DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
        }.intersection(metadata_patch)
        if reserved:
            raise ValueError("Confluence source metadata contains a reserved key")
        row = await connection.fetchrow(
            f"""
            UPDATE documents
            SET title = $3,
                content = $4,
                confluence_version = $5,
                metadata = (
                    COALESCE(metadata, '{{}}'::jsonb)
                    - '{CONFLUENCE_SYNC_GENERATION_KEY}'
                    - '{DOCUMENT_RESTORED_SOURCE_VERSION_KEY}'
                    - 'images_embedded'
                    - 'embedded_image_count'
                ) || $6::jsonb,
                status = 'waiting',
                progress = 0,
                error = NULL,
                started_at = NULL,
                completed_at = NULL,
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status = 'syncing'
              AND COALESCE(enabled, TRUE) = TRUE
              AND COALESCE(archived, FALSE) = FALSE
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
              )
              AND metadata -> '{CONFLUENCE_SYNC_GENERATION_KEY}'
                    ->> 'generation' = $7
            RETURNING document_id
            """,
            document_id,
            dataset_id,
            title,
            content,
            int(confluence_version),
            json.dumps(metadata_patch),
            normalized_generation,
        )
        if row is None:
            return False
        if page_record:
            await self.upsert_confluence_page(
                binding_id=str(page_record.get("binding_id") or ""),
                page_id=str(page_record.get("page_id") or ""),
                document_id=document_id,
                space_key=str(page_record.get("space_key") or ""),
                title=str(page_record.get("title") or title),
                version=int(page_record.get("version") or confluence_version),
                content_hash=page_record.get("content_hash"),
                parent_page_id=page_record.get("parent_page_id"),
                depth=int(page_record.get("depth") or 0),
                status=str(page_record.get("status") or "synced"),
                labels=list(page_record.get("labels") or []),
                web_url=page_record.get("web_url"),
                author=page_record.get("author"),
                confluence_updated_at=page_record.get("confluence_updated_at"),
                image_count=int(page_record.get("image_count") or 0),
                connection=connection,
            )
        return True

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """获取 Document"""
        if not self._pool:
            return None
        if connection is not None:
            row = await connection.fetchrow(
                "SELECT * FROM documents WHERE document_id = $1",
                document_id,
            )
            return self._row_to_dict(row) if row else None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM documents WHERE document_id = $1", document_id)
            return self._row_to_dict(row) if row else None

    async def list_documents(
        self,
        dataset_id: str,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
        *, active_only: bool = False, tenant_id: str | None = None,
    ) -> list[dict[str, Any]]:
        """列出文档"""
        if not self._pool:
            return []

        # D1 (frontend handoff): the list UI derives enabled/disabled/archived
        # badges and per-stage durations from row fields, so the page SELECT
        # must carry them — enabled/archived also feed the display_status
        # stamp applied by the service layer.
        query = """
            SELECT d.document_id, d.dataset_id, d.title, d.source_type, d.source_uri,
                   d.mime_type, d.size_bytes, d.status, d.progress, d.error, d.metadata,
                   d.enabled, d.disabled_at, d.archived, d.archived_reason, d.archived_at,
                   d.parsing_started_at, d.splitting_started_at, d.indexing_started_at,
                   d.started_at, d.completed_at, d.created_at, d.updated_at,
                   COALESCE(h.hit_count, 0)::bigint AS hit_count
            FROM documents AS d
            LEFT JOIN (
                SELECT document_id, SUM(hit_count)::bigint AS hit_count
                FROM segments
                WHERE dataset_id = $1
                GROUP BY document_id
            ) AS h ON h.document_id = d.document_id
            WHERE d.dataset_id = $1
        """
        params: list[Any] = [dataset_id]
        param_idx = 2

        if active_only:
            if not str(tenant_id or "").strip():
                raise ValueError("tenant_id is required for active document listing")
            query += f""" AND COALESCE(d.enabled, TRUE) = TRUE
                AND COALESCE(d.archived, FALSE) = FALSE
                AND NOT (COALESCE(d.metadata, '{{}}'::jsonb) ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}')
                AND EXISTS (SELECT 1 FROM datasets ds WHERE ds.dataset_id = d.dataset_id
                            AND ds.tenant_id = ${param_idx} AND ds.is_deleted = FALSE)"""
            params.append(tenant_id)
            param_idx += 1

        if status:
            query += f" AND d.status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY d.created_at DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def count_documents(
        self,
        dataset_id: str,
        status: str | None = None,
        *, active_only: bool = False, tenant_id: str | None = None,
    ) -> int:
        """Total document rows for a dataset page (pagination companion)."""
        if not self._pool:
            return 0

        query = "SELECT COUNT(*) FROM documents AS d WHERE d.dataset_id = $1"
        params: list[Any] = [dataset_id]
        param_idx = 2
        if active_only:
            if not str(tenant_id or "").strip():
                raise ValueError("tenant_id is required for active document count")
            query += f""" AND COALESCE(d.enabled, TRUE) = TRUE
                AND COALESCE(d.archived, FALSE) = FALSE
                AND NOT (COALESCE(d.metadata, '{{}}'::jsonb) ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}')
                AND EXISTS (SELECT 1 FROM datasets ds WHERE ds.dataset_id = d.dataset_id
                            AND ds.tenant_id = ${param_idx} AND ds.is_deleted = FALSE)"""
            params.append(tenant_id)
            param_idx += 1
        if status:
            query += f" AND d.status = ${param_idx}"
            params.append(status)

        async with self._pool.acquire() as conn:
            return await conn.fetchval(query, *params)

    async def count_documents_by_source_type(self, dataset_id: str) -> dict[str, int]:
        """Exact source totals; never derive dataset statistics from one UI page."""

        if not self._pool:
            return {}
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT COALESCE(NULLIF(source_type, ''), 'upload') AS source_type,
                       COUNT(*) AS document_count
                FROM documents
                WHERE dataset_id = $1
                GROUP BY COALESCE(NULLIF(source_type, ''), 'upload')
                """,
                dataset_id,
            )
        return {
            str(row["source_type"]): int(row["document_count"] or 0)
            for row in rows
        }

    async def list_document_ids_by_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
        active_only: bool = False,
        tenant_id: str | None = None,
    ) -> list[str]:
        """List exact document IDs, optionally on an existing lifecycle lease."""

        if not self._pool:
            return []

        async def _list(conn: Any) -> list[str]:
            if active_only and not str(tenant_id or "").strip():
                raise ValueError("tenant_id is required for active document IDs")
            rows = await conn.fetch(
                """SELECT d.document_id FROM documents AS d
                   WHERE d.dataset_id = $1
                     AND ($2::boolean = FALSE OR (
                       COALESCE(d.enabled, TRUE) = TRUE
                       AND COALESCE(d.archived, FALSE) = FALSE
                       AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? $3)
                       AND EXISTS (SELECT 1 FROM datasets ds WHERE ds.dataset_id = d.dataset_id
                                   AND ds.tenant_id = $4 AND ds.is_deleted = FALSE)
                     ))""",
                dataset_id,
                active_only,
                DOCUMENT_LIFECYCLE_REINDEX_KEY,
                tenant_id,
            )
            return [str(row["document_id"]) for row in rows]

        if connection is not None:
            return await _list(connection)
        async with self._pool.acquire() as conn:
            return await _list(conn)

    async def find_stuck_documents(
        self,
        stuck_threshold_minutes: int = 15,
    ) -> list[dict[str, Any]]:
        """查找长时间未完成处理的文档"""
        if not self._pool:
            return []

        query = f"""
            SELECT document_id, dataset_id, title, status, started_at, updated_at
            FROM documents
            WHERE (
                    (
                        metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            ->> 'status' = 'pending'
                        AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            ->> 'desired_enabled' = 'true'
                        AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            ->> 'desired_archived' = 'false'
                    )
                    OR (
                        status NOT IN ('completed', 'error')
                        AND COALESCE(enabled, TRUE) = TRUE
                        AND COALESCE(archived, FALSE) = FALSE
                        AND NOT (
                            COALESCE(metadata, '{{}}'::jsonb)
                            ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                        )
                    )
                  )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
              )
              AND NOT (
                    COALESCE(metadata, '{{}}'::jsonb)
                    ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
              )
              AND COALESCE(updated_at, started_at, created_at)
                  < NOW() - make_interval(mins => $1)
            ORDER BY updated_at ASC
        """

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, stuck_threshold_minutes)
            return [self._row_to_dict(row) for row in rows]

    async def fail_stale_document_uploads(
        self,
        stuck_threshold_minutes: int = 60,
        *,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """Fail abandoned upload owners without publishing incomplete content."""

        if not self._pool:
            return []
        threshold = max(int(stuck_threshold_minutes), 1)
        bounded_limit = min(max(int(limit), 1), 1000)
        query = f"""
            WITH candidates AS (
                SELECT d.document_id
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                CROSS JOIN LATERAL (
                    SELECT pg_try_advisory_xact_lock_shared(
                        hashtextextended(
                            'knowledge-dataset-index:' || ds.dataset_id,
                            0
                        )
                    ) AS dataset_locked
                ) AS dataset_gate
                CROSS JOIN LATERAL (
                    SELECT CASE
                        WHEN dataset_gate.dataset_locked THEN
                            pg_try_advisory_xact_lock(
                                hashtextextended(
                                    'knowledge-document-index:' || ds.dataset_id
                                        || ':' || d.document_id,
                                    0
                                )
                            )
                        ELSE FALSE
                    END AS document_locked
                ) AS document_gate
                WHERE COALESCE(d.metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                  AND d.status IN (
                        'uploading', 'waiting', 'uploading_images'
                  )
                  AND NOT (
                        COALESCE(d.metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                  )
                  AND dataset_gate.dataset_locked
                  AND document_gate.document_locked
                  AND ds.is_deleted = FALSE
                  AND NOT COALESCE(
                        COALESCE(ds.index_config, '{{}}'::jsonb)
                            -> 'retrieval' ? '{INDEX_DELETION_FENCE_KEY}',
                        FALSE
                  )
                  AND COALESCE(d.updated_at, d.created_at)
                        < NOW() - make_interval(mins => $1)
                ORDER BY d.updated_at ASC
                FOR UPDATE OF d SKIP LOCKED
                LIMIT $2
            )
            UPDATE documents AS d
            SET status = 'error',
                progress = 100,
                error = 'document upload did not finalize; upload the source again',
                metadata = (
                    COALESCE(d.metadata, '{{}}'::jsonb)
                    - '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                ) || jsonb_build_object(
                    '{DOCUMENT_UPLOAD_FAILED_KEY}',
                    jsonb_build_object(
                        'status', 'cleanup_pending',
                        'requires_reupload', TRUE
                    )
                ),
                completed_at = NOW(),
                updated_at = NOW()
            FROM candidates AS c
            WHERE d.document_id = c.document_id
            RETURNING d.document_id, d.dataset_id, d.status, d.error, d.metadata
        """
        async with self._pool.acquire() as conn, conn.transaction():
            rows = await conn.fetch(query, threshold, bounded_limit)
        return [self._row_to_dict(row) for row in rows]

    async def list_pending_document_upload_cleanups(
        self,
        *,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """List failed upload receipts whose storage cleanup is still pending."""

        if not self._pool:
            return []
        bounded_limit = min(max(int(limit), 1), 1000)
        query = f"""
            SELECT d.document_id, d.dataset_id, d.status, d.metadata
            FROM documents AS d
            JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
            WHERE d.metadata -> '{DOCUMENT_UPLOAD_FAILED_KEY}'
                    ->> 'status' = 'cleanup_pending'
              AND ds.is_deleted = FALSE
              AND NOT COALESCE(
                    COALESCE(ds.index_config, '{{}}'::jsonb)
                        -> 'retrieval' ? '{INDEX_DELETION_FENCE_KEY}',
                    FALSE
              )
            ORDER BY d.updated_at ASC
            LIMIT $1
        """
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, bounded_limit)
        return [self._row_to_dict(row) for row in rows]

    async def complete_document_upload_cleanup(
        self,
        document_id: str,
        dataset_id: str,
        *,
        connection: Any,
    ) -> bool:
        """Commit the terminal requires-reupload receipt after storage cleanup."""

        row = await connection.fetchrow(
            f"""
            UPDATE documents
            SET metadata = jsonb_set(
                    COALESCE(metadata, '{{}}'::jsonb),
                    '{{{DOCUMENT_UPLOAD_FAILED_KEY}}}',
                    jsonb_build_object(
                        'status', 'requires_reupload',
                        'requires_reupload', TRUE
                    ),
                    TRUE
                ),
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status = 'error'
              AND metadata -> '{DOCUMENT_UPLOAD_FAILED_KEY}'
                    ->> 'status' = 'cleanup_pending'
            RETURNING document_id
            """,
            document_id,
            dataset_id,
        )
        return row is not None

    async def claim_stuck_documents(
        self,
        stuck_threshold_minutes: int = 15,
        *,
        limit: int = 100,
        allow_active_bm25_v2: bool = False,
    ) -> list[dict[str, Any]]:
        """Atomically claim stale ingestion/lifecycle rows for durable replay.

        ``FOR UPDATE SKIP LOCKED`` makes concurrent service replicas divide the
        work. Updating ``updated_at`` creates a fresh recovery generation; if a
        claimant crashes before enqueue, the durable lifecycle marker makes the
        row eligible again after the same TTL. A processing-stage claim closes
        the interrupted execution and atomically links a new execution carrying
        the identical rule and input snapshot. Indexing recovery re-enters the
        incremental pipeline because the candidate may not yet be published.
        """

        if not self._pool:
            return []
        threshold = max(int(stuck_threshold_minutes), 1)
        bounded_limit = min(max(int(limit), 1), 1000)
        query = f"""
            WITH candidates AS MATERIALIZED (
                SELECT d.document_id, d.dataset_id, d.status AS old_status,
                       d.process_rule_id AS document_process_rule_id,
                       COALESCE(
                           NULLIF(
                               d.metadata ->> '{DOCUMENT_INGEST_ACTION_KEY}',
                               ''
                           ),
                           'ingest'
                       ) AS old_action,
                       NULLIF(
                           d.metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}',
                           ''
                       ) AS old_execution_id
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                CROSS JOIN LATERAL (
                    SELECT pg_try_advisory_xact_lock_shared(
                        hashtextextended(
                            'knowledge-dataset-index:' || ds.dataset_id,
                            0
                        )
                    ) AS dataset_locked
                ) AS dataset_gate
                CROSS JOIN LATERAL (
                    SELECT CASE
                        WHEN dataset_gate.dataset_locked THEN
                            pg_try_advisory_xact_lock(
                                hashtextextended(
                                    'knowledge-document-index:' || ds.dataset_id
                                        || ':' || d.document_id,
                                    0
                                )
                            )
                        ELSE FALSE
                    END AS document_locked
                ) AS document_gate
                WHERE (
                        (
                            d.metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'status' = 'pending'
                            AND d.metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_enabled' = 'true'
                            AND d.metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_archived' = 'false'
                        )
                        OR (
                            d.status NOT IN ('completed', 'error')
                            AND COALESCE(d.enabled, TRUE) = TRUE
                            AND COALESCE(d.archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(d.metadata, '{{}}'::jsonb)
                                ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            )
                        )
                      )
                  AND NOT (
                        COALESCE(d.metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                  )
                  AND NOT (
                        COALESCE(d.metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
                  )
                  AND NOT (
                        COALESCE(d.metadata, '{{}}'::jsonb)
                        ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                  )
                  -- A special preparing owner can still have a positive
                  -- revision before its first point plan. Its dedicated
                  -- recovery must finish object cleanup and requeue under
                  -- the same execution; generic TTL recovery would steal
                  -- the document marker and strand that owner.
                  AND NOT (
                        COALESCE(d.metadata, '{{}}'::jsonb)
                            ? '_special_publication_generation_id'
                  )
                  AND dataset_gate.dataset_locked
                  AND document_gate.document_locked
                  AND ds.is_deleted = FALSE
                  AND NOT COALESCE(
                        COALESCE(ds.index_config, '{{}}'::jsonb)
                            -> 'retrieval' ? '{INDEX_DELETION_FENCE_KEY}',
                        FALSE
                  )
                  AND (
                        COALESCE(
                            ds.index_config -> 'retrieval' -> 'lexical'
                                ->> 'active_version',
                            'lexical_v1'
                        ) = 'lexical_v1'
                        OR (
                            $3::boolean
                            AND ds.index_config -> 'retrieval' -> 'lexical'
                                ->> 'active_version' = 'bm25_v2'
                        )
                  )
                  AND COALESCE(d.updated_at, d.started_at, d.created_at)
                      < NOW() - make_interval(mins => $1)
                ORDER BY d.updated_at ASC
                FOR UPDATE OF d SKIP LOCKED
                LIMIT $2
            ),
            recovery_sources AS MATERIALIZED (
                SELECT c.*,
                       execution.execution_id AS previous_execution_id,
                       execution.triggered_by,
                       execution.process_rule_id,
                       execution.input_snapshot,
                       CASE
                           WHEN execution.action = 'reembed' THEN 'reembed'
                           ELSE 'recover'
                       END AS recovery_action,
                       rule.id AS verified_rule_id
                FROM candidates AS c
                LEFT JOIN document_pipeline_executions AS execution
                  ON execution.execution_id = c.old_execution_id
                 AND execution.document_id = c.document_id
                 AND execution.dataset_id = c.dataset_id
                 AND execution.action = c.old_action
                 AND execution.status = 'running'
                LEFT JOIN dataset_process_rules AS rule
                  ON rule.id = execution.process_rule_id
                 AND rule.dataset_id = c.dataset_id
                 AND c.document_process_rule_id = execution.process_rule_id
            ),
            closed_executions AS (
                UPDATE document_pipeline_executions AS execution
                SET status = 'error',
                    error = 'worker interrupted during ' || source.old_status
                        || '; superseded by crash recovery',
                    completed_at = NOW()
                FROM recovery_sources AS source
                WHERE source.old_status IN ('parsing', 'splitting', 'indexing')
                  AND source.previous_execution_id = execution.execution_id
                  AND jsonb_typeof(source.input_snapshot) = 'object'
                  AND (
                        source.old_action = 'reembed'
                        OR source.verified_rule_id IS NOT NULL
                  )
                RETURNING execution.execution_id
            ),
            recovery_executions AS (
                INSERT INTO document_pipeline_executions (
                    execution_id,
                    document_id,
                    dataset_id,
                    action,
                    trigger_source,
                    triggered_by,
                    process_rule_id,
                    input_snapshot,
                    manifest,
                    status
                )
                SELECT gen_random_uuid()::text,
                       source.document_id,
                       source.dataset_id,
                       source.recovery_action,
                       'recover',
                       source.triggered_by,
                       CASE
                           WHEN source.recovery_action = 'reembed' THEN NULL
                           ELSE source.process_rule_id
                       END,
                       source.input_snapshot,
                       jsonb_build_object(
                           'recovered_from_execution_id',
                           source.previous_execution_id,
                           'recovered_from_stage',
                           source.old_status
                       ),
                       'running'
                FROM recovery_sources AS source
                WHERE source.old_status IN ('parsing', 'splitting', 'indexing')
                  AND source.previous_execution_id IS NOT NULL
                  AND jsonb_typeof(source.input_snapshot) = 'object'
                  AND (
                        source.old_action = 'reembed'
                        OR source.verified_rule_id IS NOT NULL
                  )
                RETURNING execution_id, document_id, dataset_id, action
            ),
            updated_documents AS (
                UPDATE documents AS d
                SET status = CASE
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing')
                             AND recovery.execution_id IS NULL THEN 'error'
                        ELSE 'waiting'
                    END,
                    progress = CASE
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing')
                             AND recovery.execution_id IS NULL THEN 100
                        ELSE 0
                    END,
                    error = CASE
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing')
                             AND recovery.execution_id IS NULL THEN
                            'stuck generation replay snapshot is unavailable'
                        ELSE NULL
                    END,
                    completed_at = CASE
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing')
                             AND recovery.execution_id IS NULL THEN NOW()
                        ELSE NULL
                    END,
                    updated_at = NOW(),
                    -- A process death ends one immutable execution. Publish a
                    -- new generation whose complete input snapshot and rule id
                    -- are copied from the interrupted row. The recorded stage
                    -- remains indexing, while the worker replays the pinned
                    -- pipeline rather than assuming old rows are candidates.
                    -- reembed stays reembed because it deliberately has no
                    -- process-rule snapshot. Waiting rows already belong to a
                    -- durable generation and remain byte-for-byte pinned.
                    metadata = CASE
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing')
                             AND recovery.execution_id IS NOT NULL THEN
                            (
                                COALESCE(d.metadata, '{{}}'::jsonb)
                                    - '{DOCUMENT_INGEST_ACTION_KEY}'
                                    - '{DOCUMENT_RECOVER_STAGE_KEY}'
                                    - '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                            )
                            || jsonb_build_object(
                                '{DOCUMENT_INGEST_ACTION_KEY}', recovery.action,
                                '{DOCUMENT_PIPELINE_EXECUTION_KEY}',
                                recovery.execution_id
                            )
                            || CASE
                                WHEN recovery.action = 'recover' THEN
                                    jsonb_build_object(
                                        '{DOCUMENT_RECOVER_STAGE_KEY}',
                                        source.old_status
                                    )
                                ELSE '{{}}'::jsonb
                            END
                        WHEN source.old_status IN ('parsing', 'splitting', 'indexing') THEN
                            COALESCE(d.metadata, '{{}}'::jsonb)
                                - '{DOCUMENT_INGEST_ACTION_KEY}'
                                - '{DOCUMENT_RECOVER_STAGE_KEY}'
                                - '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                        ELSE COALESCE(d.metadata, '{{}}'::jsonb)
                    END
                FROM recovery_sources AS source
                LEFT JOIN recovery_executions AS recovery
                  ON recovery.document_id = source.document_id
                 AND recovery.dataset_id = source.dataset_id
                WHERE d.document_id = source.document_id
                RETURNING d.document_id, d.dataset_id, d.title, d.status,
                          source.old_status,
                          d.started_at, d.updated_at, d.metadata
            )
            SELECT *
            FROM updated_documents
            WHERE status = 'waiting'
        """
        async with self._pool.acquire() as conn, conn.transaction():
            rows = await conn.fetch(query, threshold, bounded_limit, allow_active_bm25_v2)
        return [self._row_to_dict(row) for row in rows]

    async def update_document_status(
        self,
        document_id: str,
        status: str,
        progress: float | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        """更新 Document 状态"""
        if not self._pool:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params: list[Any] = [status]
        param_idx = 2

        if progress is not None:
            updates.append(f"progress = ${param_idx}")
            params.append(progress)
            param_idx += 1

        if error is not None:
            updates.append(f"error = ${param_idx}")
            params.append(error)
            param_idx += 1

        # T1 lifecycle (PRD T1.1): waiting -> parsing -> splitting -> indexing
        # -> completed | error. Each stage entry stamps started_at (first
        # processing entry) plus its own per-stage timestamp.
        if status in ("parsing", "splitting", "indexing"):
            updates.append(f"started_at = COALESCE(started_at, ${param_idx})")
            params.append(datetime.utcnow())
            param_idx += 1
            stage_column = {
                "parsing": "parsing_started_at",
                "splitting": "splitting_started_at",
                "indexing": "indexing_started_at",
            }[status]
            updates.append(f"{stage_column} = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1

        if status in ("completed", "error"):
            updates.append(f"completed_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1
            if status == "error":
                # PRD T1 items 3/4: the verb marker belongs to exactly one
                # queued generation. Terminal writes retire it; the verb
                # history lives in document_pipeline_executions.
                updates.append(
                    f"metadata = COALESCE(metadata, '{{}}'::jsonb)"
                    f" - '{DOCUMENT_INGEST_ACTION_KEY}'"
                    f" - '{DOCUMENT_RECOVER_STAGE_KEY}'"
                    f" - '{DOCUMENT_PIPELINE_EXECUTION_KEY}'"
                )

        if status == "completed":
            pending_reindex = (
                f"metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}' ->> 'status' = 'pending'"
            )
            # A disabled/archived restore stays fail-closed while vectors are
            # rebuilt. Only the ingestion completion write makes the document
            # active, and the activation is committed in the same statement as
            # the terminal status.
            updates.extend(
                [
                    f"enabled = CASE WHEN {pending_reindex} THEN TRUE ELSE enabled END",
                    f"disabled_at = CASE WHEN {pending_reindex} THEN NULL ELSE disabled_at END",
                    f"disabled_by = CASE WHEN {pending_reindex} THEN NULL ELSE disabled_by END",
                    f"archived = CASE WHEN {pending_reindex} THEN FALSE ELSE archived END",
                    f"archived_at = CASE WHEN {pending_reindex} THEN NULL ELSE archived_at END",
                    f"archived_by = CASE WHEN {pending_reindex} THEN NULL ELSE archived_by END",
                    f"archived_reason = CASE WHEN {pending_reindex} THEN NULL ELSE archived_reason END",
                    f"error = CASE WHEN {pending_reindex} THEN NULL ELSE error END",
                    f"metadata = (CASE WHEN {pending_reindex} "
                    f"THEN metadata - '{DOCUMENT_LIFECYCLE_REINDEX_KEY}' ELSE metadata END)"
                    f" - '{DOCUMENT_INGEST_ACTION_KEY}'"
                    f" - '{DOCUMENT_RECOVER_STAGE_KEY}'"
                    f" - '{DOCUMENT_PIPELINE_EXECUTION_KEY}'",
                ]
            )

        params.append(document_id)
        set_clause = _build_safe_set_clause(updates)
        query = f"UPDATE documents SET {set_clause} WHERE document_id = ${param_idx}"

        if status == "completed":
            # PRD T1 unified lifecycle contract (§885-886): a restore becomes
            # visible in exactly this statement — the CASE above flips
            # enabled/archived when a pending lifecycle marker is present —
            # and retrieval cache keys are bound to the dataset revision
            # fingerprint. Advance content_revision in the same statement,
            # gated on the identical pending-marker predicate, so a result
            # cached while the document was hidden can never be served again
            # (§129: 写后不可能读到旧值). An error-terminal restore stays
            # hidden and must not invalidate anything, so only the completed
            # branch carries the bump. Deactivation bumps on its own path.
            query = f"""
            WITH pending_restore AS (
                SELECT 1
                FROM documents
                WHERE document_id = ${param_idx}
                  AND COALESCE(metadata, '{{}}'::jsonb)
                      -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}' ->> 'status' = 'pending'
            ),
            revision_bump AS (
                UPDATE datasets AS ds
                SET content_revision = COALESCE(content_revision, 0) + 1,
                    updated_at = NOW()
                WHERE ds.is_deleted = FALSE
                  AND EXISTS (SELECT 1 FROM pending_restore)
                  AND ds.dataset_id = (
                      SELECT dataset_id
                      FROM documents
                      WHERE document_id = ${param_idx}
                  )
                RETURNING ds.dataset_id
            )
            UPDATE documents SET {set_clause} WHERE document_id = ${param_idx}
            """

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def delete_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """删除 Document（级联删除 Segment）"""
        if not self._pool:
            return False

        async def _delete(conn: Any) -> bool:
            result = await conn.execute(
                "DELETE FROM documents WHERE document_id = $1",
                document_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)

    async def compare_and_swap_document_processing_mode(
        self,
        document_id: str,
        dataset_id: str,
        *,
        expected_mode: str,
        replacement_mode: str,
        detection_result: dict[str, Any],
        connection: Any | None = None,
    ) -> bool:
        """Publish auto-detection output only for its active worker generation."""

        normalized_document = str(document_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_expected = str(expected_mode or "").strip().lower()
        normalized_replacement = str(replacement_mode or "").strip().lower()
        if not all(
            (
                normalized_document,
                normalized_dataset,
                normalized_expected,
                normalized_replacement,
            )
        ):
            raise ValueError(
                "document_id, dataset_id, expected_mode, and replacement_mode are required"
            )
        if not isinstance(detection_result, dict):
            raise ValueError("detection_result must be a JSON object")
        if not self._pool and connection is None:
            raise RuntimeError("database is not connected")

        async def _publish(conn: Any) -> Any:
            return await conn.fetchrow(
                f"""
                UPDATE documents
                SET detection_result = $5::jsonb,
                    metadata = jsonb_set(
                        COALESCE(metadata, '{{}}'::jsonb),
                        '{{processing_mode}}',
                        to_jsonb($4::text),
                        TRUE
                    ),
                    updated_at = NOW()
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status = 'parsing'
                  AND COALESCE(metadata ->> 'processing_mode', 'text_only') = $3
                  AND (
                        (
                            COALESCE(enabled, TRUE) = TRUE
                            AND COALESCE(archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(metadata, '{{}}'::jsonb)
                                ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            )
                        )
                        OR (
                            metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'status' = 'pending'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_archived' = 'false'
                        )
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                  )
                RETURNING document_id
                """,
                normalized_document,
                normalized_dataset,
                normalized_expected,
                normalized_replacement,
                json.dumps(detection_result),
            )

        if connection is not None:
            row = await _publish(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _publish(conn)
        return row is not None

    async def publish_document_image_receipt(
        self,
        document_id: str,
        dataset_id: str,
        *,
        expected_original_file_key: str,
        expected_processing_mode: str,
        extracted_images: list[dict[str, Any]],
        connection: Any | None = None,
    ) -> bool:
        """Atomically publish a complete durable image-source receipt."""

        normalized_document = str(document_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_original_key = str(expected_original_file_key or "").strip()
        normalized_mode = str(expected_processing_mode or "").strip().lower()
        if not all(
            (
                normalized_document,
                normalized_dataset,
                normalized_original_key,
                normalized_mode,
            )
        ):
            raise ValueError(
                "document_id, dataset_id, original file key, and processing mode are required"
            )
        if not isinstance(extracted_images, list):
            raise ValueError("extracted_images must be a list")
        for image in extracted_images:
            if not isinstance(image, dict) or not all(
                str(image.get(key) or "").strip()
                for key in ("image_id", "storage_url", "storage_key")
            ):
                raise ValueError(
                    "each extracted image requires image_id, storage_url, and storage_key"
                )
        if not self._pool and connection is None:
            raise RuntimeError("database is not connected")
        serialized_images = json.dumps(extracted_images)

        async def _publish(conn: Any) -> Any:
            return await conn.fetchrow(
                f"""
                UPDATE documents
                SET metadata = jsonb_set(
                        jsonb_set(
                            COALESCE(metadata, '{{}}'::jsonb),
                            '{{extracted_images}}',
                            $5::jsonb,
                            TRUE
                        ),
                        '{{image_count}}',
                        to_jsonb(jsonb_array_length($5::jsonb)),
                        TRUE
                    ),
                    updated_at = NOW()
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status = 'parsing'
                  AND metadata ->> 'original_file_key' = $3
                  AND LOWER(COALESCE(metadata ->> 'processing_mode', 'text_only')) = $4
                  AND (
                        (
                            COALESCE(enabled, TRUE) = TRUE
                            AND COALESCE(archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(metadata, '{{}}'::jsonb)
                                ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            )
                        )
                        OR (
                            metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'status' = 'pending'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_archived' = 'false'
                        )
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                  )
                RETURNING document_id
                """,
                normalized_document,
                normalized_dataset,
                normalized_original_key,
                normalized_mode,
                serialized_images,
            )

        if connection is not None:
            row = await _publish(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _publish(conn)
        return row is not None

    async def clear_document_legacy_image_receipts(
        self,
        document_id: str,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """Invalidate legacy embedded-image receipts for an owned rebuild."""

        normalized_document = str(document_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_document or not normalized_dataset:
            raise ValueError("document_id and dataset_id are required")
        if not self._pool and connection is None:
            raise RuntimeError("database is not connected")

        async def _clear(conn: Any) -> Any:
            return await conn.fetchrow(
                f"""
                UPDATE documents
                SET metadata = COALESCE(metadata, '{{}}'::jsonb)
                        - 'images_embedded'
                        - 'embedded_image_count',
                    updated_at = NOW()
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status NOT IN ('uploading', 'syncing')
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_GENERATION_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{DOCUMENT_UPLOAD_FAILED_KEY}'
                  )
                  AND NOT (
                        COALESCE(metadata, '{{}}'::jsonb)
                        ? '{CONFLUENCE_SYNC_GENERATION_KEY}'
                  )
                  AND (
                        (
                            COALESCE(enabled, TRUE) = TRUE
                            AND COALESCE(archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(metadata, '{{}}'::jsonb)
                                ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            )
                        )
                        OR (
                            metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'status' = 'pending'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_archived' = 'false'
                        )
                  )
                RETURNING document_id
                """,
                normalized_document,
                normalized_dataset,
            )

        if connection is not None:
            row = await _clear(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _clear(conn)
        return row is not None

    async def update_document_fields(
        self,
        document_id: str,
        fields: dict[str, Any],
        *,
        connection: Any | None = None,
        allow_lifecycle_marker_update: bool = False,
    ) -> None:
        """Update arbitrary document fields (Dify-style enable/disable/archive support)"""
        if not self._pool or not fields:
            return

        # Allowed fields for update (content excluded — use save_document for content updates)
        allowed = {
            "title",
            "metadata",
            "enabled",
            "disabled_at",
            "disabled_by",
            "archived",
            "archived_reason",
            "archived_by",
            "archived_at",
            "batch",
            "doc_type",
            "doc_form",
            "doc_language",
            "word_count",
            "segment_count",
            "tokens",
            "process_rule_id",
            # T3 embedding provenance (migration 102): which model embedded
            # this document's vectors. Stamped at ingestion completion.
            "embedding_model",
            "embedding_model_version",
            "embedding_dimension",
        }
        filtered = {k: v for k, v in fields.items() if k in allowed}
        if not filtered:
            return
        incoming_metadata = filtered.get("metadata")
        if isinstance(incoming_metadata, dict) and (
            DOCUMENT_UPLOAD_GENERATION_KEY in incoming_metadata
            or DOCUMENT_UPLOAD_FAILED_KEY in incoming_metadata
            or CONFLUENCE_SYNC_GENERATION_KEY in incoming_metadata
            or DOCUMENT_INGEST_ACTION_KEY in incoming_metadata
            or DOCUMENT_RECOVER_STAGE_KEY in incoming_metadata
            or DOCUMENT_PIPELINE_EXECUTION_KEY in incoming_metadata
            or DOCUMENT_PENDING_RESTORE_VERSION_KEY in incoming_metadata
            or DOCUMENT_RESTORED_SOURCE_VERSION_KEY in incoming_metadata
            or "_special_publication_generation_id" in incoming_metadata
            or "_special_source_manifest" in incoming_metadata
        ):
            raise ValueError("document internal metadata keys are reserved")
        if (
            isinstance(incoming_metadata, dict)
            and DOCUMENT_LIFECYCLE_REINDEX_KEY in incoming_metadata
            and not allow_lifecycle_marker_update
        ):
            raise ValueError(f"metadata key '{DOCUMENT_LIFECYCLE_REINDEX_KEY}' is reserved")

        updates = ["updated_at = NOW()"]
        params: list[Any] = []
        param_idx = 1

        for key, value in filtered.items():
            if key == "metadata" and isinstance(value, dict):
                mutable_source_keys = (
                    {DOCUMENT_LIFECYCLE_REINDEX_KEY} if allow_lifecycle_marker_update else set()
                )
                preserved_keys = SOURCE_OWNED_DOCUMENT_METADATA_KEYS - mutable_source_keys
                sanitized_value = {
                    metadata_key: metadata_value
                    for metadata_key, metadata_value in value.items()
                    if metadata_key not in preserved_keys
                }
                preservation_terms = [
                    (
                        "(CASE WHEN COALESCE(metadata, '{}'::jsonb) "
                        f"? '{metadata_key}' THEN "
                        f"jsonb_build_object('{metadata_key}', metadata -> '{metadata_key}') "
                        "ELSE '{}'::jsonb END)"
                    )
                    for metadata_key in sorted(preserved_keys)
                ]
                # Generic callers can replace user metadata, but source receipts
                # and lifecycle owners always win from the authoritative row.
                updates.append(
                    f"metadata = ${param_idx}::jsonb || " + " || ".join(preservation_terms)
                )
                params.append(json.dumps(sanitized_value))
            else:
                updates.append(f"{key} = ${param_idx}")
                params.append(value)
            param_idx += 1

        params.append(document_id)
        query = f"UPDATE documents SET {_build_safe_set_clause(updates)} WHERE document_id = ${param_idx}"

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def clear_document_lifecycle_marker(
        self,
        document_id: str,
        *,
        expected_status: str,
        connection: Any | None = None,
    ) -> bool:
        """Remove a lifecycle marker only from the expected saga generation."""

        normalized_document = str(document_id or "").strip()
        normalized_status = str(expected_status or "").strip()
        if not normalized_document or not normalized_status:
            raise ValueError("document_id and expected_status are required")
        if not self._pool:
            raise RuntimeError("database is not connected")

        async def _clear(conn: Any) -> Any:
            return await conn.fetchrow(
                f"""
                UPDATE documents
                SET metadata = COALESCE(metadata, '{{}}'::jsonb)
                        - '{DOCUMENT_LIFECYCLE_REINDEX_KEY}',
                    updated_at = NOW()
                WHERE document_id = $1
                  AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                        ->> 'status' = $2
                RETURNING document_id
                """,
                normalized_document,
                normalized_status,
            )

        if connection is not None:
            row = await _clear(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _clear(conn)
        return row is not None

    async def update_document_content(
        self,
        document_id: str,
        content: str,
        *,
        connection: Any | None = None,
    ) -> None:
        """Internal: update document content (for re-extraction during reindex)"""
        if not self._pool:
            return

        async def _update(conn: Any) -> None:
            await conn.execute(
                "UPDATE documents SET content = $1, updated_at = NOW() WHERE document_id = $2",
                content,
                document_id,
            )

        if connection is not None:
            await _update(connection)
            return
        async with self._pool.acquire() as conn:
            await _update(conn)

    @contextlib.asynccontextmanager
    async def dataset_index_publication_lease(
        self,
        dataset_id: str,
        *,
        expected_ingestion_identity: str,
        special_execution_id: str | None = None,
        document_shared_lease_held: bool = False,
    ):
        """Serialize a short cross-store publish and mark reads retryable.

        Embedding and parsing happen before this lease.  While it is held,
        ``content_revision`` is negative, which is the durable seqlock marker
        consumed by retrieval.  A reader that began before the marker observes
        a changed revision at its final fence; a reader that begins after it
        waits/retries without ever returning the intermediate Qdrant/PG state.

        The caller must make the revision positive with either
        ``commit_text_segment_publication`` or ``abort_index_publication``.
        Leaving it negative is intentional fail-closed behavior when rollback
        itself cannot be proven complete.  Once the session advisory lock is
        free, a later worker may reacquire the same negative revision and
        resume from the durable disabled Qdrant backups keyed by that value.
        """

        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_dataset:
            raise ValueError("dataset_id is required for index publication")
        if not self._pool:
            raise RuntimeError("database is not connected")
        if document_shared_lease_held and self._pool_max_size < 3:
            raise RuntimeError("special publication requires at least three database connections")
        dataset_lock_name = self._dataset_index_lock_name(normalized_dataset)
        publication_lock_name = f"knowledge-dataset-publication:{normalized_dataset}"
        async with self._pool.acquire() as conn:
            dataset_acquired = False
            if not document_shared_lease_held:
                dataset_acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_lock_shared(hashtextextended($1, 0))",
                    dataset_lock_name,
                )
                if dataset_acquired is not True:
                    raise IndexLeaseUnavailableError(
                        "dataset index transition is in progress; refusing publication"
                    )
            publication_acquired = False
            try:
                publication_acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    publication_lock_name,
                )
                if publication_acquired is not True:
                    raise IndexLeaseUnavailableError(
                        "dataset publication is already in progress"
                    )
                async with conn.transaction():
                    await self._require_dataset_ingestion_identity(
                        conn,
                        normalized_dataset,
                        expected_ingestion_identity,
                    )
                    revision = await conn.fetchval(
                        """
                        SELECT content_revision
                        FROM datasets
                        WHERE dataset_id = $1 AND is_deleted = FALSE
                        FOR UPDATE
                        """,
                        normalized_dataset,
                    )
                    if revision is None:
                        raise RuntimeError("dataset disappeared before index publication")
                    recovered = int(revision) < 0
                    if recovered:
                        publication_revision = int(revision)
                    else:
                        publication_revision = await conn.fetchval(
                            """
                            UPDATE datasets
                            SET content_revision = (
                                    -ABS(COALESCE(content_revision, 0))
                                    - $2
                                ),
                                updated_at = NOW()
                            WHERE dataset_id = $1 AND is_deleted = FALSE
                            RETURNING content_revision
                            """,
                            normalized_dataset,
                            INDEX_PUBLICATION_REVISION_RESERVE,
                        )
                    if special_execution_id:
                        if recovered:
                            active_owner = getattr(
                                self, "get_active_special_publication_for_dataset", None,
                            )
                            if not callable(active_owner):
                                raise RuntimeError("special publication owner lookup is unavailable")
                            owner = await active_owner(
                                normalized_dataset, connection=conn,
                            )
                            if str(owner.get("execution_id") or "") != special_execution_id:
                                raise RuntimeError("negative index revision belongs to another owner")
                        else:
                            bind = getattr(self, "bind_special_publication_revision", None)
                            if not callable(bind):
                                raise RuntimeError("special publication owner binding is unavailable")
                            await bind(
                                special_execution_id,
                                normalized_dataset,
                                int(publication_revision),
                                connection=conn,
                            )
                yield IndexPublicationLease(
                    connection=conn,
                    revision=int(publication_revision),
                    recovered=recovered,
                )
            finally:
                try:
                    if publication_acquired:
                        unlock_task = asyncio.create_task(
                            conn.fetchval(
                                "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                                publication_lock_name,
                            )
                        )
                        try:
                            released = await asyncio.shield(unlock_task)
                        except asyncio.CancelledError:
                            released = await unlock_task
                            raise
                        if released is not True:
                            raise RuntimeError(
                                "dataset index publication lease was not released"
                            )
                finally:
                    if dataset_acquired:
                        shared_unlock = asyncio.create_task(
                            conn.fetchval(
                                "SELECT pg_advisory_unlock_shared(hashtextextended($1, 0))",
                                dataset_lock_name,
                            )
                        )
                        try:
                            dataset_released = await asyncio.shield(shared_unlock)
                        except asyncio.CancelledError:
                            dataset_released = await shared_unlock
                            raise
                        if dataset_released is not True:
                            raise RuntimeError(
                                "dataset shared publication lease was not released"
                            )

    @staticmethod
    async def _finish_index_publication(
        connection: Any,
        dataset_id: str,
    ) -> int:
        row = await connection.fetchrow(
            """
            UPDATE datasets
            SET content_revision = ABS(content_revision) + 1,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND is_deleted = FALSE
              AND content_revision < 0
            RETURNING content_revision
            """,
            dataset_id,
        )
        if row is None:
            raise RuntimeError("dataset index publication fence was lost")
        return int(row["content_revision"])

    async def finish_index_publication(
        self,
        dataset_id: str,
        *,
        connection: Any,
    ) -> int:
        """Close a deferred publication fence and return its positive revision."""

        return await self._finish_index_publication(connection, dataset_id)

    async def abort_index_publication(
        self,
        dataset_id: str,
        *,
        connection: Any,
    ) -> None:
        """Release the read fence only after external rollback succeeded."""

        async with connection.transaction():
            await self._finish_index_publication(connection, dataset_id)

    async def commit_text_segment_publication(
        self,
        *,
        dataset_id: str,
        document_id: str,
        segment_rows: list[dict[str, Any]],
        keep_segment_ids: list[str],
        staged_segment_ids: list[str],
        delete_excess: bool,
        expected_ingestion_identity: str,
        connection: Any,
        finish_publication: bool = True,
        candidate_content: str | None = None,
        expected_content: str | None = None,
        candidate_metadata_patch: dict[str, Any] | None = None,
        candidate_word_count: int | None = None,
        candidate_version_number: int | None = None,
        previous_version_number: int | None = None,
        finalize_document: bool = False,
        delete_all_excess: bool = False,
        defer_terminal_until_fence_release: bool = False,
    ) -> tuple[int, int]:
        """Atomically replace/activate PostgreSQL rows and release the fence."""

        async with connection.transaction():
            await self._require_dataset_ingestion_identity(
                connection,
                dataset_id,
                expected_ingestion_identity,
            )
            await self.insert_segments(segment_rows, connection=connection)
            deleted = 0
            if delete_excess:
                deleted = await self.delete_segments_by_document(
                    document_id,
                    exclude_ids=keep_segment_ids,
                    content_type=None if delete_all_excess else "text",
                    connection=connection,
                )
            promoted = await self.activate_staged_segments(
                document_id,
                staged_segment_ids,
                connection=connection,
            )
            if candidate_content is not None:
                current = await connection.fetchrow(
                    """
                    SELECT content, metadata, current_version FROM documents
                    WHERE document_id = $1 AND dataset_id = $2 AND status = 'indexing'
                    FOR UPDATE
                    """,
                    document_id,
                    dataset_id,
                )
                if current is None or str(current["content"] or "") != expected_content:
                    raise RuntimeError("document source changed during index publication")
                if candidate_version_number is not None:
                    metadata = _json_object(current["metadata"])
                    if (
                        metadata.get(DOCUMENT_PENDING_RESTORE_VERSION_KEY)
                        != {
                            "previous_version": previous_version_number,
                            "candidate_version": candidate_version_number,
                        }
                        or previous_version_number is None
                        or previous_version_number <= int(current["current_version"] or 0)
                        or candidate_version_number <= int(current["current_version"] or 0)
                    ):
                        raise RuntimeError("restore candidate changed during index publication")
                    previous = await connection.fetchrow(
                        """
                        UPDATE document_versions
                        SET change_type = 'updated'
                        WHERE document_id = $1 AND version_number = $2
                          AND change_type = 'pending_before_restore' AND content = $3
                        RETURNING version_id
                        """,
                        document_id,
                        previous_version_number,
                        expected_content,
                    )
                    if previous is None:
                        raise RuntimeError("pre-restore source snapshot changed")
                    version = await connection.fetchrow(
                        """
                        UPDATE document_versions
                        SET change_type = 'restored'
                        WHERE document_id = $1 AND version_number = $2
                          AND change_type = 'pending_restore' AND content = $3
                        RETURNING version_id
                        """,
                        document_id,
                        candidate_version_number,
                        candidate_content,
                    )
                    if version is None:
                        raise RuntimeError("restore candidate content changed")
                    await connection.execute(
                        """
                        UPDATE documents
                        SET content = $3,
                            metadata = (COALESCE(metadata, '{}'::jsonb) - $4::text)
                                || jsonb_build_object($5::text, $8::integer)
                                || $6::jsonb,
                            word_count = COALESCE($7, word_count),
                            current_version = $8,
                            version_count = COALESCE(version_count, 0) + 2
                        WHERE document_id = $1 AND dataset_id = $2
                        """,
                        document_id,
                        dataset_id,
                        candidate_content,
                        DOCUMENT_PENDING_RESTORE_VERSION_KEY,
                        DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
                        json.dumps(candidate_metadata_patch or {}),
                        candidate_word_count,
                        candidate_version_number,
                    )
                else:
                    await connection.execute(
                        """
                        UPDATE documents
                        SET content = $3,
                            metadata = COALESCE(metadata, '{}'::jsonb) || $4::jsonb,
                            word_count = COALESCE($5, word_count)
                        WHERE document_id = $1 AND dataset_id = $2
                        """,
                        document_id,
                        dataset_id,
                        candidate_content,
                        json.dumps(candidate_metadata_patch or {}),
                        candidate_word_count,
                    )
            await connection.execute(
                """
                UPDATE documents
                SET segment_count = (
                        SELECT COUNT(*) FROM segments WHERE document_id = $1
                    ),
                    updated_at = NOW()
                WHERE document_id = $1 AND dataset_id = $2
                """,
                document_id,
                dataset_id,
            )
            if finalize_document:
                if not finish_publication and not defer_terminal_until_fence_release:
                    raise RuntimeError("text terminal requires one completed publication")
                if defer_terminal_until_fence_release:
                    await connection.execute(
                        """UPDATE documents
                           SET status = 'completed', progress = 100, error = NULL,
                               completed_at = NOW(), updated_at = NOW()
                           WHERE document_id = $1 AND dataset_id = $2""",
                        document_id, dataset_id,
                    )
                else:
                    await connection.execute(
                        f"""UPDATE documents
                           SET status = 'completed', progress = 100, error = NULL,
                               metadata = COALESCE(metadata, '{{}}'::jsonb)
                                   - '{DOCUMENT_INGEST_ACTION_KEY}'
                                   - '{DOCUMENT_RECOVER_STAGE_KEY}'
                                   - '{DOCUMENT_PIPELINE_EXECUTION_KEY}',
                               completed_at = NOW(), updated_at = NOW()
                           WHERE document_id = $1 AND dataset_id = $2""",
                        document_id, dataset_id,
                    )
            if finish_publication:
                await self._finish_index_publication(connection, dataset_id)
        return promoted, deleted

    async def commit_reembed_publication(
        self,
        *,
        dataset_id: str,
        document_id: str,
        staged_segment_ids: list[str],
        expected_ingestion_identity: str,
        connection: Any,
        finish_publication: bool = True,
    ) -> int:
        """Atomically promote repaired staging rows and release the fence."""

        async with connection.transaction():
            await self._require_dataset_ingestion_identity(
                connection,
                dataset_id,
                expected_ingestion_identity,
            )
            promoted = await self.activate_staged_segments(
                document_id,
                staged_segment_ids,
                connection=connection,
            )
            if finish_publication:
                await self._finish_index_publication(connection, dataset_id)
        return promoted

    async def insert_segments(
        self,
        segments: list[dict[str, Any]],
        *,
        connection: Any | None = None,
    ) -> None:
        """批量插入/更新 Segment (enhanced with Dify-style fields + content_hash)"""
        if not self._pool or not segments:
            return

        rows = []
        for seg in segments:
            # Extract source metadata from segment metadata dict if present.
            seg_meta = seg.get("metadata", {})
            if isinstance(seg_meta, str):
                try:
                    seg_meta = json.loads(seg_meta)
                except Exception:
                    seg_meta = {}

            level = seg.get("level")
            if level is None:
                level = 3

            rows.append(
                (
                    seg.get("segment_id"),
                    seg.get("dataset_id"),
                    seg.get("document_id"),
                    int(seg.get("position", 0) or 0),
                    int(level),
                    seg.get("parent_segment_id"),
                    seg.get("summary"),
                    seg.get("page_start") or seg_meta.get("page_start"),
                    seg.get("page_end") or seg_meta.get("page_end"),
                    seg.get("text", ""),
                    int(seg.get("token_count", 0) or 0),
                    seg.get("vector_id"),
                    json.dumps(seg.get("metadata", {})),
                    # New Dify-style fields
                    seg.get("enabled", True),
                    seg.get("status", "completed"),
                    int(seg.get("word_count", 0) or 0),
                    json.dumps(seg.get("keywords", [])),
                    seg.get("answer"),
                    seg.get("created_by"),
                    # Content hash for incremental updates
                    seg.get("content_hash"),
                    # Source traceability fields
                    seg.get("source_type") or seg_meta.get("source_type", "unknown"),
                    json.dumps(
                        seg.get("source_reference") or seg_meta.get("source_reference") or {}
                    ),
                    seg.get("citation_text") or seg_meta.get("citation_text", ""),
                    seg.get("page_number") or seg_meta.get("page_number"),
                    seg.get("section_header") or seg_meta.get("section_header", ""),
                    seg.get("language") or seg_meta.get("language", "en"),
                    seg.get("contextual_prefix") or seg_meta.get("contextual_prefix", ""),
                    # T1: content_type participates in the uniqueness target so
                    # text upserts never clobber image rows sharing a position.
                    seg.get("content_type") or "text",
                    # T1 stable identity columns (document_id::content_type::position)
                    seg.get("index_node_id"),
                    seg.get("index_node_hash"),
                    seg.get("image_url"),
                    seg.get("image_attachment_id"),
                    seg.get("image_filename"),
                    seg.get("image_media_type"),
                    seg.get("image_file_size"),
                )
            )

        async def _insert(conn: Any) -> None:
            await conn.executemany(
                """
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position,
                    level, parent_segment_id, summary, page_start, page_end,
                    text, token_count, vector_id, metadata,
                    enabled, status, word_count, keywords, answer, created_by,
                    content_hash,
                    source_type, source_reference, citation_text,
                    page_number, section_header, language, contextual_prefix,
                    content_type, index_node_id, index_node_hash,
                    image_url, image_attachment_id, image_filename,
                    image_media_type, image_file_size
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                          $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27,
                          $28, $29, $30, $31, $32, $33, $34, $35)
                ON CONFLICT (document_id, content_type, position) DO UPDATE SET
                    segment_id = EXCLUDED.segment_id,
                    dataset_id = EXCLUDED.dataset_id,
                    level = EXCLUDED.level,
                    parent_segment_id = EXCLUDED.parent_segment_id,
                    summary = EXCLUDED.summary,
                    page_start = EXCLUDED.page_start,
                    page_end = EXCLUDED.page_end,
                    text = EXCLUDED.text,
                    token_count = EXCLUDED.token_count,
                    vector_id = EXCLUDED.vector_id,
                    metadata = EXCLUDED.metadata,
                    -- PRD T1 item 6 / §6.3: an operator's segment disable is an
                    -- explicit visibility decision and survives re-ingestion.
                    -- A content change re-stages the row's CONTENT (text, hash,
                    -- vector) but must never revoke the disable: the row keeps
                    -- enabled=FALSE and a non-staging status so the completion
                    -- flip (status='indexing' only) can never promote it. New
                    -- rows (INSERT leg) cannot be operator-disabled.
                    enabled = CASE
                        WHEN segments.disabled_by IS NOT NULL THEN FALSE
                        ELSE EXCLUDED.enabled
                    END,
                    status = CASE
                        WHEN segments.disabled_by IS NOT NULL THEN 'completed'
                        ELSE EXCLUDED.status
                    END,
                    word_count = EXCLUDED.word_count,
                    keywords = EXCLUDED.keywords,
                    answer = EXCLUDED.answer,
                    content_hash = EXCLUDED.content_hash,
                    source_type = EXCLUDED.source_type,
                    source_reference = EXCLUDED.source_reference,
                    citation_text = EXCLUDED.citation_text,
                    page_number = EXCLUDED.page_number,
                    section_header = EXCLUDED.section_header,
                    language = EXCLUDED.language,
                    contextual_prefix = EXCLUDED.contextual_prefix,
                    index_node_id = EXCLUDED.index_node_id,
                    index_node_hash = EXCLUDED.index_node_hash,
                    image_url = EXCLUDED.image_url,
                    image_attachment_id = EXCLUDED.image_attachment_id,
                    image_filename = EXCLUDED.image_filename,
                    image_media_type = EXCLUDED.image_media_type,
                    image_file_size = EXCLUDED.image_file_size,
                    updated_at = NOW()
                """,
                rows,
            )

        if connection is not None:
            # The caller owns atomicity here (pass a transactional connection
            # when a mid-batch failure must roll the whole batch back).
            await _insert(connection)
            return
        # PRD T1 item 5 (revision atomicity): a replacement batch is
        # all-or-nothing. asyncpg executemany without an explicit transaction
        # can commit rows one at a time, so a mid-batch failure would leave
        # durable rows whose vectors were compensated away — rows the replay
        # classifier then treats as staged-resumable and flips to serving
        # without a point (silent permanent retrieval miss). One transaction
        # makes "row committed ⇒ its batch's vectors exist" hold.
        async with self._pool.acquire() as conn, conn.transaction():
            await _insert(conn)

    async def get_operator_disabled_segment_positions(
        self, document_id: str, content_type: str, *, connection: Any,
    ) -> set[int]:
        """Read the operator's visibility decisions inside a publication lease."""

        rows = await connection.fetch(
            "SELECT position FROM segments "
            "WHERE document_id = $1 AND content_type = $2 "
            "AND disabled_by IS NOT NULL FOR UPDATE",
            document_id, content_type,
        )
        return {int(row["position"]) for row in rows}

    async def get_segment_hashes_by_document(
        self, document_id: str, content_type: str = "text"
    ) -> dict[int, dict[str, Any]]:
        """
        获取文档现有 segments 的 hash 映射，用于增量更新比对

        Args:
            document_id: 文档 ID
            content_type: 内容类型过滤 (text/image)

        Returns:
            position -> {segment_id, vector_id, content_hash, status, enabled}
            的映射。status/enabled let the incremental engine distinguish
            serving rows (completed) from rows a crashed generation left in
            staging (indexing), so replay can finish staging without
            re-embedding already-persisted vectors.
        """
        if not self._pool:
            return {}
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT position, segment_id, vector_id, content_hash,
                       status, enabled
                FROM segments
                WHERE document_id = $1 AND content_type = $2
                ORDER BY position
                """,
                document_id,
                content_type,
            )
            return {
                row["position"]: {
                    "segment_id": row["segment_id"],
                    "vector_id": row["vector_id"],
                    "content_hash": row["content_hash"],
                    "status": row["status"],
                    "enabled": row["enabled"],
                }
                for row in rows
            }

    async def activate_staged_segments(
        self,
        document_id: str,
        segment_ids: list[str],
        *,
        connection: Any | None = None,
    ) -> int:
        """Flip staged segments to serving state once their generation completes.

        T1 staging contract: new/changed chunks are persisted disabled with
        status='indexing'; only rows STILL in that staging state are promoted
        to enabled + 'completed'. Rows an operator disabled are never
        re-enabled: the insert upsert keeps operator-disabled rows out of the
        staging state (insert_segments DO UPDATE), and this flip additionally
        refuses any row carrying disabled_by as defense in depth. Idempotent:
        replaying the flip after success returns 0.

        Returns:
            number of rows promoted
        """
        normalized_ids = sorted(
            {str(segment_id).strip() for segment_id in segment_ids if str(segment_id).strip()}
        )
        normalized_document = str(document_id or "").strip()
        if not normalized_document or not normalized_ids:
            return 0
        if not self._pool and connection is None:
            return 0

        async def _activate(conn: Any) -> int:
            result = await conn.execute(
                """
                UPDATE segments
                SET enabled = TRUE,
                    status = 'completed',
                    updated_at = NOW()
                WHERE document_id = $1
                  AND segment_id = ANY($2::text[])
                  AND status = 'indexing'
                  AND disabled_by IS NULL
                """,
                normalized_document,
                normalized_ids,
            )
            if result.startswith("UPDATE "):
                return int(result.split()[-1])
            return 0

        if connection is not None:
            return await _activate(connection)
        async with self._pool.acquire() as conn:
            return await _activate(conn)

    async def delete_segments_by_document(
        self,
        document_id: str,
        exclude_ids: list[str] | None = None,
        content_type: str | None = None,
        *,
        connection: Any | None = None,
    ) -> int:
        """
        删除指定文档下的 Segment

        Args:
            document_id: 文档 ID
            exclude_ids: 要排除的 segment ID 列表（用于无感更新：先插入新的，再删除旧的）
            content_type: 可选的内容类型过滤（text/image）

        Returns:
            删除的记录数
        """
        if not self._pool:
            return 0

        async def _delete(conn: Any) -> int:
            if exclude_ids and content_type:
                result = await conn.execute(
                    "DELETE FROM segments WHERE document_id = $1 AND segment_id != ALL($2::text[]) AND content_type = $3",
                    document_id,
                    exclude_ids,
                    content_type,
                )
            elif exclude_ids:
                result = await conn.execute(
                    "DELETE FROM segments WHERE document_id = $1 AND segment_id != ALL($2::text[])",
                    document_id,
                    exclude_ids,
                )
            elif content_type:
                result = await conn.execute(
                    "DELETE FROM segments WHERE document_id = $1 AND content_type = $2",
                    document_id,
                    content_type,
                )
            else:
                result = await conn.execute(
                    "DELETE FROM segments WHERE document_id = $1", document_id
                )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)

    async def list_segments(
        self,
        dataset_id: str,
        document_id: str | None = None,
        query_text: str | None = None,
        limit: int = 100,
        offset: int = 0,
        *, active_only: bool = False, tenant_id: str | None = None,
    ) -> list[dict[str, Any]]:
        """列出 Segment"""
        if not self._pool:
            return []

        query = "SELECT s.* FROM segments AS s WHERE s.dataset_id = $1"
        params: list[Any] = [dataset_id]
        param_idx = 2

        if active_only:
            if not str(tenant_id or "").strip():
                raise ValueError("tenant_id is required for active segment listing")
            query += f""" AND COALESCE(s.enabled, TRUE) = TRUE
                AND s.status = 'completed'
                AND EXISTS (SELECT 1 FROM documents d JOIN datasets ds
                            ON ds.dataset_id = d.dataset_id
                            WHERE d.document_id = s.document_id
                              AND d.dataset_id = s.dataset_id
                              AND ds.tenant_id = ${param_idx}
                              AND ds.is_deleted = FALSE
                              AND COALESCE(d.enabled, TRUE) = TRUE
                              AND COALESCE(d.archived, FALSE) = FALSE
                              AND NOT (COALESCE(d.metadata, '{{}}'::jsonb)
                                       ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'))"""
            params.append(tenant_id)
            param_idx += 1

        if document_id:
            query += f" AND s.document_id = ${param_idx}"
            params.append(document_id)
            param_idx += 1

        if query_text:
            query += f" AND s.text ILIKE ${param_idx}"
            params.append(f"%{_escape_like_pattern(query_text)}%")
            param_idx += 1

        # content_type joins the sort key: positions restart per content type,
        # so without it text/image rows tie and a paginated reader (reembed's
        # full-generation walk) can skip or duplicate rows at a page boundary
        # that splits a tied group.
        query += (
            f" ORDER BY s.document_id ASC, s.position ASC, s.content_type ASC, s.segment_id ASC"
            f" LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        )
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def count_segments(
        self,
        dataset_id: str,
        document_id: str | None = None,
        query_text: str | None = None,
        *, active_only: bool = False, tenant_id: str | None = None,
    ) -> int:
        """Total segment rows for a list page (pagination companion).

        Mirrors ``list_segments``' WHERE clause exactly so the count always
        matches the filtered page it accompanies.
        """
        if not self._pool:
            return 0

        query = "SELECT COUNT(*) FROM segments AS s WHERE s.dataset_id = $1"
        params: list[Any] = [dataset_id]
        param_idx = 2
        if active_only:
            if not str(tenant_id or "").strip():
                raise ValueError("tenant_id is required for active segment count")
            query += f""" AND COALESCE(s.enabled, TRUE) = TRUE
                AND s.status = 'completed'
                AND EXISTS (SELECT 1 FROM documents d JOIN datasets ds
                            ON ds.dataset_id = d.dataset_id
                            WHERE d.document_id = s.document_id
                              AND d.dataset_id = s.dataset_id
                              AND ds.tenant_id = ${param_idx}
                              AND ds.is_deleted = FALSE
                              AND COALESCE(d.enabled, TRUE) = TRUE
                              AND COALESCE(d.archived, FALSE) = FALSE
                              AND NOT (COALESCE(d.metadata, '{{}}'::jsonb)
                                       ? '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'))"""
            params.append(tenant_id)
            param_idx += 1

        if document_id:
            query += f" AND s.document_id = ${param_idx}"
            params.append(document_id)
            param_idx += 1

        if query_text:
            query += f" AND s.text ILIKE ${param_idx}"
            params.append(f"%{_escape_like_pattern(query_text)}%")
            param_idx += 1

        async with self._pool.acquire() as conn:
            return await conn.fetchval(query, *params)

    async def get_document_statistics_aggregate(
        self, dataset_id: str, document_id: str
    ) -> dict[str, Any] | None:
        """Return uncapped document/segment counters in one SQL query."""

        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT d.document_id,
                       d.word_count,
                       d.status,
                       d.enabled,
                       d.archived,
                       COUNT(s.segment_id)::bigint AS segment_count,
                       COALESCE(SUM(s.hit_count), 0)::bigint AS hit_count
                FROM documents AS d
                LEFT JOIN segments AS s
                  ON s.dataset_id = d.dataset_id
                 AND s.document_id = d.document_id
                WHERE d.dataset_id = $1 AND d.document_id = $2
                GROUP BY d.document_id, d.word_count, d.status, d.enabled, d.archived
                """,
                dataset_id,
                document_id,
            )
        return self._row_to_dict(row) if row else None

    async def search_segments_like_any(
        self,
        dataset_id: str,
        tenant_id: str,
        terms: list[str],
        document_id: str | None = None,
        source_type: str | None = None,
        language: str | None = None,
        limit: int = 200,
        metadata_filter: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        """Keyword candidate retrieval using PostgreSQL full-text search (GIN index).

        Uses tsvector/tsquery for O(log N) index lookup when the text_search column
        is populated (migration 028). Falls back to ILIKE for pre-migration databases.
        """
        if not self._pool:
            return []

        cleaned = [t.strip() for t in (terms or []) if str(t or "").strip()]
        if not cleaned:
            return []

        # Try FTS first (fast GIN index). Only a verified pre-migration schema
        # may use the O(N) ILIKE compatibility path; operational failures must
        # remain visible instead of silently doubling database work.
        result = await self._search_segments_fts(
            dataset_id,
            tenant_id,
            cleaned,
            document_id,
            source_type,
            language,
            limit,
            metadata_filter,
        )
        if result is not None:
            return result

        return await self._search_segments_ilike(
            dataset_id,
            tenant_id,
            cleaned,
            document_id,
            source_type,
            language,
            limit,
            metadata_filter,
        )

    # Alias for retrieval_service compatibility
    search_segments_text = search_segments_like_any

    async def filter_active_segment_ids(
        self,
        dataset_id: str,
        tenant_id: str,
        segment_ids: list[str],
    ) -> set[str]:
        """Return exact-scope segment IDs that are serving.

        This is the authoritative candidate boundary for Qdrant dense,
        hierarchy, native-hybrid, and cached results. Callers must fail closed
        if this database check raises.

        Zero-window contract (PRD T1): serving-ness is decided per segment
        (enabled + status='completed'), never per document status. A document
        mid re-ingest keeps serving its previous generation; staging rows are
        invisible until the completion flip. Documents are hidden only when
        disabled, archived, or under an explicit reindex marker. Gating on
        d.status='completed' here would re-introduce the retrieval blackout
        the incremental engine exists to eliminate.
        """

        normalized_ids = sorted(
            {str(segment_id).strip() for segment_id in segment_ids if str(segment_id).strip()}
        )
        if not normalized_ids:
            return set()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise ValueError("dataset_id and tenant_id are required for active segment filtering")
        if not self._pool:
            raise RuntimeError("database is not connected")

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT DISTINCT s.segment_id
                FROM segments AS s
                JOIN documents AS d
                  ON d.document_id = s.document_id
                 AND d.dataset_id = s.dataset_id
                JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                WHERE s.dataset_id = $1
                  AND ds.tenant_id = $2
                  AND ds.is_deleted = FALSE
                  AND s.segment_id = ANY($3::text[])
                  AND COALESCE(s.enabled, TRUE) = TRUE
                  AND s.status = 'completed'
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                """,
                normalized_dataset,
                normalized_tenant,
                normalized_ids,
            )
        return {
            str(row["segment_id"]).strip() for row in rows if str(row["segment_id"] or "").strip()
        }

    async def filter_active_document_ids(
        self,
        dataset_id: str,
        tenant_id: str,
        document_ids: list[str],
    ) -> set[str]:
        """Return exact-scope document IDs that are retrieval-ready."""

        normalized_ids = sorted(
            {str(document_id).strip() for document_id in document_ids if str(document_id).strip()}
        )
        if not normalized_ids:
            return set()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise ValueError("dataset_id and tenant_id are required for active document filtering")
        if not self._pool:
            raise RuntimeError("database is not connected")

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT DISTINCT d.document_id
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.dataset_id = $1
                  AND ds.tenant_id = $2
                  AND ds.is_deleted = FALSE
                  AND d.document_id = ANY($3::text[])
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                """,
                normalized_dataset,
                normalized_tenant,
                normalized_ids,
            )
        return {
            str(row["document_id"]).strip() for row in rows if str(row["document_id"] or "").strip()
        }

    async def filter_active_summary_vector_ids(
        self, dataset_id: str, tenant_id: str, vector_ids: list[str],
    ) -> set[str]:
        """Keep L1 points on the currently published summary generation."""

        normalized_ids = sorted({str(value).strip() for value in vector_ids if str(value).strip()})
        if not normalized_ids:
            return set()
        if not dataset_id or not tenant_id or not self._pool:
            raise RuntimeError("hierarchical summary authority is unavailable")
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """SELECT DISTINCT summary.vector_id
                   FROM document_summaries AS summary
                   JOIN documents AS d ON d.document_id = summary.document_id
                   JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                  WHERE d.dataset_id = $1 AND ds.tenant_id = $2
                    AND ds.is_deleted = FALSE
                    AND summary.vector_id = ANY($3::text[])
                    AND COALESCE(d.enabled, TRUE) = TRUE
                    AND COALESCE(d.archived, FALSE) = FALSE
                    AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_document_lifecycle_reindex')""",
                dataset_id, tenant_id, normalized_ids,
            )
        return {str(row["vector_id"]).strip() for row in rows if str(row["vector_id"] or "").strip()}

    async def authorize_document_source_versions(
        self,
        dataset_id: str,
        tenant_id: str,
        references: list[tuple[str, int, str]],
    ) -> set[tuple[str, int, str]]:
        """Resolve exact saved versions for active documents in one scoped read."""

        if not dataset_id or not tenant_id or not self._pool:
            raise RuntimeError("document source authority is unavailable")
        normalized = sorted({
            (str(document_id).strip(), int(version), str(source_hash).strip().lower())
            for document_id, version, source_hash in references
            if str(document_id).strip() and int(version) > 0
        })
        if not normalized:
            return set()
        document_ids, versions, hashes = map(list, zip(*normalized, strict=True))
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """WITH requested(document_id, version_number, source_hash) AS (
                       SELECT * FROM unnest($3::text[], $4::integer[], $5::text[])
                   )
                   SELECT DISTINCT requested.document_id,
                          requested.version_number, requested.source_hash
                   FROM requested
                   JOIN documents AS d ON d.document_id = requested.document_id
                   JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                   JOIN document_versions AS v
                     ON v.document_id = d.document_id
                    AND v.version_number = requested.version_number
                   WHERE d.dataset_id = $1 AND ds.tenant_id = $2
                     AND ds.is_deleted = FALSE
                     AND COALESCE(d.enabled, TRUE) = TRUE
                     AND COALESCE(d.archived, FALSE) = FALSE
                     AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_document_lifecycle_reindex')
                     AND v.change_type NOT IN ('pending_restore', 'pending_before_restore')
                     AND lower(v.content_hash) = requested.source_hash
                     AND encode(sha256(convert_to(v.content, 'UTF8')), 'hex')
                         = requested.source_hash""",
                dataset_id, tenant_id, document_ids, versions, hashes,
            )
        return {
            (str(row["document_id"]), int(row["version_number"]), str(row["source_hash"]))
            for row in rows
        }

    async def published_source_identities(
        self, dataset_id: str, tenant_id: str, document_ids: list[str],
    ) -> dict[str, tuple[int, str]]:
        """Bind retrieval hits only to the active, verified version snapshot."""

        normalized = sorted({str(value).strip() for value in document_ids if str(value).strip()})
        if not normalized:
            return {}
        if not dataset_id or not tenant_id or not self._pool:
            raise RuntimeError("published source identity authority is unavailable")
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """SELECT d.document_id, d.current_version, lower(v.content_hash) AS source_hash
                   FROM documents AS d
                   JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                   JOIN document_versions AS v
                     ON v.document_id = d.document_id
                    AND v.version_number = d.current_version
                  WHERE d.dataset_id = $1 AND ds.tenant_id = $2
                    AND ds.is_deleted = FALSE
                    AND d.document_id = ANY($3::text[])
                    AND COALESCE(d.enabled, TRUE) = TRUE
                    AND COALESCE(d.archived, FALSE) = FALSE
                    AND NOT (COALESCE(d.metadata, '{}'::jsonb) ? '_document_lifecycle_reindex')
                    AND v.change_type NOT IN ('pending_restore', 'pending_before_restore')
                    AND v.content = d.content
                    AND encode(sha256(convert_to(v.content, 'UTF8')), 'hex')
                        = lower(v.content_hash)""",
                dataset_id, tenant_id, normalized,
            )
        return {
            str(row["document_id"]): (int(row["current_version"]), str(row["source_hash"]))
            for row in rows
        }

    async def list_unfinished_special_publication_datasets(
        self, *, limit: int = 100,
    ) -> list[dict[str, Any]]:
        """Find fenced datasets that have a durable special owner to reconcile."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """SELECT DISTINCT ds.dataset_id
                   FROM datasets AS ds
                   JOIN document_pipeline_executions AS execution
                     ON execution.dataset_id = ds.dataset_id
                  WHERE ds.content_revision < 0 AND ds.is_deleted = FALSE
                    AND execution.status = 'running'
                    AND COALESCE(execution.manifest, '{}'::jsonb)
                        ? 'special_publication'
                  ORDER BY ds.dataset_id LIMIT $1""",
                max(1, min(int(limit), 1000)),
            )
        datasets: list[dict[str, Any]] = []
        for row in rows:
            dataset = await self.get_dataset(str(row["dataset_id"]))
            if dataset is not None:
                datasets.append(dataset)
        return datasets

    async def list_unbound_special_preparations(
        self, *, limit: int = 100,
    ) -> list[dict[str, str]]:
        """Find crashed image preparations before they acquired a read fence."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """SELECT execution.execution_id, execution.document_id,
                          execution.dataset_id,
                          execution.manifest -> 'special_publication'
                              ->> 'generation_id' AS generation_id,
                          execution.manifest -> 'special_publication'
                              ->> 'source_hash' AS source_hash
                   FROM document_pipeline_executions AS execution
                   JOIN documents AS document
                     ON document.document_id = execution.document_id
                    AND document.dataset_id = execution.dataset_id
                   JOIN datasets AS dataset
                     ON dataset.dataset_id = execution.dataset_id
                  WHERE execution.status = 'running'
                    AND dataset.is_deleted = FALSE
                    AND dataset.content_revision >= 0
                    AND execution.manifest -> 'special_publication'
                        ->> 'phase' = 'preparing'
                    AND NOT (execution.manifest -> 'special_publication'
                        ? 'publication_revision')
                    AND document.metadata ->> '_document_pipeline_execution_id'
                        = execution.execution_id
                    AND document.metadata ->> '_special_publication_generation_id'
                        = execution.manifest -> 'special_publication'
                            ->> 'generation_id'
                  ORDER BY execution.created_at
                  LIMIT $1""",
                max(1, min(int(limit), 1000)),
            )
        return [
            {key: str(row[key]) for key in (
                "execution_id", "document_id", "dataset_id", "generation_id", "source_hash",
            )}
            for row in rows
        ]

    async def document_has_completed_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
    ) -> bool:
        """Detect any published or partially published rows before a special replay.

        This deliberately ignores the document's current enabled/status flags:
        an interrupted reprocess can change those while its old rows remain.
        """
        if not all(str(value or "").strip() for value in (dataset_id, tenant_id, document_id)):
            raise ValueError("dataset, tenant, and document are required")
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            return bool(await conn.fetchval(
                """SELECT EXISTS (
                       SELECT 1 FROM segments AS s
                       JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                       WHERE s.dataset_id = $1 AND ds.tenant_id = $2
                         AND ds.is_deleted = FALSE AND s.document_id = $3
                         AND s.status = 'completed'
                   )""",
                dataset_id, tenant_id, document_id,
            ))

    async def document_has_specialized_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
        *, connection: Any | None = None,
    ) -> bool:
        """Detect image or hierarchy rows even if current config has changed."""
        if not all(str(value or "").strip() for value in (dataset_id, tenant_id, document_id)):
            raise ValueError("dataset, tenant, and document are required")
        if not self._pool:
            raise RuntimeError("database is not connected")
        query = """SELECT EXISTS (
                       SELECT 1 FROM segments AS s
                       JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                       WHERE s.dataset_id = $1 AND ds.tenant_id = $2
                         AND ds.is_deleted = FALSE AND s.document_id = $3
                         AND (s.level IN (1, 2) OR s.parent_segment_id IS NOT NULL
                              OR s.content_type IN
                              ('image', 'section', 'document_summary'))
                   )"""
        if connection is not None:
            return bool(await connection.fetchval(query, dataset_id, tenant_id, document_id))
        async with self._pool.acquire() as conn:
            return bool(await conn.fetchval(query, dataset_id, tenant_id, document_id))

    async def dataset_has_embeddings(self, dataset_id: str) -> bool:
        """Check if dataset has any embedded segments with vectors."""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            count = await conn.fetchval(
                """
                SELECT COUNT(*) FROM segments
                WHERE dataset_id = $1 AND vector_id IS NOT NULL
                LIMIT 1
                """,
                dataset_id,
            )
            return (count or 0) > 0

    async def search_segments_vector(
        self,
        dataset_id: str,
        query_embedding: list[float],
        top_k: int = 10,
    ) -> list[dict[str, Any]]:
        """Vector search placeholder - delegates to vector_store.

        Note: Actual vector search should use VectorStore.search() directly.
        This method exists for API compatibility.
        """
        _ = (dataset_id, query_embedding, top_k)
        # Return empty - retrieval_service should use VectorStore
        return []

    async def _search_segments_fts(
        self,
        dataset_id: str,
        tenant_id: str,
        terms: list[str],
        document_id: str | None,
        source_type: str | None,
        language: str | None,
        limit: int,
        metadata_filter: dict[str, Any] | None,
    ) -> list[dict[str, Any]] | None:
        """Full-text search using tsvector + GIN index (O(log N)).

        Returns None if text_search column doesn't exist (pre-migration).
        Uses 'simple' config for multilingual compatibility.
        """
        if not self._pool:
            return None

        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_tenant:
            raise ValueError("tenant_id is required for segment search")

        query = """
            SELECT s.*
            FROM segments AS s
            JOIN documents AS d
              ON d.document_id = s.document_id
             AND d.dataset_id = s.dataset_id
            JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
            WHERE s.dataset_id = $1
              AND ds.tenant_id = $2
              AND ds.is_deleted = FALSE
              AND COALESCE(s.enabled, TRUE) = TRUE
              AND s.status = 'completed'
              AND COALESCE(d.enabled, TRUE) = TRUE
              AND COALESCE(d.archived, FALSE) = FALSE
              AND NOT (
                  COALESCE(d.metadata, '{}'::jsonb)
                  ? '_document_lifecycle_reindex'
              )
        """
        params: list[Any] = [dataset_id, normalized_tenant]
        param_idx = 3

        if document_id:
            query += f" AND s.document_id = ${param_idx}"
            params.append(document_id)
            param_idx += 1
        if source_type:
            query += f" AND s.source_type = ${param_idx}"
            params.append(source_type)
            param_idx += 1
        if language:
            query += f" AND s.language = ${param_idx}"
            params.append(language)
            param_idx += 1
        if metadata_filter:
            query += f" AND s.metadata @> ${param_idx}::jsonb"
            params.append(json.dumps(metadata_filter))
            param_idx += 1

        # Build tsquery expression with correct parameter indexing
        # Combine all terms into a single tsquery using AND operator
        tsquery_parts = []
        for i, _ in enumerate(terms):
            tsquery_parts.append(f"plainto_tsquery('simple', ${param_idx + i})")
        tsquery_expr = " || ".join(tsquery_parts) if len(tsquery_parts) > 1 else tsquery_parts[0]

        query += f" AND s.text_search @@ ({tsquery_expr})"
        params.extend(terms)
        param_idx = param_idx + len(terms)

        # Order by relevance using the same tsquery expression
        # Note: ts_rank_cd is faster and often sufficient for ranking
        query += f" ORDER BY ts_rank_cd(s.text_search, ({tsquery_expr})) DESC"

        query += f" LIMIT ${param_idx}"
        params.append(int(limit))

        try:
            async with self._pool.acquire() as conn:
                rows = await conn.fetch(query, *params)
                return [self._row_to_dict(row) for row in rows]
        except Exception as e:
            sqlstate = str(getattr(e, "sqlstate", "") or "")
            column_name = str(getattr(e, "column_name", "") or "").lower()
            err_str = str(e).lower()
            missing_text_search = sqlstate == "42703" and (
                column_name == "text_search" or "text_search" in err_str
            )
            if missing_text_search:
                # Column doesn't exist yet — signal caller to use ILIKE fallback
                logger.warning(
                    "text_search column missing, falling back to ILIKE for dataset=%s", dataset_id
                )
                return None
            logger.error("FTS search error for dataset=%s: %s", dataset_id, e)
            raise

    async def _search_segments_ilike(
        self,
        dataset_id: str,
        tenant_id: str,
        terms: list[str],
        document_id: str | None,
        source_type: str | None,
        language: str | None,
        limit: int,
        metadata_filter: dict[str, Any] | None,
    ) -> list[dict[str, Any]]:
        """Legacy ILIKE fallback for pre-migration databases (O(N) sequential scan)."""
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_tenant:
            raise ValueError("tenant_id is required for segment search")

        query = """
            SELECT s.*
            FROM segments AS s
            JOIN documents AS d
              ON d.document_id = s.document_id
             AND d.dataset_id = s.dataset_id
            JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
            WHERE s.dataset_id = $1
              AND ds.tenant_id = $2
              AND ds.is_deleted = FALSE
              AND COALESCE(s.enabled, TRUE) = TRUE
              AND s.status = 'completed'
              AND COALESCE(d.enabled, TRUE) = TRUE
              AND COALESCE(d.archived, FALSE) = FALSE
              AND NOT (
                  COALESCE(d.metadata, '{}'::jsonb)
                  ? '_document_lifecycle_reindex'
              )
        """
        params: list[Any] = [dataset_id, normalized_tenant]
        param_idx = 3

        if document_id:
            query += f" AND s.document_id = ${param_idx}"
            params.append(document_id)
            param_idx += 1
        if source_type:
            query += f" AND s.source_type = ${param_idx}"
            params.append(source_type)
            param_idx += 1
        if language:
            query += f" AND s.language = ${param_idx}"
            params.append(language)
            param_idx += 1
        if metadata_filter:
            query += f" AND s.metadata @> ${param_idx}::jsonb"
            params.append(json.dumps(metadata_filter))
            param_idx += 1

        parts = []
        for t in terms:
            parts.append(f"s.text ILIKE ${param_idx}")
            # Escape LIKE metacharacters to prevent wildcard injection
            params.append(f"%{_escape_like_pattern(t)}%")
            param_idx += 1

        if parts:
            query += " AND (" + " OR ".join(parts) + ")"

        query += f" LIMIT ${param_idx}"
        params.append(int(limit))

        try:
            async with self._pool.acquire() as conn:
                rows = await conn.fetch(query, *params)
                return [self._row_to_dict(row) for row in rows]
        except Exception as e:
            logger.error(f"ILIKE search error: {e}, params: {params[:2]}...")
            return []

    async def get_segment(
        self,
        segment_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """获取 Segment"""
        if not self._pool:
            return None
        if connection is not None:
            row = await connection.fetchrow(
                "SELECT * FROM segments WHERE segment_id = $1",
                segment_id,
            )
            return self._row_to_dict(row) if row else None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM segments WHERE segment_id = $1", segment_id)
            return self._row_to_dict(row) if row else None

    async def get_segment_scoped(
        self,
        segment_id: str,
        dataset_id: str,
        tenant_id: str,
    ) -> dict[str, Any] | None:
        """Read one active segment under an exact tenant/dataset authority."""

        normalized_segment = str(segment_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_segment or not normalized_dataset or not normalized_tenant:
            raise ValueError("segment_id, dataset_id, and tenant_id are required")
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT s.*
                FROM segments AS s
                JOIN documents AS d
                  ON d.document_id = s.document_id
                 AND d.dataset_id = s.dataset_id
                JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                WHERE s.segment_id = $1
                  AND s.dataset_id = $2
                  AND ds.tenant_id = $3
                  AND ds.is_deleted = FALSE
                  AND COALESCE(s.enabled, TRUE) = TRUE
                  AND s.status = 'completed'
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                """,
                normalized_segment,
                normalized_dataset,
                normalized_tenant,
            )
            return self._row_to_dict(row) if row else None

    async def get_active_segment_by_tenant(
        self,
        segment_id: str,
        tenant_id: str,
    ) -> dict[str, Any] | None:
        """Resolve an active segment without exposing cross-tenant existence."""

        normalized_segment = str(segment_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_segment or not normalized_tenant:
            raise ValueError("segment_id and tenant_id are required")
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT s.*
                FROM segments AS s
                JOIN documents AS d
                  ON d.document_id = s.document_id
                 AND d.dataset_id = s.dataset_id
                JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                WHERE s.segment_id = $1
                  AND ds.tenant_id = $2
                  AND ds.is_deleted = FALSE
                  AND COALESCE(s.enabled, TRUE) = TRUE
                  AND s.status = 'completed'
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                """,
                normalized_segment,
                normalized_tenant,
            )
            return self._row_to_dict(row) if row else None

    async def get_segments_by_ids(self, segment_ids: list[str]) -> list[dict[str, Any]]:
        """批量获取 Segment，避免 N+1 查询。"""
        if not self._pool:
            return []
        ids = [sid for sid in (segment_ids or []) if sid]
        if not ids:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT * FROM segments WHERE segment_id = ANY($1::text[])",
                ids,
            )
            return [self._row_to_dict(row) for row in rows]

    async def update_segment(
        self,
        segment_id: str,
        text: str,
        token_count: int | None = None,
        metadata: dict[str, Any] | None = None,
        vector_id: str | None = None,
        *,
        answer: str | None = None,
        keywords: list[str] | None = None,
        content_hash: str | None = None,
        connection: Any | None = None,
    ) -> None:
        """更新 Segment

        ``answer``/``keywords``/``content_hash`` are keyword-only and
        ``None`` means "leave the column untouched" — callers pass ``""`` or
        ``[]`` to clear. This keeps the PUT /segments/{id} contract additive:
        omitting a field never destroys stored data.
        """
        if not self._pool:
            return

        updates = ["text = $1", "updated_at = NOW()"]
        params: list[Any] = [text]
        param_idx = 2

        if token_count is not None:
            updates.append(f"token_count = ${param_idx}")
            params.append(token_count)
            param_idx += 1

        if vector_id is not None:
            updates.append(f"vector_id = ${param_idx}")
            params.append(vector_id)
            param_idx += 1

        if metadata is not None:
            updates.append(f"metadata = ${param_idx}")
            params.append(json.dumps(metadata))
            param_idx += 1

        if answer is not None:
            updates.append(f"answer = ${param_idx}")
            params.append(answer)
            param_idx += 1

        if keywords is not None:
            updates.append(f"keywords = ${param_idx}::jsonb")
            params.append(json.dumps(keywords))
            param_idx += 1

        if content_hash is not None:
            updates.append(f"content_hash = ${param_idx}")
            params.append(content_hash)
            param_idx += 1

        params.append(segment_id)
        query = (
            f"UPDATE segments SET {_build_safe_set_clause(updates)} WHERE segment_id = ${param_idx}"
        )

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def update_segment_fields(
        self,
        segment_id: str,
        fields: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        """Update arbitrary segment fields (Dify-style enable/disable support)"""
        if not self._pool or not fields:
            return

        # Allowed fields for update
        allowed = {
            "enabled",
            "disabled_at",
            "disabled_by",
            "status",
            "hit_count",
            "word_count",
            "keywords",
            "answer",
            "index_node_id",
            "index_node_hash",
            "vector_id",
            "error",
        }
        filtered = {k: v for k, v in fields.items() if k in allowed}
        if not filtered:
            return

        updates = ["updated_at = NOW()"]
        params: list[Any] = []
        param_idx = 1

        for key, value in filtered.items():
            if key == "keywords" and isinstance(value, list):
                updates.append(f"{key} = ${param_idx}")
                params.append(json.dumps(value))
            else:
                updates.append(f"{key} = ${param_idx}")
                params.append(value)
            param_idx += 1

        params.append(segment_id)
        query = (
            f"UPDATE segments SET {_build_safe_set_clause(updates)} WHERE segment_id = ${param_idx}"
        )

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def set_segment_index_state(
        self,
        segment_id: str,
        state: str,
        *,
        error: str | None = None,
        connection: Any | None = None,
    ) -> None:
        """Commit the fail-closed vector synchronization state for a segment.

        ``enabled`` is deliberately preserved: editing a manually disabled
        segment must not re-enable it. Retrieval requires both enabled=true and
        status=completed, so pending/error states hide any stale Qdrant payload.
        """

        normalized_segment = str(segment_id or "").strip()
        if not normalized_segment:
            raise ValueError("segment_id is required")
        if state not in {"pending", "completed", "error"}:
            raise ValueError("segment index state must be pending, completed, or error")
        if not self._pool:
            raise RuntimeError("database is not connected")

        status = {
            "pending": "indexing",
            "completed": "completed",
            "error": "error",
        }[state]
        error_value = None if state != "error" else str(error or "vector update failed")[:1000]

        async def _update(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                UPDATE segments
                SET status = $2,
                    error = $3,
                    updated_at = NOW()
                WHERE segment_id = $1
                RETURNING segment_id
                """,
                normalized_segment,
                status,
                error_value,
            )

        if connection is not None:
            row = await _update(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _update(conn)
        if not row:
            raise RuntimeError("segment disappeared while updating vector synchronization state")

    async def delete_segment(
        self,
        segment_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """删除 Segment"""
        if not self._pool:
            return False

        async def _delete(conn: Any) -> bool:
            result = await conn.execute(
                "DELETE FROM segments WHERE segment_id = $1",
                segment_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)

    async def save_image_segment(self, segment_data: dict[str, Any]) -> None:
        """保存图片段到数据库

        Args:
            segment_data: 包含以下字段的字典:
                - segment_id: 段ID (必需)
                - document_id: 文档ID (必需)
                - dataset_id: 数据集ID (可选，如未提供会从文档中查询)
                - content_type: 内容类型，默认 'image'
                - image_url: 图片存储URL
                - image_attachment_id: Confluence附件ID
                - image_filename: 文件名
                - image_media_type: MIME类型
                - image_file_size: 文件大小
                - vector_id: 向量ID (可选)
                - text: 上下文文本 (可选)
        """
        if not self._pool:
            return

        # 获取 dataset_id，如果未提供则从文档中查询
        dataset_id = segment_data.get("dataset_id")
        if not dataset_id:
            document = await self.get_document(segment_data.get("document_id"))
            if document:
                dataset_id = document.get("dataset_id")

        if not dataset_id:
            raise ValueError("dataset_id is required but could not be determined")

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position,
                    text, token_count, vector_id, metadata,
                    content_type, image_url, image_attachment_id,
                    image_filename, image_media_type, image_file_size,
                    enabled, status
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8,
                    $9, $10, $11, $12, $13, $14, $15, $16
                )
                ON CONFLICT (segment_id) DO UPDATE SET
                    dataset_id = EXCLUDED.dataset_id,
                    document_id = EXCLUDED.document_id,
                    vector_id = EXCLUDED.vector_id,
                    content_type = EXCLUDED.content_type,
                    image_url = EXCLUDED.image_url,
                    image_attachment_id = EXCLUDED.image_attachment_id,
                    image_filename = EXCLUDED.image_filename,
                    image_media_type = EXCLUDED.image_media_type,
                    image_file_size = EXCLUDED.image_file_size,
                    updated_at = NOW()
                """,
                segment_data.get("segment_id"),
                dataset_id,
                segment_data.get("document_id"),
                segment_data.get("position", 0),
                segment_data.get("text", ""),  # context text
                segment_data.get("token_count", 0),
                segment_data.get("vector_id"),
                json.dumps(segment_data.get("metadata", {})),
                segment_data.get("content_type", "image"),
                segment_data.get("image_url"),
                segment_data.get("image_attachment_id"),
                segment_data.get("image_filename"),
                segment_data.get("image_media_type"),
                segment_data.get("image_file_size"),
                True,  # enabled
                "completed",  # status
            )

    async def get_image_segments_by_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> list[dict[str, Any]]:
        """获取文档的所有图片段

        Args:
            document_id: 文档ID

        Returns:
            图片段列表，每个包含 segment_id, image_attachment_id, metadata 等
        """
        if not self._pool:
            return []

        async def _get(conn: Any) -> list[dict[str, Any]]:
            rows = await conn.fetch(
                """
                SELECT segment_id, document_id, dataset_id, position,
                       text, vector_id, metadata, content_type,
                       image_url, image_attachment_id, image_filename,
                       image_media_type, image_file_size,
                       created_at, updated_at
                FROM segments
                WHERE document_id = $1 AND content_type = 'image'
                ORDER BY position
                """,
                document_id,
            )
            return [dict(row) for row in rows]

        if connection is not None:
            return await _get(connection)
        async with self._pool.acquire() as conn:
            return await _get(conn)

    async def delete_image_segments_by_document(self, document_id: str) -> int:
        """删除文档的所有图片段

        Args:
            document_id: 文档ID

        Returns:
            删除的段数量
        """
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM segments
                WHERE document_id = $1 AND content_type = 'image'
                """,
                document_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    async def count_segments_by_document(self, document_id: str) -> int:
        """Count total segments for a document.

        Args:
            document_id: The document ID

        Returns:
            Total segment count for the document
        """
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT COUNT(*) as cnt FROM segments WHERE document_id = $1",
                document_id,
            )
            return int(row["cnt"]) if row else 0

    async def refresh_document_segment_count(self, document_id: str) -> int:
        """Recount segments and update the document's segment_count field.

        This method counts all segments (text + image) for a document and updates
        the documents.segment_count field atomically.

        Args:
            document_id: The document ID

        Returns:
            The new segment count
        """
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            # Use a single atomic UPDATE with subquery for consistency
            row = await conn.fetchrow(
                """
                UPDATE documents
                SET segment_count = (
                    SELECT COUNT(*) FROM segments WHERE document_id = $1
                ),
                updated_at = NOW()
                WHERE document_id = $1
                RETURNING segment_count
                """,
                document_id,
            )
            return int(row["segment_count"]) if row else 0

    # =========================================================================
    # Segment Image Associations (segment_images) - P3 Multimodal RAG
    # =========================================================================

    async def add_segment_image_association(
        self,
        segment_id: str,
        image_segment_id: str,
        position: int = 0,
        proximity_score: float = 1.0,
        char_offset: int = 0,
        page_number: int | None = None,
        *,
        dataset_id: str,
        tenant_id: str,
    ) -> bool:
        """Associate an image segment with a text segment.

        Args:
            segment_id: The text segment ID
            image_segment_id: The image segment ID to associate
            position: Position of image in the chunk context (0-indexed)
            proximity_score: Relevance score [0,1] - 1.0 = inline, 0.5 = same page
            char_offset: Character offset in source document
            page_number: Page number in multi-page documents

        Returns:
            True if association was created/updated
        """
        if not self._pool:
            return False
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise ValueError("dataset_id and tenant_id are required for segment association")

        async with self._pool.acquire() as conn:
            try:
                result = await conn.execute(
                    """
                    INSERT INTO segment_images (
                        segment_id, image_segment_id, position,
                        proximity_score, char_offset, page_number
                    )
                    SELECT $1, $2, $3, $4, $5, $6
                    FROM segments AS source_s
                    JOIN segments AS image_s
                      ON image_s.segment_id = $2
                     AND image_s.dataset_id = source_s.dataset_id
                    JOIN datasets AS ds ON ds.dataset_id = source_s.dataset_id
                    WHERE source_s.segment_id = $1
                      AND source_s.dataset_id = $7
                      AND ds.tenant_id = $8
                      AND ds.is_deleted = FALSE
                    ON CONFLICT (segment_id, image_segment_id) DO UPDATE SET
                        position = EXCLUDED.position,
                        proximity_score = EXCLUDED.proximity_score,
                        char_offset = EXCLUDED.char_offset,
                        page_number = EXCLUDED.page_number
                    """,
                    segment_id,
                    image_segment_id,
                    position,
                    proximity_score,
                    char_offset,
                    page_number,
                    normalized_dataset,
                    normalized_tenant,
                )
                return result.rsplit(" ", 1)[-1] == "1"
            except Exception:
                return False

    async def add_segment_image_associations_batch(
        self,
        associations: list[dict[str, Any]],
        *,
        dataset_id: str,
        tenant_id: str,
    ) -> int:
        """Add multiple image associations in batch.

        Args:
            associations: List of dicts with keys:
                - segment_id: Text segment ID
                - image_segment_id: Image segment ID
                - position: Position (optional, default 0)
                - proximity_score: Score (optional, default 1.0)
                - char_offset: Offset (optional, default 0)
                - page_number: Page (optional)

        Returns:
            Number of associations created/updated
        """
        if not self._pool or not associations:
            return 0
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise ValueError("dataset_id and tenant_id are required for segment associations")

        async with self._pool.acquire() as conn:
            count = 0
            # Use a transaction for batch insert
            async with conn.transaction():
                for assoc in associations:
                    try:
                        result = await conn.execute(
                            """
                            INSERT INTO segment_images (
                                segment_id, image_segment_id, position,
                                proximity_score, char_offset, page_number
                            )
                            SELECT $1, $2, $3, $4, $5, $6
                            FROM segments AS source_s
                            JOIN segments AS image_s
                              ON image_s.segment_id = $2
                             AND image_s.dataset_id = source_s.dataset_id
                            JOIN datasets AS ds ON ds.dataset_id = source_s.dataset_id
                            WHERE source_s.segment_id = $1
                              AND source_s.dataset_id = $7
                              AND ds.tenant_id = $8
                              AND ds.is_deleted = FALSE
                            ON CONFLICT (segment_id, image_segment_id) DO UPDATE SET
                                position = EXCLUDED.position,
                                proximity_score = EXCLUDED.proximity_score,
                                char_offset = EXCLUDED.char_offset,
                                page_number = EXCLUDED.page_number
                            """,
                            assoc.get("segment_id"),
                            assoc.get("image_segment_id"),
                            assoc.get("position", 0),
                            assoc.get("proximity_score", 1.0),
                            assoc.get("char_offset", 0),
                            assoc.get("page_number"),
                            normalized_dataset,
                            normalized_tenant,
                        )
                        if result.rsplit(" ", 1)[-1] == "1":
                            count += 1
                    except Exception:
                        continue
            return count

    async def get_segment_associated_images(self, segment_id: str) -> list[dict[str, Any]]:
        """Get all images associated with a text segment.

        Args:
            segment_id: The text segment ID

        Returns:
            List of associated image info including image segment details
        """
        if not self._pool:
            return []

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT
                    si.image_segment_id,
                    si.position,
                    si.proximity_score,
                    si.char_offset,
                    si.page_number,
                    s.image_url AS storage_url,
                    s.image_filename AS filename,
                    s.image_media_type AS media_type,
                    s.text AS vlm_description
                FROM segment_images si
                JOIN segments s ON s.segment_id = si.image_segment_id
                WHERE si.segment_id = $1
                ORDER BY si.proximity_score DESC, si.position
                """,
                segment_id,
            )
            return [dict(row) for row in rows]

    async def get_segment_associations_batch(
        self,
        segment_ids: list[str],
        *,
        dataset_id: str,
        tenant_id: str,
    ) -> dict[str, list[dict[str, Any]]]:
        """Get associated images for multiple segments efficiently.

        Args:
            segment_ids: List of text segment IDs

        Returns:
            Dict mapping segment_id -> list of associated image info
        """
        if not self._pool or not segment_ids:
            return {}
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise ValueError("dataset_id and tenant_id are required for segment associations")

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT
                    si.segment_id,
                    si.image_segment_id,
                    si.position,
                    si.proximity_score,
                    si.char_offset,
                    si.page_number,
                    image_s.image_url AS storage_url,
                    image_s.image_filename AS filename,
                    image_s.image_media_type AS media_type,
                    image_s.text AS vlm_description
                FROM segment_images si
                JOIN segments AS source_s ON source_s.segment_id = si.segment_id
                JOIN segments AS image_s
                  ON image_s.segment_id = si.image_segment_id
                 AND image_s.dataset_id = source_s.dataset_id
                JOIN documents AS source_d
                  ON source_d.document_id = source_s.document_id
                 AND source_d.dataset_id = source_s.dataset_id
                JOIN documents AS image_d
                  ON image_d.document_id = image_s.document_id
                 AND image_d.dataset_id = image_s.dataset_id
                JOIN datasets AS ds ON ds.dataset_id = source_s.dataset_id
                WHERE si.segment_id = ANY($1::text[])
                  AND source_s.dataset_id = $2
                  AND ds.tenant_id = $3
                  AND ds.is_deleted = FALSE
                  AND COALESCE(source_s.enabled, TRUE) = TRUE
                  AND COALESCE(image_s.enabled, TRUE) = TRUE
                  AND source_s.status = 'completed'
                  AND image_s.status = 'completed'
                  AND COALESCE(source_d.enabled, TRUE) = TRUE
                  AND COALESCE(source_d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(source_d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                  AND COALESCE(image_d.enabled, TRUE) = TRUE
                  AND COALESCE(image_d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(image_d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                ORDER BY si.segment_id, si.proximity_score DESC, si.position
                """,
                segment_ids,
                normalized_dataset,
                normalized_tenant,
            )

            result: dict[str, list[dict[str, Any]]] = {sid: [] for sid in segment_ids}
            for row in rows:
                seg_id = row["segment_id"]
                if seg_id in result:
                    result[seg_id].append(dict(row))
            return result

    async def delete_segment_image_associations(self, segment_id: str) -> int:
        """Delete all image associations for a text segment.

        Args:
            segment_id: The text segment ID

        Returns:
            Number of associations deleted
        """
        if not self._pool:
            return 0

        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM segment_images WHERE segment_id = $1",
                segment_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    async def delete_image_associations_by_document(self, document_id: str) -> int:
        """Delete all image associations for segments in a document.

        Args:
            document_id: The document ID

        Returns:
            Number of associations deleted
        """
        if not self._pool:
            return 0

        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM segment_images
                WHERE segment_id IN (
                    SELECT segment_id FROM segments WHERE document_id = $1
                )
                """,
                document_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    async def update_segment_image_flags(self, segment_id: str) -> None:
        """Update has_images and image_count flags for a segment.

        This should be called after adding/removing image associations.

        Args:
            segment_id: The text segment ID
        """
        if not self._pool:
            return

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE segments
                SET
                    has_images = (SELECT COUNT(*) > 0 FROM segment_images WHERE segment_id = $1),
                    image_count = (SELECT COUNT(*) FROM segment_images WHERE segment_id = $1)
                WHERE segment_id = $1
                """,
                segment_id,
            )

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
                ) VALUES ($1, $2, $3::jsonb, $4::jsonb, $5)
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
    # Confluence 集成表
    # =========================================================================

    async def save_confluence_connection(self, connection: dict[str, Any]) -> None:
        """保存或更新 Confluence 连接配置"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO confluence_connections (
                    connection_id, tenant_id, name, domain, email, api_token,
                    sync_mode, polling_interval_minutes, status,
                    last_sync_at, last_error, created_by, owner_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                ON CONFLICT (connection_id) DO UPDATE SET
                    name = EXCLUDED.name,
                    domain = EXCLUDED.domain,
                    email = EXCLUDED.email,
                    api_token = EXCLUDED.api_token,
                    sync_mode = EXCLUDED.sync_mode,
                    polling_interval_minutes = EXCLUDED.polling_interval_minutes,
                    status = EXCLUDED.status,
                    last_sync_at = EXCLUDED.last_sync_at,
                    last_error = EXCLUDED.last_error,
                    updated_at = NOW()
            """,
                connection.get("connection_id"),
                connection.get("tenant_id", ""),
                connection.get("name"),
                connection.get("domain"),
                connection.get("email"),
                connection.get("api_token"),
                connection.get("sync_mode", "manual"),
                connection.get("polling_interval_minutes", 60),
                connection.get("status", "active"),
                connection.get("last_sync_at"),
                connection.get("last_error"),
                connection.get("created_by"),
                # owner_id: 优先使用显式设置的 owner_id，否则回退到 created_by
                connection.get("owner_id") or connection.get("created_by"),
            )

    async def get_confluence_connection(
        self,
        connection_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """获取 Confluence 连接。可复用已持有生命周期锁的连接。"""
        if connection is not None:
            row = await connection.fetchrow(
                "SELECT * FROM confluence_connections WHERE connection_id = $1", connection_id
            )
            return self._row_to_dict(row) if row else None
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM confluence_connections WHERE connection_id = $1", connection_id
            )
            return self._row_to_dict(row) if row else None

    async def list_confluence_connections(
        self,
        tenant_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """列出 Confluence 连接"""
        if not self._pool:
            return []

        query = "SELECT * FROM confluence_connections WHERE 1=1"
        params: list[Any] = []
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

    async def get_confluence_connections_with_polling(self) -> list[dict[str, Any]]:
        """获取启用轮询的 Confluence 连接"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch("""
                SELECT * FROM confluence_connections
                WHERE sync_mode = 'polling' AND status = 'active'
                ORDER BY created_at
            """)
            return [self._row_to_dict(row) for row in rows]

    async def update_confluence_connection_status(
        self,
        connection_id: str,
        status: str,
        last_sync_at: datetime | None = None,
        last_error: str | None = None,
    ) -> None:
        """更新 Confluence 连接状态"""
        if not self._pool:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params: list[Any] = [status]
        param_idx = 2

        if last_sync_at:
            updates.append(f"last_sync_at = ${param_idx}")
            params.append(last_sync_at)
            param_idx += 1

        if last_error is not None:
            updates.append(f"last_error = ${param_idx}")
            params.append(last_error if last_error else None)
            param_idx += 1

        params.append(connection_id)
        query = f"UPDATE confluence_connections SET {_build_safe_set_clause(updates)} WHERE connection_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def delete_confluence_connection(self, connection_id: str) -> bool:
        """删除 Confluence 连接（级联删除绑定）"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM confluence_connections WHERE connection_id = $1", connection_id
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

    async def update_confluence_connection(
        self,
        connection_id: str,
        updates: dict[str, Any],
    ) -> None:
        """更新 Confluence 连接配置"""
        if not self._pool or not updates:
            return

        set_clauses = []
        params: list[Any] = []
        param_idx = 1

        allowed_fields = {
            "name",
            "domain",
            "email",
            "api_token",
            "sync_mode",
            "polling_interval_minutes",
            "status",
            "last_sync_at",
            "last_error",
        }

        for key, value in updates.items():
            if key in allowed_fields:
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(value)
                param_idx += 1

        if not set_clauses:
            return

        set_clauses.append("updated_at = NOW()")
        params.append(connection_id)

        query = f"UPDATE confluence_connections SET {_build_safe_set_clause(set_clauses)} WHERE connection_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def find_confluence_connection_by_domain(
        self,
        domain: str,
        tenant_id: str | None = None,
    ) -> dict[str, Any] | None:
        """根据域名查找连接"""
        if not self._pool:
            return None

        query = "SELECT * FROM confluence_connections WHERE domain = $1"
        params: list[Any] = [domain]

        if tenant_id:
            query += " AND tenant_id = $2"
            params.append(tenant_id)

        query += " AND status = 'active' ORDER BY created_at DESC LIMIT 1"

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, *params)
            return self._row_to_dict(row) if row else None

    # =========================================================================
    # Confluence Space Binding 表
    # =========================================================================

    async def save_confluence_binding(self, binding: dict[str, Any]) -> dict[str, Any] | None:
        """
        保存或更新 Confluence 空间绑定

        使用 RETURNING 子句在单个事务中完成保存和返回，确保原子性。

        Returns:
            保存后的绑定数据，如果失败返回 None
        """
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO confluence_space_bindings (
                    binding_id, connection_id, tenant_id, dataset_id, space_key, space_id,
                    space_name, root_page_id, root_page_title,
                    include_patterns, exclude_patterns, max_depth,
                    include_attachments, include_comments, status,
                    last_sync_at, synced_page_count, total_page_count,
                    last_error, created_by, owner_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)
                ON CONFLICT (binding_id) DO UPDATE SET
                    space_id = EXCLUDED.space_id,
                    space_name = EXCLUDED.space_name,
                    root_page_id = EXCLUDED.root_page_id,
                    root_page_title = EXCLUDED.root_page_title,
                    include_patterns = EXCLUDED.include_patterns,
                    exclude_patterns = EXCLUDED.exclude_patterns,
                    max_depth = EXCLUDED.max_depth,
                    include_attachments = EXCLUDED.include_attachments,
                    include_comments = EXCLUDED.include_comments,
                    status = EXCLUDED.status,
                    last_sync_at = EXCLUDED.last_sync_at,
                    synced_page_count = EXCLUDED.synced_page_count,
                    total_page_count = EXCLUDED.total_page_count,
                    last_error = EXCLUDED.last_error,
                    updated_at = NOW()
                RETURNING *
            """,
                binding.get("binding_id"),
                binding.get("connection_id"),
                binding.get("tenant_id"),
                binding.get("dataset_id"),
                binding.get("space_key"),
                binding.get("space_id"),
                binding.get("space_name"),
                binding.get("root_page_id"),
                binding.get("root_page_title"),
                json.dumps(binding.get("include_patterns", [])),
                json.dumps(binding.get("exclude_patterns", [])),
                binding.get("max_depth", 10),
                binding.get("include_attachments", False),
                binding.get("include_comments", False),
                binding.get("status", "pending"),
                binding.get("last_sync_at"),
                binding.get("synced_page_count", 0),
                binding.get("total_page_count", 0),
                binding.get("last_error"),
                binding.get("created_by"),
                # owner_id: 优先使用显式设置的 owner_id，否则回退到 created_by
                binding.get("owner_id") or binding.get("created_by"),
            )
            return self._row_to_dict(row) if row else None

    async def get_confluence_binding(
        self,
        binding_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """获取 Confluence 空间绑定。可复用已持有生命周期锁的连接。"""
        if connection is not None:
            row = await connection.fetchrow(
                "SELECT * FROM confluence_space_bindings WHERE binding_id = $1", binding_id
            )
            return self._row_to_dict(row) if row else None
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM confluence_space_bindings WHERE binding_id = $1", binding_id
            )
            return self._row_to_dict(row) if row else None

    async def get_confluence_bindings_by_connection(
        self, connection_id: str
    ) -> list[dict[str, Any]]:
        """获取连接下的所有空间绑定"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_space_bindings
                WHERE connection_id = $1
                ORDER BY created_at
            """,
                connection_id,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_confluence_bindings_by_dataset(self, dataset_id: str) -> list[dict[str, Any]]:
        """获取数据集关联的所有空间绑定"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_space_bindings
                WHERE dataset_id = $1
                ORDER BY created_at
            """,
                dataset_id,
            )
            return [self._row_to_dict(row) for row in rows]

    async def update_confluence_binding_status(
        self,
        binding_id: str,
        status: str,
        synced_page_count: int | None = None,
        total_page_count: int | None = None,
        last_sync_at: datetime | None = None,
        last_error: str | None = None,
    ) -> None:
        """更新 Confluence 空间绑定状态"""
        if not self._pool:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params: list[Any] = [status]
        param_idx = 2

        if synced_page_count is not None:
            updates.append(f"synced_page_count = ${param_idx}")
            params.append(synced_page_count)
            param_idx += 1

        if total_page_count is not None:
            updates.append(f"total_page_count = ${param_idx}")
            params.append(total_page_count)
            param_idx += 1

        if last_sync_at:
            updates.append(f"last_sync_at = ${param_idx}")
            params.append(last_sync_at)
            param_idx += 1

        if last_error is not None:
            updates.append(f"last_error = ${param_idx}")
            params.append(last_error if last_error else None)
            param_idx += 1

        params.append(binding_id)
        query = f"UPDATE confluence_space_bindings SET {_build_safe_set_clause(updates)} WHERE binding_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def delete_confluence_binding(self, binding_id: str) -> bool:
        """删除 Confluence 空间绑定（级联删除页面记录）"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM confluence_space_bindings WHERE binding_id = $1", binding_id
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

    async def update_confluence_binding(
        self,
        binding_id: str,
        updates: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        """更新 Confluence 空间绑定"""
        if not self._pool or not updates:
            return

        set_clauses = []
        params: list[Any] = []
        param_idx = 1

        allowed_fields = {
            "space_id",
            "space_name",
            "include_patterns",
            "exclude_patterns",
            "max_depth",
            "include_attachments",
            "include_comments",
            "status",
            "last_sync_at",
            "synced_page_count",
            "total_page_count",
            "last_error",
            "root_page_id",
            "root_page_title",
            "sync_mode",
            "polling_interval_minutes",
            "last_incremental_sync_at",  # binding 级别同步配置
            "sync_enabled",
            "next_sync_at",  # 调度器相关
        }

        # JSON 字段需要序列化
        json_fields = {"include_patterns", "exclude_patterns"}

        for key, value in updates.items():
            if key in allowed_fields:
                set_clauses.append(f"{key} = ${param_idx}")
                # JSON 字段需要序列化为字符串
                if key in json_fields and isinstance(value, (list, dict)):
                    params.append(json.dumps(value))
                else:
                    params.append(value)
                param_idx += 1

        if not set_clauses:
            return

        set_clauses.append("updated_at = NOW()")
        params.append(binding_id)

        query = f"UPDATE confluence_space_bindings SET {_build_safe_set_clause(set_clauses)} WHERE binding_id = ${param_idx}"

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def list_confluence_bindings(
        self,
        connection_id: str | None = None,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """列出 Confluence 空间绑定"""
        if not self._pool:
            return []

        query = "SELECT * FROM confluence_space_bindings WHERE 1=1"
        params: list[Any] = []
        param_idx = 1

        if connection_id:
            query += f" AND connection_id = ${param_idx}"
            params.append(connection_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if dataset_id:
            query += f" AND dataset_id = ${param_idx}"
            params.append(dataset_id)
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

    # =========================================================================
    # Confluence Page 表
    # =========================================================================

    @contextlib.asynccontextmanager
    async def _confluence_page_reference_write(
        self,
        binding_id: str,
        document_id: str | None,
        *,
        connection: Any | None = None,
    ):
        """Fence reference publication on the existing dataset lifecycle lock.

        A supplied owner-lease connection stays in use throughout; a nested
        transaction is a savepoint and cannot release the outer transaction's
        advisory lock. Read binding identity again after acquiring the lock so
        a deleted or replaced binding never lends stale dataset authority.
        """
        if not binding_id or not str(binding_id).strip():
            raise ValueError("Confluence binding_id is required")
        if document_id is not None and not str(document_id).strip():
            raise ValueError("Confluence document_id must not be empty")

        @contextlib.asynccontextmanager
        async def write_connection():
            if connection is not None:
                yield connection
            else:
                if not self._pool:
                    raise RuntimeError("database is not connected")
                async with self._pool.acquire() as acquired:
                    yield acquired

        async with write_connection() as conn, conn.transaction():
            binding = await conn.fetchrow(
                "SELECT dataset_id FROM confluence_space_bindings WHERE binding_id = $1",
                binding_id,
            )
            if not binding:
                raise RuntimeError("Confluence binding is missing before reference write")
            dataset_id = str(binding["dataset_id"] or "")
            acquired = await conn.fetchval(
                "SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1, 0))",
                self._dataset_index_lock_name(dataset_id),
            )
            if acquired is not True:
                raise IndexLeaseUnavailableError(
                    "dataset index lifecycle work is in progress; refusing Confluence reference write"
                )
            current = await conn.fetchrow(
                "SELECT dataset_id FROM confluence_space_bindings "
                "WHERE binding_id = $1 FOR SHARE",
                binding_id,
            )
            if not current or str(current["dataset_id"] or "") != dataset_id:
                raise RuntimeError("Confluence binding changed before reference write")
            await self._require_dataset_ingestion_identity(conn, dataset_id, None)
            if document_id is not None:
                document = await conn.fetchrow(
                    "SELECT dataset_id FROM documents WHERE document_id = $1 FOR SHARE",
                    document_id,
                )
                if not document or str(document["dataset_id"] or "") != dataset_id:
                    raise RuntimeError("Confluence document is missing or belongs to another dataset")
            yield conn

    async def save_confluence_page(
        self,
        page: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        """保存或更新 Confluence 页面记录"""
        if not self._pool and connection is None:
            return
        async with self._confluence_page_reference_write(
            page.get("binding_id"), page.get("document_id"), connection=connection
        ) as conn:
            await conn.execute(
                """
                INSERT INTO confluence_pages (
                    id, binding_id, document_id, page_id, space_key, title,
                    version, content_hash, parent_page_id, depth, status,
                    last_synced_at, confluence_updated_at, error, labels, web_url, author
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                ON CONFLICT (binding_id, page_id) DO UPDATE SET
                    document_id = EXCLUDED.document_id,
                    title = EXCLUDED.title,
                    version = EXCLUDED.version,
                    content_hash = EXCLUDED.content_hash,
                    status = EXCLUDED.status,
                    last_synced_at = EXCLUDED.last_synced_at,
                    confluence_updated_at = EXCLUDED.confluence_updated_at,
                    error = EXCLUDED.error,
                    labels = EXCLUDED.labels,
                    web_url = EXCLUDED.web_url,
                    updated_at = NOW()
            """,
                page.get("id"),
                page.get("binding_id"),
                page.get("document_id"),
                page.get("page_id"),
                page.get("space_key"),
                page.get("title"),
                page.get("version", 1),
                page.get("content_hash"),
                page.get("parent_page_id"),
                page.get("depth", 0),
                page.get("status", "pending"),
                page.get("last_synced_at"),
                page.get("confluence_updated_at"),
                page.get("error"),
                json.dumps(page.get("labels", [])),
                page.get("web_url"),
                page.get("author"),
            )

    async def get_confluence_page(self, page_record_id: str) -> dict[str, Any] | None:
        """通过记录 ID 获取 Confluence 页面记录"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM confluence_pages WHERE id = $1", page_record_id
            )
            return self._row_to_dict(row) if row else None

    async def get_confluence_page_by_binding_and_page(
        self, binding_id: str, page_id: str
    ) -> dict[str, Any] | None:
        """通过绑定 ID 和页面 ID 获取 Confluence 页面记录"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM confluence_pages
                WHERE binding_id = $1 AND page_id = $2
            """,
                binding_id,
                page_id,
            )
            return self._row_to_dict(row) if row else None

    async def get_confluence_page_by_document(self, document_id: str) -> dict[str, Any] | None:
        """根据文档 ID 获取 Confluence 页面记录"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM confluence_pages WHERE document_id = $1", document_id
            )
            return self._row_to_dict(row) if row else None

    async def list_confluence_pages(
        self,
        binding_id: str,
        status: str | None = None,
        limit: int | None = 100,
        offset: int = 0,
        synced_only: bool = False,
    ) -> list[dict[str, Any]]:
        """
        列出 Confluence 页面记录

        Args:
            binding_id: 绑定 ID
            status: 筛选状态
            limit: 返回数量限制；内部同步传 None 读取完整绑定快照
            offset: 偏移量
            synced_only: 如果为 True，只返回有 document_id 的记录（已入库的）

        返回结果包含关联文档的处理状态:
        - document_status: 文档的实际处理状态 (waiting/parsing/splitting/indexing/completed/error)
        - document_progress: 文档处理进度 (0-100)
        - effective_status: 计算后的有效状态，检测是否需要重新同步
          当页面标记为 'synced' 但关联文档不存在或没有任何 segment 时，
          返回 'needs_resync' 表示需要重新同步
        """
        if not self._pool:
            return []

        # JOIN documents 表获取关联文档的处理状态
        # 计算 effective_status: 检测 synced 状态但实际无数据的情况
        query = """
            SELECT
                cp.*,
                d.status AS document_status,
                d.progress AS document_progress,
                CASE
                    WHEN cp.status = 'synced'
                         AND cp.document_id IS NOT NULL
                         AND (d.document_id IS NULL OR COALESCE(d.segment_count, 0) = 0)
                    THEN 'needs_resync'
                    ELSE cp.status
                END AS effective_status
            FROM confluence_pages cp
            LEFT JOIN documents d ON cp.document_id = d.document_id
            WHERE cp.binding_id = $1
        """
        params: list[Any] = [binding_id]
        param_idx = 2

        # 只返回已入库的页面（有 document_id）
        if synced_only:
            query += " AND cp.document_id IS NOT NULL"

        if status:
            query += f" AND cp.status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY cp.title ASC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def cleanup_unsynced_confluence_pages(
        self,
        binding_id: str,
    ) -> int:
        """
        清理未同步的 Confluence 页面记录

        删除所有 document_id 为空的记录（从未真正同步到知识库的页面）

        Args:
            binding_id: 绑定 ID

        Returns:
            删除的记录数
        """
        if not self._pool:
            return 0

        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM confluence_pages
                WHERE binding_id = $1 AND document_id IS NULL
                """,
                binding_id,
            )
            # Parse result like "DELETE 5"
            try:
                return int(result.split()[-1])
            except (ValueError, IndexError):
                return 0

    async def update_confluence_page_status(
        self,
        id: str,
        status: str,
        document_id: str | None = None,
        content_hash: str | None = None,
        version: int | None = None,
        last_synced_at: datetime | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        """更新 Confluence 页面状态"""
        if not self._pool and connection is None:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params: list[Any] = [status]
        param_idx = 2

        if document_id is not None:
            updates.append(f"document_id = ${param_idx}")
            params.append(document_id)
            param_idx += 1

        if content_hash is not None:
            updates.append(f"content_hash = ${param_idx}")
            params.append(content_hash)
            param_idx += 1

        if version is not None:
            updates.append(f"version = ${param_idx}")
            params.append(version)
            param_idx += 1

        if last_synced_at:
            updates.append(f"last_synced_at = ${param_idx}")
            params.append(last_synced_at)
            param_idx += 1

        if error is not None:
            updates.append(f"error = ${param_idx}")
            params.append(error if error else None)
            param_idx += 1

        params.append(id)
        query = (
            f"UPDATE confluence_pages SET {_build_safe_set_clause(updates)} WHERE id = ${param_idx}"
        )

        async def _update(conn: Any) -> None:
            if document_id is None:
                # Status/error observation does not create a reference or own
                # document deletion, and must not acquire deletion authority.
                await conn.execute(query, *params)
                return
            page = await conn.fetchrow(
                "SELECT binding_id, page_id FROM confluence_pages WHERE id = $1", id
            )
            if not page:
                raise RuntimeError("Confluence page is missing before reference write")
            async with self._confluence_page_reference_write(
                page["binding_id"], document_id, connection=conn
            ):
                row = await conn.fetchrow(
                    query
                    + f" AND binding_id = ${param_idx + 1} AND page_id = ${param_idx + 2}"
                    + " RETURNING id",
                    *params,
                    page["binding_id"],
                    page["page_id"],
                )
                if not row:
                    raise RuntimeError("Confluence page changed before reference write")

        if connection is not None:
            await _update(connection)
            return
        async with self._pool.acquire() as conn:
            await _update(conn)

    async def update_confluence_page_image_count(
        self,
        binding_id: str,
        page_id: str,
        image_count: int,
    ) -> None:
        """更新 Confluence 页面的图片数量

        Args:
            binding_id: 绑定ID
            page_id: Confluence页面ID
            image_count: 图片数量（必须 >= 0）
        """
        if not self._pool:
            return
        # Ensure non-negative image count
        if image_count < 0:
            image_count = 0
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE confluence_pages
                SET image_count = $1, images_synced_at = NOW()
                WHERE binding_id = $2 AND page_id = $3
                """,
                image_count,
                binding_id,
                page_id,
            )

    async def delete_confluence_page(self, id: str) -> bool:
        """删除 Confluence 页面记录"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute("DELETE FROM confluence_pages WHERE id = $1", id)
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

    async def delete_confluence_pages_by_binding(self, binding_id: str) -> int:
        """删除绑定下的所有页面记录"""
        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM confluence_pages WHERE binding_id = $1", binding_id
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0

    async def get_confluence_page_by_page_id(
        self,
        binding_id: str,
        page_id: str,
    ) -> dict[str, Any] | None:
        """根据 Confluence page_id 获取页面记录"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM confluence_pages
                WHERE binding_id = $1 AND page_id = $2
            """,
                binding_id,
                page_id,
            )
            return self._row_to_dict(row) if row else None

    async def update_confluence_page_sync_config(
        self,
        page_id: str,
        updates: dict[str, Any],
    ) -> dict[str, Any] | None:
        """更新页面级同步配置"""
        if not self._pool:
            return None

        allowed_fields = {
            "sync_mode",
            "polling_interval_minutes",
            "sync_enabled",
            "next_sync_at",
            "sync_priority",
        }

        set_clauses = ["updated_at = NOW()"]
        params: list[Any] = []
        param_idx = 1

        for key, value in updates.items():
            if key in allowed_fields:
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(value)
                param_idx += 1

        if len(set_clauses) == 1:  # Only updated_at
            return None

        params.append(page_id)
        query = f"""
            UPDATE confluence_pages
            SET {", ".join(set_clauses)}
            WHERE id = ${param_idx}
            RETURNING *
        """

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, *params)
            return self._row_to_dict(row) if row else None

    async def get_bindings_due_for_sync(self, limit: int = 100) -> list[dict[str, Any]]:
        """获取需要同步的绑定列表（next_sync_at <= now）"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_space_bindings
                WHERE sync_enabled = TRUE
                  AND sync_mode = 'polling'
                  AND next_sync_at IS NOT NULL
                  AND next_sync_at <= NOW()
                ORDER BY next_sync_at ASC
                LIMIT $1
            """,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_pages_due_for_sync(self, limit: int = 100) -> list[dict[str, Any]]:
        """获取需要同步的页面列表（有独立配置且 next_sync_at <= now）"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_pages
                WHERE sync_enabled = TRUE
                  AND sync_mode = 'polling'
                  AND next_sync_at IS NOT NULL
                  AND next_sync_at <= NOW()
                ORDER BY sync_priority DESC, next_sync_at ASC
                LIMIT $1
            """,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_all_polling_pages(self, limit: int = 500) -> list[dict[str, Any]]:
        """
        获取所有启用轮询的页面（用于调度器初始化）

        与 get_pages_due_for_sync 不同，此方法返回所有启用轮询的页面，
        无论 next_sync_at 是否已到期。调度器会根据 next_sync_at 决定何时执行。

        Returns:
            所有 sync_enabled=TRUE AND sync_mode='polling' 的页面
        """
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_pages
                WHERE sync_enabled = TRUE
                  AND sync_mode = 'polling'
                ORDER BY sync_priority DESC, next_sync_at ASC NULLS FIRST
                LIMIT $1
            """,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    async def schedule_next_sync(
        self,
        table: str,
        id_field: str,
        id_value: str,
        interval_minutes: int,
    ) -> None:
        """设置下次同步时间

        Args:
            table: 表名（仅支持白名单中的表）
            id_field: ID 字段名（仅支持白名单中的字段）
            id_value: ID 值
            interval_minutes: 间隔分钟数

        Raises:
            ValueError: 如果表名或字段名不在白名单中
        """
        if not self._pool:
            return

        # 白名单验证，防止 SQL 注入
        allowed_tables = {"confluence_space_bindings", "confluence_pages"}
        allowed_id_fields = {"binding_id", "id"}

        if table not in allowed_tables:
            raise ValueError(f"Invalid table name: {table}")
        if id_field not in allowed_id_fields:
            raise ValueError(f"Invalid id field: {id_field}")

        # 验证 interval_minutes 是有效的整数
        interval_minutes = int(interval_minutes)
        if interval_minutes < 0 or interval_minutes > 10080:  # 最大 7 天
            raise ValueError(f"Invalid interval_minutes: {interval_minutes}")

        async with self._pool.acquire() as conn:
            # 使用参数化的 interval 值
            await conn.execute(
                f"""
                UPDATE {table}
                SET next_sync_at = NOW() + $1 * INTERVAL '1 minute',
                    updated_at = NOW()
                WHERE {id_field} = $2
            """,
                interval_minutes,
                id_value,
            )

    async def upsert_confluence_page(
        self,
        binding_id: str,
        page_id: str,
        document_id: str | None = None,
        space_key: str = "",
        title: str = "",
        version: int = 1,
        content_hash: str | None = None,
        parent_page_id: str | None = None,
        depth: int = 0,
        status: str = "synced",
        labels: list[str] | None = None,
        web_url: str | None = None,
        author: str | None = None,
        confluence_updated_at: str | None = None,
        image_count: int = 0,
        *,
        connection: Any | None = None,
    ) -> None:
        """插入或更新 Confluence 页面记录"""
        if not self._pool and connection is None:
            return

        record_id = f"{binding_id}:{page_id}"

        # 将 ISO 字符串转换为 datetime 对象
        updated_at_dt = None
        if confluence_updated_at:
            if isinstance(confluence_updated_at, str):
                # 处理 ISO 格式: '2025-08-12T04:04:43.266Z'
                updated_at_dt = datetime.fromisoformat(confluence_updated_at.replace("Z", "+00:00"))
            else:
                updated_at_dt = confluence_updated_at

        async def _upsert(conn: Any) -> None:
            await conn.execute(
                """
                INSERT INTO confluence_pages (
                    id, binding_id, document_id, page_id, space_key, title,
                    version, content_hash, parent_page_id, depth, status,
                    last_synced_at, confluence_updated_at, labels, web_url, author,
                    image_count
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                    NOW(), $12, $13, $14, $15, $16
                )
                ON CONFLICT (binding_id, page_id) DO UPDATE SET
                    document_id = EXCLUDED.document_id,
                    title = EXCLUDED.title,
                    version = EXCLUDED.version,
                    content_hash = EXCLUDED.content_hash,
                    status = EXCLUDED.status,
                    last_synced_at = NOW(),
                    confluence_updated_at = EXCLUDED.confluence_updated_at,
                    labels = EXCLUDED.labels,
                    web_url = EXCLUDED.web_url,
                    author = EXCLUDED.author,
                    image_count = EXCLUDED.image_count,
                    updated_at = NOW()
            """,
                record_id,
                binding_id,
                document_id,
                page_id,
                space_key,
                title,
                version,
                content_hash,
                parent_page_id,
                depth,
                status,
                updated_at_dt,
                json.dumps(labels or []),  # JSONB 列需要 JSON 字符串
                web_url,
                author,
                max(int(image_count or 0), 0),
            )

        async with self._confluence_page_reference_write(
            binding_id, document_id, connection=connection
        ) as conn:
            await _upsert(conn)

    async def delete_confluence_page_by_page_id(
        self,
        binding_id: str,
        page_id: str,
    ) -> bool:
        """根据 page_id 删除页面记录"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM confluence_pages
                WHERE binding_id = $1 AND page_id = $2
            """,
                binding_id,
                page_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

    # =========================================================================
    # Confluence Sync Task 表
    # =========================================================================

    async def save_confluence_sync_task(self, task: dict[str, Any]) -> None:
        """保存或更新 Confluence 同步任务"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO confluence_sync_tasks (
                    task_id, binding_id, page_id, task_type, priority,
                    status, retry_count, max_retries, progress,
                    total_items, processed_items, error, result,
                    started_at, completed_at, owner_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                ON CONFLICT (task_id) DO UPDATE SET
                    status = EXCLUDED.status,
                    retry_count = EXCLUDED.retry_count,
                    progress = EXCLUDED.progress,
                    total_items = EXCLUDED.total_items,
                    processed_items = EXCLUDED.processed_items,
                    error = EXCLUDED.error,
                    result = EXCLUDED.result,
                    started_at = EXCLUDED.started_at,
                    completed_at = EXCLUDED.completed_at,
                    updated_at = NOW()
            """,
                task.get("task_id"),
                task.get("binding_id"),
                task.get("page_id"),
                task.get("task_type"),
                task.get("priority", 0),
                task.get("status", "pending"),
                task.get("retry_count", 0),
                task.get("max_retries", 3),
                task.get("progress", 0.0),
                task.get("total_items", 0),
                task.get("processed_items", 0),
                task.get("error"),
                json.dumps(task.get("result")) if task.get("result") else None,
                task.get("started_at"),
                task.get("completed_at"),
                task.get("owner_id"),  # ACL: owner_id for access control
            )

    async def get_confluence_sync_task(
        self,
        task_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """获取 Confluence 同步任务。可复用已持有生命周期锁的连接。"""
        if connection is not None:
            row = await connection.fetchrow(
                "SELECT * FROM confluence_sync_tasks WHERE task_id = $1", task_id
            )
            return self._row_to_dict(row) if row else None
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM confluence_sync_tasks WHERE task_id = $1", task_id
            )
            return self._row_to_dict(row) if row else None

    async def list_confluence_sync_tasks(
        self,
        binding_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """列出 Confluence 同步任务"""
        if not self._pool:
            return []

        query = "SELECT * FROM confluence_sync_tasks WHERE 1=1"
        params: list[Any] = []
        param_idx = 1

        if binding_id:
            query += f" AND binding_id = ${param_idx}"
            params.append(binding_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY priority DESC, created_at ASC LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def update_confluence_sync_task_status(
        self,
        task_id: str,
        status: str,
        progress: float | None = None,
        processed_items: int | None = None,
        error: str | None = None,
        result: dict | None = None,
    ) -> None:
        """更新 Confluence 同步任务状态"""
        if not self._pool:
            return

        updates = ["status = $1", "updated_at = NOW()"]
        params: list[Any] = [status]
        param_idx = 2

        if progress is not None:
            updates.append(f"progress = ${param_idx}")
            params.append(progress)
            param_idx += 1

        if processed_items is not None:
            updates.append(f"processed_items = ${param_idx}")
            params.append(processed_items)
            param_idx += 1

        if error is not None:
            updates.append(f"error = ${param_idx}")
            params.append(error if error else None)
            param_idx += 1

        if result is not None:
            updates.append(f"result = ${param_idx}")
            params.append(json.dumps(result))
            param_idx += 1

        if status == "processing":
            updates.append(f"started_at = COALESCE(started_at, ${param_idx})")
            params.append(datetime.utcnow())
            param_idx += 1
        elif status in ("completed", "failed"):
            updates.append(f"completed_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1

        params.append(task_id)
        query = f"UPDATE confluence_sync_tasks SET {_build_safe_set_clause(updates)} WHERE task_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def get_pending_confluence_sync_tasks(self, limit: int = 10) -> list[dict[str, Any]]:
        """获取待处理的 Confluence 同步任务"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM confluence_sync_tasks
                WHERE status = 'pending'
                ORDER BY priority DESC, created_at ASC
                LIMIT $1
            """,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]

    async def create_confluence_sync_task(
        self,
        task_id: str,
        binding_id: str | None = None,
        page_id: str | None = None,
        task_type: str = "full_sync",
        priority: int = 0,
        owner_id: str | None = None,
    ) -> None:
        """创建 Confluence 同步任务"""
        if not self._pool:
            return

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO confluence_sync_tasks (
                    task_id, binding_id, page_id, task_type, priority,
                    status, retry_count, max_retries, progress,
                    total_items, processed_items, owner_id
                ) VALUES ($1, $2, $3, $4, $5, 'pending', 0, 3, 0, 0, 0, $6)
            """,
                task_id,
                binding_id,
                page_id,
                task_type,
                priority,
                owner_id,  # ACL: owner_id for access control
            )

    async def update_confluence_sync_task(
        self,
        task_id: str,
        updates: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        """更新 Confluence 同步任务"""
        if not self._pool or not updates:
            return

        set_clauses = []
        params: list[Any] = []
        param_idx = 1

        allowed_fields = {
            "status",
            "retry_count",
            "progress",
            "total_items",
            "processed_items",
            "error",
            "result",
            "started_at",
            "completed_at",
        }

        for key, value in updates.items():
            if key in allowed_fields:
                if key == "result" and isinstance(value, dict):
                    set_clauses.append(f"{key} = ${param_idx}")
                    params.append(json.dumps(value))
                else:
                    set_clauses.append(f"{key} = ${param_idx}")
                    params.append(value)
                param_idx += 1

        if not set_clauses:
            return

        set_clauses.append("updated_at = NOW()")
        params.append(task_id)

        query = f"UPDATE confluence_sync_tasks SET {_build_safe_set_clause(set_clauses)} WHERE task_id = ${param_idx}"

        if connection is not None:
            await connection.execute(query, *params)
            return
        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    # =========================================================================
    # Document Confluence 扩展字段
    # =========================================================================

    async def update_document_confluence_fields(
        self,
        document_id: str,
        confluence_page_id: str | None = None,
        confluence_binding_id: str | None = None,
        confluence_version: int | None = None,
        confluence_web_url: str | None = None,
    ) -> None:
        """更新文档的 Confluence 关联字段"""
        if not self._pool:
            return

        updates = ["updated_at = NOW()"]
        params: list[Any] = []
        param_idx = 1

        if confluence_page_id is not None:
            updates.append(f"confluence_page_id = ${param_idx}")
            params.append(confluence_page_id)
            param_idx += 1

        if confluence_binding_id is not None:
            updates.append(f"confluence_binding_id = ${param_idx}")
            params.append(confluence_binding_id)
            param_idx += 1

        if confluence_version is not None:
            updates.append(f"confluence_version = ${param_idx}")
            params.append(confluence_version)
            param_idx += 1

        if confluence_web_url is not None:
            updates.append(f"confluence_web_url = ${param_idx}")
            params.append(confluence_web_url)
            param_idx += 1

        if len(updates) == 1:  # 只有 updated_at
            return

        params.append(document_id)
        query = f"UPDATE documents SET {_build_safe_set_clause(updates)} WHERE document_id = ${param_idx}"

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

    async def get_documents_by_confluence_binding(
        self,
        binding_id: str,
        limit: int = 100,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """获取 Confluence 绑定关联的文档"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM documents
                WHERE confluence_binding_id = $1
                ORDER BY created_at DESC
                LIMIT $2 OFFSET $3
            """,
                binding_id,
                limit,
                offset,
            )
            return [self._row_to_dict(row) for row in rows]

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
        try:
            lock_minutes = int(minutes)
        except (TypeError, ValueError) as exc:
            raise ValueError("minutes must be an integer") from exc
        if lock_minutes < 1 or lock_minutes > 24 * 60:
            raise ValueError("minutes must be between 1 and 1440")
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE users SET
                    locked_until = NOW() + ($2::int * INTERVAL '1 minute'),
                    updated_at = NOW()
                WHERE user_id = $1
            """,
                user_id,
                lock_minutes,
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

    async def get_user_permissions(self, user_id: str) -> list[str]:
        """获取用户的所有权限（角色权限 + 额外权限）"""
        if not self._pool:
            return []
        cached = await self._get_cached_permissions(user_id)
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
            except Exception:
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
                except Exception:
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
                        except Exception:
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
            except Exception:
                # user_permissions 表可能不存在
                pass

            permissions = list(permissions_set)
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
        query = (
            f"UPDATE users SET {_build_safe_set_clause(set_clauses)} WHERE user_id = ${param_idx}"
        )

        async with self._pool.acquire() as conn:
            await conn.execute(query, *params)

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
            except Exception:
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
            except Exception:
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
            except Exception:
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
        json_dict_fields = {
            "metadata",
            "embedding_config",
            "index_config",
            "result",
            "config",
            "stage_timings",
        }

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
            if field in result and isinstance(result[field], str):
                with contextlib.suppress(json.JSONDecodeError, TypeError):
                    result[field] = json.loads(result[field])

        # Convert datetime fields to ISO format strings
        for field in ("created_at", "updated_at", "last_accessed_at"):
            if field in result and isinstance(result[field], datetime):
                result[field] = result[field].isoformat()

        return result

    # =========================================================================
    # Document Version Control Methods
    # =========================================================================

    async def set_pending_restore_version(
        self,
        document_id: str,
        dataset_id: str,
        previous_version_number: int,
        candidate_version_number: int,
        execution_id: str,
        *,
        connection: Any,
    ) -> bool:
        """Bind a hidden version to the already-queued document generation."""

        if (
            previous_version_number <= 0
            or candidate_version_number <= previous_version_number
            or not str(execution_id or "").strip()
        ):
            raise ValueError("restore candidate identity is invalid")

        row = await connection.fetchrow(
            """
            UPDATE documents
            SET metadata = COALESCE(metadata, '{}'::jsonb)
                    || jsonb_build_object(
                        $3::text, jsonb_build_object(
                            'previous_version', $4::integer,
                            'candidate_version', $5::integer
                        ),
                        $6::text, 'reprocess',
                        $7::text, $8::text
                    ),
                updated_at = NOW()
            WHERE document_id = $1 AND dataset_id = $2 AND status = 'waiting'
              AND NOT (COALESCE(metadata, '{}'::jsonb) ? $3::text)
            RETURNING document_id
            """,
            document_id,
            dataset_id,
            DOCUMENT_PENDING_RESTORE_VERSION_KEY,
            previous_version_number,
            candidate_version_number,
            DOCUMENT_INGEST_ACTION_KEY,
            DOCUMENT_PIPELINE_EXECUTION_KEY,
            execution_id,
        )
        return row is not None

    async def create_document_version(
        self,
        document_id: str,
        content: str,
        content_hash: str,
        change_type: str,
        title: str | None = None,
        metadata: dict[str, Any] | None = None,
        change_reason: str | None = None,
        changed_by: str | None = None,
        confluence_version: int | None = None,
        confluence_updated_at: datetime | None = None,
        *,
        connection: Any | None = None,
        activate: bool = True,
    ) -> dict[str, Any] | None:
        """
        Create a new document version snapshot.

        Args:
            document_id: Document ID
            content: Content snapshot
            content_hash: SHA256 hash of content
            change_type: Type of change (created/updated/restored/deleted)
            title: Title snapshot
            metadata: Metadata snapshot
            change_reason: Reason for the change
            changed_by: User who made the change
            confluence_version: Confluence page version number
            confluence_updated_at: Confluence page update timestamp

        Returns:
            Created version record
        """
        if not self._pool:
            return None

        async def _create(conn: Any) -> dict[str, Any]:
            # Get next version number for this document
            row = await conn.fetchrow(
                "SELECT COALESCE(MAX(version_number), 0) + 1 as next_version FROM document_versions WHERE document_id = $1",
                document_id,
            )
            next_version = row["next_version"] if row else 1

            # Calculate word count
            word_count = len(content.split()) if content else 0

            # Insert version record
            version_id = str(uuid.uuid4())
            await conn.execute(
                """
                INSERT INTO document_versions (
                    version_id, document_id, version_number, content, content_hash,
                    confluence_version, confluence_updated_at, title, metadata, word_count,
                    change_type, change_reason, changed_by, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW())
                """,
                version_id,
                document_id,
                next_version,
                content,
                content_hash,
                confluence_version,
                confluence_updated_at,
                title,
                json.dumps(metadata) if metadata else "{}",
                word_count,
                change_type,
                change_reason,
                changed_by,
            )

            if activate:
                await conn.execute(
                    """
                    UPDATE documents
                    SET current_version = $1, version_count = COALESCE(version_count, 0) + 1
                    WHERE document_id = $2
                    """,
                    next_version,
                    document_id,
                )

            return {
                "version_id": version_id,
                "document_id": document_id,
                "version_number": next_version,
                "change_type": change_type,
                "word_count": word_count,
            }

        if connection is not None:
            return await _create(connection)
        async with self._pool.acquire() as conn, conn.transaction():
            return await _create(conn)

    async def list_document_versions(
        self,
        document_id: str,
        limit: int = 20,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """
        List version history for a document.

        Args:
            document_id: Document ID
            limit: Max results
            offset: Pagination offset

        Returns:
            List of version records (without full content)
        """
        if not self._pool:
            return []

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT
                    version_id, document_id, version_number, content_hash,
                    confluence_version, confluence_updated_at, title, word_count,
                    change_type, change_reason, changed_by, created_at
                FROM document_versions
                WHERE document_id = $1
                  AND change_type NOT IN ('pending_restore', 'pending_before_restore')
                ORDER BY version_number DESC
                LIMIT $2 OFFSET $3
                """,
                document_id,
                limit,
                offset,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_document_version(
        self,
        document_id: str,
        version_number: int,
        *,
        include_pending: bool = False,
    ) -> dict[str, Any] | None:
        """
        Get a specific document version with full content.

        Args:
            document_id: Document ID
            version_number: Version number to retrieve

        Returns:
            Full version record including content
        """
        if not self._pool:
            return None

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM document_versions
                WHERE document_id = $1 AND version_number = $2
                  AND ($3::boolean OR change_type NOT IN ('pending_restore', 'pending_before_restore'))
                """,
                document_id,
                version_number,
                include_pending,
            )
            return self._row_to_dict(row) if row else None

    async def get_document_version_count(self, document_id: str) -> int:
        """Get total version count for a document."""
        if not self._pool:
            return 0

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT COUNT(*) as count FROM document_versions WHERE document_id = $1 AND change_type NOT IN ('pending_restore', 'pending_before_restore')",
                document_id,
            )
            return row["count"] if row else 0

    async def get_latest_document_version(
        self,
        document_id: str,
    ) -> dict[str, Any] | None:
        """Get the latest version of a document."""
        if not self._pool:
            return None

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM document_versions
                WHERE document_id = $1
                  AND change_type NOT IN ('pending_restore', 'pending_before_restore')
                ORDER BY version_number DESC
                LIMIT 1
                """,
                document_id,
            )
            return self._row_to_dict(row) if row else None

    async def delete_old_document_versions(
        self,
        document_id: str,
        keep_count: int = 50,
        keep_first: bool = True,
    ) -> int:
        """
        Delete old versions exceeding the keep count.

        Args:
            document_id: Document ID
            keep_count: Number of recent versions to keep
            keep_first: Whether to always keep the first version

        Returns:
            Number of versions deleted
        """
        if not self._pool:
            return 0

        async with self._pool.acquire() as conn:
            if keep_first:
                # Keep first version and most recent N-1 versions
                result = await conn.execute(
                    """
                    DELETE FROM document_versions
                    WHERE document_id = $1
                    AND version_number NOT IN (
                        SELECT version_number FROM (
                            SELECT version_number FROM document_versions
                            WHERE document_id = $1
                            ORDER BY version_number DESC
                            LIMIT $2
                        ) recent
                        UNION
                        SELECT 1
                    )
                    """,
                    document_id,
                    keep_count - 1,
                )
            else:
                # Keep most recent N versions
                result = await conn.execute(
                    """
                    DELETE FROM document_versions
                    WHERE document_id = $1
                    AND version_number NOT IN (
                        SELECT version_number FROM document_versions
                        WHERE document_id = $1
                        ORDER BY version_number DESC
                        LIMIT $2
                    )
                    """,
                    document_id,
                    keep_count,
                )

            # Update version count
            count_row = await conn.fetchrow(
                "SELECT COUNT(*) as count FROM document_versions WHERE document_id = $1",
                document_id,
            )
            await conn.execute(
                "UPDATE documents SET version_count = $1 WHERE document_id = $2",
                count_row["count"] if count_row else 0,
                document_id,
            )

            return int(result.split()[-1]) if result else 0

    # ============================================
    # Document Summaries (Hierarchical Indexing)
    # ============================================

    async def save_document_summary(
        self, data: dict[str, Any], *, connection: Any | None = None,
    ) -> bool:
        """
        Save or update a document summary for L1 hierarchical indexing.

        Args:
            data: Dictionary containing:
                - document_id: UUID
                - summary: Text summary
                - keywords: List of keywords
                - topics: List of topics
                - vector_id: Qdrant vector ID

        Returns:
            True if saved successfully
        """
        if not self._pool:
            return False

        document_id = data.get("document_id")
        if not document_id:
            return False

        async def _save(conn: Any) -> None:
            await conn.execute(
                """
                INSERT INTO document_summaries (
                    document_id, summary, keywords, topics, vector_id
                ) VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (document_id) DO UPDATE SET
                    summary = EXCLUDED.summary,
                    keywords = EXCLUDED.keywords,
                    topics = EXCLUDED.topics,
                    vector_id = EXCLUDED.vector_id,
                    updated_at = NOW()
                """,
                document_id,
                data.get("summary", ""),
                json.dumps(data.get("keywords", [])),
                json.dumps(data.get("topics", [])),
                data.get("vector_id"),
            )
        if connection is not None:
            await _save(connection)
            return True
        async with self._pool.acquire() as conn:
            await _save(conn)
            return True

    async def get_document_summary(self, document_id: str) -> dict[str, Any] | None:
        """
        Get document summary for L1 retrieval context.

        Args:
            document_id: Document UUID

        Returns:
            Summary dict or None if not found
        """
        if not self._pool:
            return None

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT document_id, summary, keywords, topics, vector_id, created_at, updated_at
                FROM document_summaries
                WHERE document_id = $1
                """,
                document_id,
            )
            return self._row_to_dict(row) if row else None

    async def get_document_summary_scoped(
        self,
        document_id: str,
        dataset_id: str,
        tenant_id: str,
    ) -> dict[str, Any] | None:
        """Read an active document summary under exact tenant/dataset scope."""

        normalized_document = str(document_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_document or not normalized_dataset or not normalized_tenant:
            raise ValueError("document_id, dataset_id, and tenant_id are required")
        if not self._pool:
            raise RuntimeError("database is not connected")

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT dsumm.document_id, dsumm.summary, dsumm.keywords,
                       dsumm.topics, dsumm.vector_id, dsumm.created_at,
                       dsumm.updated_at
                FROM document_summaries AS dsumm
                JOIN documents AS d ON d.document_id = dsumm.document_id
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE dsumm.document_id = $1
                  AND d.dataset_id = $2
                  AND ds.tenant_id = $3
                  AND ds.is_deleted = FALSE
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                """,
                normalized_document,
                normalized_dataset,
                normalized_tenant,
            )
            return self._row_to_dict(row) if row else None

    async def delete_document_summary(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """
        Delete document summary.

        Args:
            document_id: Document UUID

        Returns:
            True if deleted
        """
        if not self._pool:
            return False

        async def _delete(conn: Any) -> bool:
            result = await conn.execute(
                "DELETE FROM document_summaries WHERE document_id = $1",
                document_id,
            )
            return "DELETE" in result

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)

    async def get_dataset_summaries(
        self,
        dataset_id: str,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """
        Get all document summaries in a dataset.

        Args:
            dataset_id: Dataset UUID
            limit: Max summaries to return

        Returns:
            List of summary dicts
        """
        if not self._pool:
            return []

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT ds.document_id, ds.summary, ds.keywords, ds.topics,
                       ds.vector_id, ds.created_at, d.title as document_title
                FROM document_summaries ds
                JOIN documents d ON ds.document_id = d.document_id
                WHERE d.dataset_id = $1
                ORDER BY ds.created_at DESC
                LIMIT $2
                """,
                dataset_id,
                limit,
            )
            return [self._row_to_dict(row) for row in rows]
