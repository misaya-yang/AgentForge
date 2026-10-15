"""Dataset persistence helpers and the dataset-facing storage mixin."""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import logging
import uuid
from typing import Any

try:
    import asyncpg

    HAS_ASYNCPG = True
except ImportError:
    HAS_ASYNCPG = False
    asyncpg = None

# Preserve the logger category used before this code was extracted.
logger = logging.getLogger("knowledge_service.persistence.database")

INDEX_DELETION_FENCE_KEY = "_index_deletion_fence"
INDEX_DELETION_FENCE_VERSION = 1
DOCUMENT_LIFECYCLE_REINDEX_KEY = "_document_lifecycle_reindex"
DOCUMENT_UPLOAD_GENERATION_KEY = "_document_upload_generation"
DOCUMENT_UPLOAD_FAILED_KEY = "_document_upload_failed"
CONFLUENCE_SYNC_GENERATION_KEY = "_confluence_sync_generation"

# PRD T1 items 3/4 + addendum §1-T1: dual-verb reprocessing contract.
# The durable queue is the set of documents.status='waiting' rows; the verb a
# queued generation must run is pinned on the row at claim time so it cannot
# drift between enqueue and dispatch. 'ingest' is the default first-generation
# pipeline and carries no marker. The marker vocabulary mirrors the action
# column of document_pipeline_executions (migration 101).
DOCUMENT_INGEST_ACTION_KEY = "_document_ingest_action"
DOCUMENT_RECOVER_STAGE_KEY = "_document_recover_stage"
DOCUMENT_PIPELINE_EXECUTION_KEY = "_document_pipeline_execution_id"
DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY = "_special_publication_generation_id"
SPECIAL_PUBLICATION_MANIFEST_KEY = "special_publication"
SPECIAL_PUBLICATION_PHASES = (
    "preparing",
    "prepared",
    "points_written",
    "committed",
    "aborted",
)
# The candidate text for a restore lives in document_versions until its
# segment generation publishes; the document marker stores only its number.
DOCUMENT_PENDING_RESTORE_VERSION_KEY = "_document_pending_restore_version"
DOCUMENT_RESTORED_SOURCE_VERSION_KEY = "_document_restored_source_version"
INGEST_ACTION_VOCABULARY = frozenset(
    {"ingest", "reprocess", "reembed", "recover", "retry"}
)
# Interactive single-document verbs jump the durable queue ahead of bulk
# import backlogs (PRD §3.1 dual queue, adapt-3: routing by operation type).
PRIORITY_INGEST_ACTIONS = frozenset({"reprocess", "reembed", "recover", "retry"})
# Interactive work may overtake bulk work queued at most this much earlier.
# The finite bias preserves low-latency repairs without letting a continuous
# stream of repairs starve an older bulk generation forever.
INTERACTIVE_QUEUE_BIAS_SECONDS = 5 * 60
# Stuck stages the crash-recovery loop can observe; they decide the recover
# branch (PRD T1 item 4: splitting redoes the full pipeline, indexing rebuilds
# vectors only from already-persisted chunks).
RECOVER_STAGE_VOCABULARY = frozenset({"parsing", "splitting", "indexing"})

_INDEX_DELETION_OPERATIONS = frozenset({"dataset_delete", "document_delete", "segment_delete"})


class IndexLeaseUnavailableError(RuntimeError):
    """A short-lived index lease is busy; callers must defer, not fail work."""


def _json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            decoded = json.loads(value)
        except (json.JSONDecodeError, TypeError):
            return {}
        return decoded if isinstance(decoded, dict) else {}
    return {}


def _special_publication_plan(
    collections: dict[str, dict[str, list[str]]] | None,
    objects: dict[str, str] | None,
) -> tuple[dict[str, dict[str, list[str]]], dict[str, str]]:
    """Canonicalize the immutable cross-store deletion/publication plan."""

    normalized_collections: dict[str, dict[str, list[str]]] = {}
    for raw_name, raw_points in (collections or {}).items():
        name = str(raw_name or "").strip()
        if not name or name in normalized_collections or not isinstance(raw_points, dict):
            raise ValueError("special publication collection plan is invalid")
        if set(raw_points) != {"candidate_point_ids", "old_point_ids"}:
            raise ValueError("special publication point sets are incomplete")
        normalized_points: dict[str, list[str]] = {}
        for key in ("candidate_point_ids", "old_point_ids"):
            values = raw_points[key]
            if not isinstance(values, list) or any(
                not isinstance(value, str) or not value.strip() for value in values
            ):
                raise ValueError("special publication point ids are invalid")
            normalized_points[key] = sorted(set(values))
        if set(normalized_points["candidate_point_ids"]) & set(
            normalized_points["old_point_ids"]
        ):
            raise ValueError("candidate and old point ids must be disjoint")
        normalized_collections[name] = normalized_points
    normalized_objects: dict[str, str] = {}
    for raw_key, raw_hash in (objects or {}).items():
        key = str(raw_key or "").strip()
        content_hash = str(raw_hash or "").strip().lower()
        if (
            not key or key in normalized_objects or len(content_hash) != 64
            or any(character not in "0123456789abcdef" for character in content_hash)
        ):
            raise ValueError("special publication object receipt is incomplete")
        normalized_objects[key] = content_hash
    return dict(sorted(normalized_collections.items())), dict(sorted(normalized_objects.items()))


def _special_source_hash(value: str) -> str:
    normalized = str(value or "").strip().lower()
    if len(normalized) != 64 or any(character not in "0123456789abcdef" for character in normalized):
        raise ValueError("special publication source_hash must be a SHA-256 hex digest")
    return normalized


def _special_object_keys(value: list[str] | None) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list) or any(
        not isinstance(item, str) or not item.strip() for item in value
    ):
        raise ValueError("special publication planned object keys are invalid")
    normalized = [item.strip() for item in value]
    if len(normalized) != len(set(normalized)):
        raise ValueError("special publication planned object keys are duplicated")
    return sorted(normalized)


def make_dataset_index_deletion_fence(
    operation: str,
    target_id: str,
) -> dict[str, Any]:
    """Build one deterministic, target-bound deletion fence marker."""

    normalized_operation = str(operation or "").strip()
    normalized_target = str(target_id or "").strip()
    if normalized_operation not in _INDEX_DELETION_OPERATIONS:
        raise ValueError("unsupported dataset index deletion operation")
    if not normalized_target:
        raise ValueError("dataset index deletion target_id is required")
    return {
        "operation": normalized_operation,
        "target_id": normalized_target,
        "status": "pending",
        "version": INDEX_DELETION_FENCE_VERSION,
    }


def dataset_index_deletion_fence(dataset: dict[str, Any]) -> dict[str, Any] | None:
    """Return the validated durable deletion fence, failing closed if malformed."""

    index_config = _json_object(dataset.get("index_config"))
    retrieval = _json_object(index_config.get("retrieval"))
    if INDEX_DELETION_FENCE_KEY not in retrieval:
        return None
    marker = retrieval.get(INDEX_DELETION_FENCE_KEY)
    if not isinstance(marker, dict):
        raise RuntimeError("dataset index deletion fence is malformed")
    operation = marker.get("operation")
    target_id = marker.get("target_id")
    if (
        operation not in _INDEX_DELETION_OPERATIONS
        or not isinstance(target_id, str)
        or not target_id.strip()
        or marker.get("status") != "pending"
        or marker.get("version") != INDEX_DELETION_FENCE_VERSION
    ):
        raise RuntimeError("dataset index deletion fence is malformed")
    return make_dataset_index_deletion_fence(operation, target_id)


def index_config_has_reserved_deletion_fence(index_config: Any) -> bool:
    """Return whether caller-controlled config contains the internal marker key."""

    retrieval = _json_object(_json_object(index_config).get("retrieval"))
    return INDEX_DELETION_FENCE_KEY in retrieval


NON_INGESTION_INDEX_CONFIG_KEYS = frozenset(
    {"retrieval", "document_metadata_registry"}
)


def dataset_ingestion_identity(dataset: dict[str, Any]) -> str:
    """Hash the dataset choices that determine persisted index generations.

    Retrieval-only tuning is intentionally excluded: it can be changed without
    rebuilding chunks or dense vectors. The hash keeps credential-bearing
    embedding configuration out of logs and exception messages.
    """

    index_config = _json_object(dataset.get("index_config"))
    payload = {
        "tenant_id": str(dataset.get("tenant_id") or ""),
        "collection_name": str(dataset.get("collection_name") or ""),
        "embedding_provider": str(dataset.get("embedding_provider") or ""),
        "embedding_model": str(dataset.get("embedding_model") or ""),
        "embedding_dimension": int(dataset.get("embedding_dimension") or 0),
        "embedding_config": _json_object(dataset.get("embedding_config")),
        "ingestion_index_config": {
            key: value
            for key, value in index_config.items()
            if key not in NON_INGESTION_INDEX_CONFIG_KEYS
        },
    }
    canonical = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


class DatasetPersistenceMixin:
    """Dataset CRUD, permissions, ingestion claims, and index leases."""

    async def dataset_exists(self, dataset_id: str) -> bool:
        """Return whether an ID is reserved by any active or soft-deleted dataset."""
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            return bool(
                await conn.fetchval(
                    "SELECT EXISTS(SELECT 1 FROM datasets WHERE dataset_id = $1)",
                    dataset_id,
                )
            )

    async def collection_name_in_use(self, collection_name: str) -> bool:
        """Return whether a Qdrant collection name is already bound to a dataset."""
        if not self._pool:
            raise RuntimeError("database is not connected")
        if not collection_name:
            return False
        async with self._pool.acquire() as conn:
            return bool(
                await conn.fetchval(
                    "SELECT EXISTS(SELECT 1 FROM datasets WHERE collection_name = $1)",
                    collection_name,
                )
            )

    async def _insert_dataset_row(self, conn: Any, dataset: dict[str, Any]) -> Any:
        return await conn.fetchrow(
            """
            INSERT INTO datasets (
                dataset_id, name, description, tenant_id, visibility,
                embedding_provider, embedding_model, embedding_dimension,
                embedding_config, index_config, collection_name,
                is_deleted, deleted_at, deleted_by, delete_reason,
                created_by
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8,
                $9, $10, $11,
                $12, $13, $14, $15,
                $16
            )
            RETURNING dataset_id
            """,
            dataset.get("dataset_id"),
            dataset.get("name"),
            dataset.get("description"),
            dataset.get("tenant_id", ""),
            dataset.get("visibility", "private"),
            dataset.get("embedding_provider", "gemini"),
            dataset.get("embedding_model", "gemini-embedding-001"),
            int(dataset.get("embedding_dimension") or 0) or 1024,
            json.dumps(dataset.get("embedding_config", {})),
            json.dumps(dataset.get("index_config", {})),
            dataset.get("collection_name"),
            bool(dataset.get("is_deleted", False)),
            dataset.get("deleted_at"),
            dataset.get("deleted_by"),
            dataset.get("delete_reason"),
            dataset.get("created_by"),
        )

    async def create_dataset(self, dataset: dict[str, Any]) -> bool:
        """Insert a new dataset without ever overwriting an existing identity."""
        if not self._pool:
            raise RuntimeError("database is not connected")

        try:
            async with self._pool.acquire() as conn:
                row = await self._insert_dataset_row(conn, dataset)
                return row is not None
        except Exception as exc:
            if HAS_ASYNCPG and isinstance(exc, asyncpg.UniqueViolationError):
                return False
            raise

    async def create_dataset_with_owner(
        self,
        dataset: dict[str, Any],
        owner_user_id: str,
    ) -> bool:
        """Atomically insert a dataset and its explicit owner permission."""
        if not self._pool:
            raise RuntimeError("database is not connected")
        if not owner_user_id:
            raise ValueError("owner_user_id is required")

        try:
            async with self._pool.acquire() as conn, conn.transaction():
                row = await self._insert_dataset_row(conn, dataset)
                if row is None:
                    raise RuntimeError("dataset insert returned no identity")
                await conn.execute(
                    """
                    INSERT INTO dataset_permissions (
                        dataset_id, subject_type, subject_id, permission
                    ) VALUES ($1, 'user', $2, 'owner')
                    """,
                    dataset.get("dataset_id"),
                    owner_user_id,
                )
                return True
        except Exception as exc:
            if HAS_ASYNCPG and isinstance(exc, asyncpg.UniqueViolationError):
                return False
            raise

    async def save_dataset(self, dataset: dict[str, Any]) -> None:
        """保存或更新知识库 Dataset"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO datasets (
                    dataset_id, name, description, tenant_id, visibility,
                    embedding_provider, embedding_model, embedding_dimension,
                    embedding_config, index_config, collection_name,
                    is_deleted, deleted_at, deleted_by, delete_reason,
                    created_by
                ) VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8,
                    $9, $10, $11,
                    $12, $13, $14, $15,
                    $16
                )
                ON CONFLICT (dataset_id) DO UPDATE SET
                    name = EXCLUDED.name,
                    description = EXCLUDED.description,
                    tenant_id = EXCLUDED.tenant_id,
                    visibility = EXCLUDED.visibility,
                    embedding_provider = EXCLUDED.embedding_provider,
                    embedding_model = EXCLUDED.embedding_model,
                    embedding_dimension = EXCLUDED.embedding_dimension,
                    embedding_config = EXCLUDED.embedding_config,
                    index_config = EXCLUDED.index_config,
                    collection_name = EXCLUDED.collection_name,
                    is_deleted = FALSE,
                    deleted_at = NULL,
                    deleted_by = NULL,
                    delete_reason = NULL,
                    created_by = EXCLUDED.created_by,
                    updated_at = NOW()
                """,
                dataset.get("dataset_id"),
                dataset.get("name"),
                dataset.get("description"),
                dataset.get("tenant_id", ""),
                dataset.get("visibility", "private"),
                dataset.get("embedding_provider", "gemini"),
                dataset.get("embedding_model", "gemini-embedding-001"),
                int(dataset.get("embedding_dimension") or 0) or 1024,
                json.dumps(dataset.get("embedding_config", {})),
                json.dumps(dataset.get("index_config", {})),
                dataset.get("collection_name"),
                bool(dataset.get("is_deleted", False)),
                dataset.get("deleted_at"),
                dataset.get("deleted_by"),
                dataset.get("delete_reason"),
                dataset.get("created_by"),
            )

    async def patch_dataset_fields(
        self,
        dataset_id: str,
        changes: dict[str, Any],
        *,
        expected_config: dict[str, Any] | None = None,
        require_no_documents: bool = False,
    ) -> dict[str, Any] | None:
        """Patch only requested fields, with CAS for retrieval configuration.

        Dataset callers commonly operate on snapshots. Updating the full row
        from such a snapshot can overwrite a concurrent lexical transition,
        so this method deliberately rejects unknown columns and never touches
        fields omitted by the caller. Any embedding/index/collection change
        must match the caller's complete prior configuration projection.
        """
        if not self._pool:
            raise RuntimeError("database is not connected")
        allowed = (
            "name",
            "description",
            "visibility",
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
            "index_config",
            "collection_name",
        )
        config_fields = {
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
            "index_config",
            "collection_name",
        }
        unknown = sorted(set(changes) - set(allowed))
        if unknown:
            raise ValueError("unsupported dataset update fields: " + ", ".join(unknown))
        selected = [field for field in allowed if field in changes]
        if not selected:
            return await self.get_dataset(dataset_id)

        expected: dict[str, Any] | None = None
        if config_fields.intersection(selected):
            expected = dict(expected_config or {})
            missing_expected = sorted(config_fields - set(expected))
            if missing_expected:
                raise ValueError(
                    "dataset configuration CAS is missing fields: " + ", ".join(missing_expected)
                )

        values: list[Any] = [dataset_id]
        assignments: list[str] = []
        for field in selected:
            value = changes[field]
            if field in {"embedding_config", "index_config"}:
                value = json.dumps(value or {})
                cast = "::jsonb"
            else:
                cast = ""
            values.append(value)
            assignments.append(f"{field} = ${len(values)}{cast}")

        predicates = ["dataset_id = $1", "is_deleted = FALSE", "is_archived = FALSE"]
        if expected is not None:
            for field in (
                "embedding_provider",
                "embedding_model",
                "embedding_dimension",
                "embedding_config",
                "index_config",
                "collection_name",
            ):
                value = expected[field]
                if field in {"embedding_config", "index_config"}:
                    value = json.dumps(value or {})
                    cast = "::jsonb"
                else:
                    cast = ""
                values.append(value)
                predicates.append(f"{field} IS NOT DISTINCT FROM ${len(values)}{cast}")
        if require_no_documents:
            predicates.append(
                "NOT EXISTS (SELECT 1 FROM documents "
                "WHERE documents.dataset_id = datasets.dataset_id)"
            )

        async def _patch(conn: Any) -> Any:
            return await conn.fetchrow(
                "UPDATE datasets SET "
                + ", ".join(assignments)
                + ", updated_at = NOW() WHERE "
                + " AND ".join(predicates)
                + " RETURNING *",
                *values,
            )

        async with self._pool.acquire() as conn:
            if require_no_documents:
                async with conn.transaction():
                    await conn.fetchval(
                        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                        self._dataset_index_lock_name(dataset_id),
                    )
                    row = await _patch(conn)
            else:
                row = await _patch(conn)
        return self._row_to_dict(row) if row else None

    async def compare_and_swap_dataset_collection_identity(
        self,
        dataset_id: str,
        *,
        expected_dimension: int,
        expected_collection_name: str,
        replacement_dimension: int,
        replacement_collection_name: str,
    ) -> bool:
        """Initialize dimension/collection without overwriting a newer value."""
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn, conn.transaction():
            await conn.fetchval(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                self._dataset_index_lock_name(dataset_id),
            )
            row = await conn.fetchrow(
                """
                UPDATE datasets
                SET embedding_dimension = $4,
                    collection_name = $5,
                    updated_at = NOW()
                WHERE dataset_id = $1
                  AND is_deleted = FALSE
                  AND COALESCE(embedding_dimension, 0) = $2
                  AND COALESCE(collection_name, '') = $3
                RETURNING dataset_id
                """,
                dataset_id,
                int(expected_dimension or 0),
                str(expected_collection_name or ""),
                int(replacement_dimension),
                replacement_collection_name,
            )
        return row is not None

    @staticmethod
    def _dataset_index_lock_name(dataset_id: str) -> str:
        normalized = str(dataset_id or "").strip()
        if not normalized:
            raise ValueError("dataset_id is required for index locking")
        return f"knowledge-dataset-index:{normalized}"

    def connection_pool_max_size(self) -> int:
        """Expose configured capacity for long-lived worker lease admission."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        getter = getattr(self._pool, "get_max_size", None)
        if not callable(getter):
            raise RuntimeError("database pool does not expose its maximum size")
        return int(getter())

    @staticmethod
    def _segment_index_lock_name(dataset_id: str, segment_id: str) -> str:
        normalized_dataset = str(dataset_id or "").strip()
        normalized_segment = str(segment_id or "").strip()
        if not normalized_dataset or not normalized_segment:
            raise ValueError("dataset_id and segment_id are required for segment locking")
        return f"knowledge-segment-index:{normalized_dataset}:{normalized_segment}"

    @staticmethod
    def _document_index_lock_name(dataset_id: str, document_id: str) -> str:
        normalized_dataset = str(dataset_id or "").strip()
        normalized_document = str(document_id or "").strip()
        if not normalized_dataset or not normalized_document:
            raise ValueError("dataset_id and document_id are required for document locking")
        return f"knowledge-document-index:{normalized_dataset}:{normalized_document}"

    @contextlib.asynccontextmanager
    async def document_index_update_lease(
        self,
        dataset_id: str,
        document_id: str,
    ):
        """Short dataset-shared/document-exclusive cross-replica barrier."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        dataset_lock_name = self._dataset_index_lock_name(dataset_id)
        document_lock_name = self._document_index_lock_name(dataset_id, document_id)
        async with self._pool.acquire() as conn:
            dataset_acquired = await conn.fetchval(
                "SELECT pg_try_advisory_lock_shared(hashtextextended($1, 0))",
                dataset_lock_name,
            )
            if dataset_acquired is not True:
                raise IndexLeaseUnavailableError(
                    "dataset index deletion is in progress; refusing document index update"
                )
            document_acquired = False
            try:
                document_acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    document_lock_name,
                )
                if document_acquired is not True:
                    raise IndexLeaseUnavailableError("document index update is already in progress")
                yield conn
            finally:
                try:
                    if document_acquired:
                        document_unlock = asyncio.create_task(
                            conn.fetchval(
                                "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                                document_lock_name,
                            )
                        )
                        try:
                            document_released = await asyncio.shield(document_unlock)
                        except asyncio.CancelledError:
                            document_released = await document_unlock
                            raise
                        if document_released is not True:
                            raise RuntimeError("document index update lease was not released")
                finally:
                    dataset_unlock = asyncio.create_task(
                        conn.fetchval(
                            "SELECT pg_advisory_unlock_shared(hashtextextended($1, 0))",
                            dataset_lock_name,
                        )
                    )
                    try:
                        dataset_released = await asyncio.shield(dataset_unlock)
                    except asyncio.CancelledError:
                        dataset_released = await dataset_unlock
                        raise
                    if dataset_released is not True:
                        raise RuntimeError("dataset shared index lease was not released")

    @contextlib.asynccontextmanager
    async def segment_index_update_lease(
        self,
        dataset_id: str,
        document_id: str,
        segment_id: str,
    ):
        """Lock dataset, document generation, then one segment generation."""

        segment_lock_name = self._segment_index_lock_name(dataset_id, segment_id)
        async with self.document_index_update_lease(dataset_id, document_id) as conn:
            segment_acquired = False
            try:
                segment_acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    segment_lock_name,
                )
                if segment_acquired is not True:
                    raise IndexLeaseUnavailableError("segment index update is already in progress")
                yield conn
            finally:
                if segment_acquired:
                    segment_unlock = asyncio.create_task(
                        conn.fetchval(
                            "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                            segment_lock_name,
                        )
                    )
                    try:
                        segment_released = await asyncio.shield(segment_unlock)
                    except asyncio.CancelledError:
                        segment_released = await segment_unlock
                        raise
                    if segment_released is not True:
                        raise RuntimeError("segment index update lease was not released")

    @contextlib.asynccontextmanager
    async def document_segment_create_lease(
        self,
        dataset_id: str,
        document_id: str,
    ):
        """Fence deletion and serialize position allocation for one document."""

        async with self.document_index_update_lease(dataset_id, document_id) as conn:
            yield conn

    async def claim_document_for_enqueue(
        self,
        dataset_id: str,
        document_id: str,
        *,
        action: str | None = None,
        recover_stage: str | None = None,
        execution_id: str | None = None,
        pin_execution_rule: bool = False,
        connection: Any | None = None,
    ) -> bool:
        """Durably move one eligible document into the queued generation.

        ``action`` pins the dual-verb contract (PRD T1 item 3) on the queued
        row atomically with the waiting-transition; ``None`` means the default
        first-generation pipeline and clears any stale verb left by an earlier
        generation. ``recover_stage`` records the stage a crashed generation
        died in (recover verb only); ``execution_id`` links the queued row to
        its document_pipeline_executions replay snapshot (addendum §1-T1.3).
        """

        normalized_action = str(action or "").strip().lower() or None
        if normalized_action is not None and normalized_action not in INGEST_ACTION_VOCABULARY:
            raise ValueError(f"unsupported ingest action: {action}")
        normalized_stage = str(recover_stage or "").strip().lower() or None
        if normalized_stage is not None and normalized_stage not in RECOVER_STAGE_VOCABULARY:
            raise ValueError(f"unsupported recover stage: {recover_stage}")
        if normalized_stage and normalized_action != "recover":
            raise ValueError("recover_stage is only valid with the recover action")
        normalized_execution_id = str(execution_id or "").strip() or None
        if pin_execution_rule and (
            normalized_action not in {"reprocess", "recover", "retry"}
            or not normalized_execution_id
        ):
            raise ValueError("replay rule pin requires a replay execution")

        # Build the exact metadata patch in Python; the UPDATE merges it over
        # the authoritative row after stripping stage/exec keys so a verb can
        # never inherit stale replay state from a previous generation.
        if normalized_action is None or normalized_action == "ingest":
            metadata_patch: dict[str, Any] | None = None
        else:
            metadata_patch = {DOCUMENT_INGEST_ACTION_KEY: normalized_action}
            if normalized_stage:
                metadata_patch[DOCUMENT_RECOVER_STAGE_KEY] = normalized_stage
            if normalized_execution_id:
                metadata_patch[DOCUMENT_PIPELINE_EXECUTION_KEY] = normalized_execution_id

        @contextlib.asynccontextmanager
        async def claim_connection():
            if connection is not None:
                yield connection
            else:
                async with self.document_index_update_lease(dataset_id, document_id) as leased:
                    yield leased

        async with claim_connection() as conn:
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                None,
            )
            row = await conn.fetchrow(
                f"""
                UPDATE documents
                SET status = 'waiting',
                    progress = 0,
                    error = NULL,
                    process_rule_id = CASE
                        WHEN $5::boolean THEN (
                            SELECT execution.process_rule_id
                            FROM document_pipeline_executions AS execution
                            WHERE execution.execution_id = $6
                        )
                        ELSE process_rule_id
                    END,
                    -- This claim opens a new pipeline generation. Stage
                    -- timestamps describe that generation only; retaining a
                    -- prior run makes the UI report ever-growing parsing /
                    -- splitting durations. Cancellation/recovery requeues use
                    -- requeue_cancelled_document_generation instead and keep
                    -- their current-generation stamps.
                    started_at = NULL,
                    completed_at = NULL,
                    parsing_started_at = NULL,
                    splitting_started_at = NULL,
                    indexing_started_at = NULL,
                    updated_at = NOW(),
                    metadata = CASE
                        WHEN $3::jsonb IS NULL THEN
                            COALESCE(metadata, '{{}}'::jsonb)
                                - '{DOCUMENT_INGEST_ACTION_KEY}'
                                - '{DOCUMENT_RECOVER_STAGE_KEY}'
                                - '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                        ELSE
                            (COALESCE(metadata, '{{}}'::jsonb)
                                - '{DOCUMENT_RECOVER_STAGE_KEY}'
                                - '{DOCUMENT_PIPELINE_EXECUTION_KEY}')
                            || $3::jsonb
                    END
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status IN ('waiting', 'completed', 'error')
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
                        OR metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                            ->> 'status' = 'pending'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'desired_archived' = 'false'
                  )
                  -- A row already queued under a verb belongs to that verb:
                  -- only an identical re-claim by the same execution may
                  -- refresh it. A different
                  -- verb (or a plain claim that would strip the marker) is
                  -- rejected so a queued generation can never be silently
                  -- swapped or demoted. Terminal rows are exempt: they never
                  -- legitimately carry a marker, and re-claiming is the
                  -- self-healing path for stale ones.
                  AND (
                        status <> 'waiting'
                        OR NOT (
                            COALESCE(metadata, '{{}}'::jsonb)
                            ? '{DOCUMENT_INGEST_ACTION_KEY}'
                        )
                        OR COALESCE(metadata ->> '{DOCUMENT_INGEST_ACTION_KEY}', '')
                            = $4
                  )
                  -- A recovery requeue retains the running ledger link even
                  -- while status is waiting. Another request may only reclaim
                  -- the exact same execution, never replace that owner.
                  AND (
                        status <> 'waiting'
                        OR COALESCE(metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}', '') = ''
                        OR metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}' = $6
                  )
                  AND (
                        NOT $5::boolean
                        OR EXISTS (
                            SELECT 1 FROM document_pipeline_executions AS execution
                            WHERE execution.execution_id = $6
                              AND execution.document_id = $1
                              AND execution.dataset_id = $2
                              AND execution.action = $4
                              AND execution.status = 'running'
                              AND execution.process_rule_id IS NOT NULL
                              AND execution.input_snapshot <> '{{}}'::jsonb
                        )
                  )
                RETURNING document_id
                """,
                document_id,
                dataset_id,
                json.dumps(metadata_patch) if metadata_patch is not None else None,
                normalized_action or "",
                pin_execution_rule,
                normalized_execution_id,
            )
            return row is not None

    async def pin_document_ingest_action(
        self,
        dataset_id: str,
        document_id: str,
        action: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """Pin the dual-verb marker on a lifecycle-owned restore generation.

        Restore transitions persist ``status='waiting'`` directly under the
        dataset-exclusive lifecycle lease, whose exclusive advisory lock makes
        the shared lock taken by claim_document_for_enqueue unacquirable, so
        the verb marker needs this owner-scoped writer. Like the claim path,
        the write strips stale replay state first so the verb never inherits
        a previous generation's stage/exec keys. Generic callers cannot reach
        the verb keys through update_document_fields.
        """

        normalized_action = str(action or "").strip().lower()
        if normalized_action not in INGEST_ACTION_VOCABULARY:
            raise ValueError(f"unsupported ingest action: {action}")
        query = f"""
            UPDATE documents
            SET metadata = (
                    (COALESCE(metadata, '{{}}'::jsonb)
                        - '{DOCUMENT_RECOVER_STAGE_KEY}'
                        - '{DOCUMENT_PIPELINE_EXECUTION_KEY}')
                    || jsonb_build_object('{DOCUMENT_INGEST_ACTION_KEY}', $3::text)
                ),
                updated_at = NOW()
            WHERE document_id = $1 AND dataset_id = $2
            RETURNING document_id
        """
        if connection is not None:
            row = await connection.fetchrow(
                query, document_id, dataset_id, normalized_action
            )
            return row is not None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                query, document_id, dataset_id, normalized_action
            )
            return row is not None

    async def list_queued_documents(
        self,
        *,
        limit: int = 100,
        tenant_cursor: str | None = None,
    ) -> list[dict[str, Any]]:
        """Return bounded durable work for worker-role dispatch.

        This is discovery only. The consumer still performs the authoritative
        queued-to-processing CAS while holding the document owner lease, so
        concurrent worker replicas may observe the same row but cannot process
        the same generation twice. Rows are round-robin by tenant; within each
        tenant, interactive verbs receive a finite age bias over bulk work.
        ``tenant_cursor`` rotates the first tenant across polling batches.
        """

        if not self._pool:
            return []
        bounded_limit = min(max(int(limit), 1), 1000)
        normalized_cursor = str(tenant_cursor or "").strip()
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                WITH eligible AS (
                    SELECT
                        ds.tenant_id,
                        d.dataset_id,
                        d.document_id,
                        d.updated_at,
                        CASE
                            WHEN COALESCE(d.metadata, '{{}}'::jsonb)
                                ->> '{DOCUMENT_INGEST_ACTION_KEY}'
                                = ANY($4::text[])
                            THEN 'interactive'
                            ELSE 'bulk'
                        END AS dispatch_lane,
                        d.updated_at - (
                            CASE
                                WHEN COALESCE(d.metadata, '{{}}'::jsonb)
                                    ->> '{DOCUMENT_INGEST_ACTION_KEY}'
                                    = ANY($4::text[])
                                THEN $3::double precision
                                ELSE 0
                            END * INTERVAL '1 second'
                        ) AS dispatch_order_at
                    FROM documents AS d
                    JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                    WHERE d.status = 'waiting'
                      AND ds.is_deleted = FALSE
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
                      AND NOT COALESCE(
                            COALESCE(ds.index_config, '{{}}'::jsonb)
                                -> 'retrieval' ? '{INDEX_DELETION_FENCE_KEY}',
                            FALSE
                      )
                ), tenant_ranked AS (
                    SELECT
                        eligible.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY tenant_id
                            ORDER BY dispatch_order_at, updated_at, document_id
                        ) AS tenant_round
                    FROM eligible
                )
                SELECT
                    tenant_id,
                    dataset_id,
                    document_id,
                    dispatch_lane
                FROM tenant_ranked
                ORDER BY
                    tenant_round,
                    CASE
                        WHEN $2::text = '' OR tenant_id > $2::text THEN 0
                        ELSE 1
                    END,
                    tenant_id,
                    dispatch_order_at,
                    updated_at,
                    document_id
                LIMIT $1
                """,
                bounded_limit,
                normalized_cursor,
                float(INTERACTIVE_QUEUE_BIAS_SECONDS),
                sorted(PRIORITY_INGEST_ACTIONS),
            )
        return [self._row_to_dict(row) for row in rows]

    async def count_queued_documents(self) -> int:
        """Count the same dispatchable rows ``list_queued_documents`` serves.

        Feeds the ``kb_ingestion_queue_depth`` gauge at ``/metrics`` scrape
        time. The WHERE clause must stay in lockstep with
        ``list_queued_documents``: a depth that disagrees with what workers
        can actually claim misleads capacity alerts.
        """

        if not self._pool:
            return 0
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"""
                SELECT count(*) AS depth
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.status = 'waiting'
                  AND ds.is_deleted = FALSE
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
                  AND NOT COALESCE(
                        COALESCE(ds.index_config, '{{}}'::jsonb)
                            -> 'retrieval' ? '{INDEX_DELETION_FENCE_KEY}',
                        FALSE
                  )
                """
            )
        if row is None:
            return 0
        return int(row["depth"] or 0)

    async def claim_queued_document_for_processing(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """CAS one durable queued row to processing before consuming it."""

        async def _claim(conn: Any) -> bool:
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                None,
            )
            row = await conn.fetchrow(
                f"""
                UPDATE documents
                SET status = 'parsing',
                    progress = 0,
                    error = NULL,
                    updated_at = NOW(),
                    started_at = COALESCE(started_at, NOW()),
                    parsing_started_at = COALESCE(parsing_started_at, NOW())
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status = 'waiting'
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
                document_id,
                dataset_id,
            )
            return row is not None

        if connection is not None:
            return await _claim(connection)
        async with self.document_index_update_lease(dataset_id, document_id) as conn:
            return await _claim(conn)

    async def requeue_cancelled_document_generation(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any,
    ) -> bool:
        """Return one owned processing generation to the durable queue."""

        row = await connection.fetchrow(
            """
            UPDATE documents
            SET status = 'waiting',
                progress = 0,
                error = NULL,
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status IN (
                    'parsing',
                    'splitting',
                    'indexing'
              )
            RETURNING document_id
            """,
            document_id,
            dataset_id,
        )
        return row is not None

    # ------------------------------------------------------------------
    # Document progress events (migration 111 / H1 #4).  The event ledger is
    # append-only and read-only from this API; the database trigger records all
    # progress writes so API and worker processes share one replay cursor.
    # ------------------------------------------------------------------

    async def list_document_progress_events(
        self,
        dataset_id: str,
        *,
        after_sequence: int = 0,
        document_ids: list[str] | None = None,
        limit: int = 200,
    ) -> list[dict[str, Any]]:
        """Return dataset-scoped progress events after a durable cursor."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_dataset:
            raise ValueError("dataset_id is required")
        try:
            cursor = max(int(after_sequence), 0)
        except (TypeError, ValueError) as exc:
            raise ValueError("after_sequence must be a non-negative integer") from exc
        bounded_limit = min(max(int(limit), 1), 500)
        normalized_documents = sorted(
            {
                str(document_id or "").strip()
                for document_id in (document_ids or [])
                if str(document_id or "").strip()
            }
        ) or None

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT event_sequence, dataset_id, document_id, event_type,
                       payload, created_at
                FROM kb_document_progress_events
                WHERE dataset_id = $1
                  AND event_sequence > $2
                  AND ($3::varchar[] IS NULL OR document_id = ANY($3::varchar[]))
                ORDER BY event_sequence ASC
                LIMIT $4
                """,
                normalized_dataset,
                cursor,
                normalized_documents,
                bounded_limit,
            )
        return [self._row_to_dict(row) for row in rows]

    async def get_document_progress_event_bounds(
        self,
        dataset_id: str,
    ) -> tuple[int | None, int | None]:
        """Return the oldest and newest retained event sequence for a dataset."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_dataset:
            raise ValueError("dataset_id is required")

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT MIN(event_sequence) AS oldest_sequence,
                       MAX(event_sequence) AS newest_sequence
                FROM kb_document_progress_events
                WHERE dataset_id = $1
                """,
                normalized_dataset,
            )
        if not row:
            return None, None
        oldest = row["oldest_sequence"]
        newest = row["newest_sequence"]
        return (
            int(oldest) if oldest is not None else None,
            int(newest) if newest is not None else None,
        )

    # ------------------------------------------------------------------
    # Per-document pipeline executions (migration 101). One row per queued
    # generation: the immutable input snapshot reprocess/recover replay
    # (addendum §1-T1.3 — in-flight documents must not drift to a config
    # changed after submission) plus the staging manifest for the revision
    # flip (PRD T1.5).
    # ------------------------------------------------------------------

    async def record_pipeline_execution(
        self,
        document_id: str,
        dataset_id: str,
        *,
        action: str,
        trigger_source: str = "api",
        triggered_by: str | None = None,
        process_rule_id: str | None = None,
        input_snapshot: dict[str, Any] | None = None,
        connection: Any | None = None,
    ) -> str:
        """Insert one execution row at submission time; returns its id."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_action = str(action or "").strip().lower()
        if normalized_action not in INGEST_ACTION_VOCABULARY:
            raise ValueError(f"unsupported pipeline execution action: {action}")
        normalized_trigger = str(trigger_source or "").strip().lower() or "api"
        if normalized_trigger not in {
            "upload",
            "api",
            "worker",
            "confluence_sync",
            "recover",
        }:
            raise ValueError(f"unsupported pipeline execution trigger: {trigger_source}")
        snapshot = input_snapshot if isinstance(input_snapshot, dict) else {}
        execution_id = uuid.uuid4().hex

        async def _insert(conn: Any) -> None:
            await conn.execute(
                """
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
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, '{}'::jsonb, 'running')
                """,
                execution_id,
                document_id,
                dataset_id,
                normalized_action,
                normalized_trigger,
                str(triggered_by or "").strip() or None,
                str(process_rule_id or "").strip() or None,
                json.dumps(snapshot),
            )

        if connection is not None:
            await _insert(connection)
            return execution_id
        async with self._pool.acquire() as conn:
            await _insert(conn)
        return execution_id

    async def link_pipeline_execution(
        self,
        document_id: str,
        execution_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """Persist the queued-generation -> execution link on the document row.

        Internal writer only (the key is reserved against API callers). Keeping
        the link in metadata lets a requeued generation (cancel/requeue, crash
        recovery) find its execution row instead of opening a duplicate.
        """

        if not self._pool:
            return False
        normalized_document = str(document_id or "").strip()
        normalized_execution = str(execution_id or "").strip()
        if not normalized_document or not normalized_execution:
            return False

        async def _link(conn: Any) -> bool:
            row = await conn.fetchrow(
                f"""
                UPDATE documents
                SET metadata = jsonb_set(
                        COALESCE(metadata, '{{}}'::jsonb),
                        '{{{DOCUMENT_PIPELINE_EXECUTION_KEY}}}',
                        to_jsonb($2::text)
                    )
                WHERE document_id = $1
                RETURNING document_id
                """,
                normalized_document,
                normalized_execution,
            )
            return row is not None

        if connection is not None:
            return await _link(connection)
        async with self._pool.acquire() as conn:
            return await _link(conn)

    async def get_pipeline_execution(
        self,
        execution_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """Return one execution row (snapshot + manifest + status)."""

        if not self._pool:
            return None
        normalized = str(execution_id or "").strip()
        if not normalized:
            return None

        async def _fetch(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                SELECT execution_id, document_id, dataset_id, action,
                       trigger_source, triggered_by, process_rule_id,
                       input_snapshot, manifest, status, error,
                       created_at, completed_at
                FROM document_pipeline_executions
                WHERE execution_id = $1
                """,
                normalized,
            )

        if connection is not None:
            row = await _fetch(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _fetch(conn)
        return self._row_to_dict(row) if row is not None else None

    async def get_latest_pipeline_execution(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """Return the newest execution row for one document, if any."""

        if not self._pool:
            return None
        normalized = str(document_id or "").strip()
        if not normalized:
            return None

        async def _fetch(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                SELECT execution_id, document_id, dataset_id, action,
                       trigger_source, triggered_by, process_rule_id,
                       input_snapshot, manifest, status, error,
                       created_at, completed_at
                FROM document_pipeline_executions
                WHERE document_id = $1
                ORDER BY created_at DESC, execution_id DESC
                LIMIT 1
                """,
                normalized,
            )

        if connection is not None:
            row = await _fetch(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _fetch(conn)
        return self._row_to_dict(row) if row is not None else None

    async def complete_pipeline_execution(
        self,
        execution_id: str,
        *,
        status: str,
        error: str | None = None,
        manifest: dict[str, Any] | list[Any] | None = None,
        connection: Any | None = None,
    ) -> bool:
        """Close one execution row; the manifest records the revision flip."""

        if not self._pool:
            return False
        normalized = str(execution_id or "").strip()
        if not normalized:
            return False
        normalized_status = str(status or "").strip().lower()
        if normalized_status not in {"completed", "error"}:
            raise ValueError("pipeline execution status must be completed or error")

        async def _close(conn: Any) -> bool:
            if manifest is None:
                row = await conn.fetchrow(
                    """
                    UPDATE document_pipeline_executions
                    SET status = $2,
                        error = $3,
                        completed_at = NOW()
                    WHERE execution_id = $1 AND status = 'running'
                    RETURNING execution_id
                    """,
                    normalized,
                    normalized_status,
                    str(error or "").strip() or None,
                )
            else:
                row = await conn.fetchrow(
                    """
                    UPDATE document_pipeline_executions
                    SET status = $2,
                        error = $3,
                        manifest = CASE
                            WHEN jsonb_typeof($4::jsonb) = 'object' THEN
                                COALESCE(manifest, '{}'::jsonb) || $4::jsonb
                            ELSE $4::jsonb
                        END,
                        completed_at = NOW()
                    WHERE execution_id = $1 AND status = 'running'
                    RETURNING execution_id
                    """,
                    normalized,
                    normalized_status,
                    str(error or "").strip() or None,
                    json.dumps(manifest),
                )
            return row is not None

        if connection is not None:
            return await _close(connection)
        async with self._pool.acquire() as conn:
            return await _close(conn)

    async def record_special_publication_manifest(
        self,
        execution_id: str,
        document_id: str,
        dataset_id: str,
        generation_id: str,
        *,
        source_hash: str,
        plan_hash: str | None = None,
        planned_object_keys: list[str] | None = None,
        collections: dict[str, dict[str, list[str]]] | None = None,
        objects: dict[str, str] | None = None,
        phase: str = "preparing",
        connection: Any | None = None,
    ) -> dict[str, Any]:
        """Pin a special generation before its first Qdrant/object-store write.

        ``preparing`` may have no point/object plan yet. The complete immutable
        plan is supplied by ``advance_special_publication_manifest`` before
        candidate points are published. A repeated call for the same generation
        is read-only; a different generation never takes over a running owner.
        """

        if phase not in {"preparing", "prepared"}:
            raise ValueError("special publication must begin in preparing or prepared")
        if not all(str(value or "").strip() for value in (
            execution_id, document_id, dataset_id, generation_id
        )):
            raise ValueError("special publication identity is incomplete")
        normalized_collections, normalized_objects = _special_publication_plan(
            collections, objects
        )
        normalized_source_hash = _special_source_hash(source_hash)
        normalized_plan_hash = _special_source_hash(plan_hash) if plan_hash is not None else None
        normalized_object_keys = _special_object_keys(planned_object_keys)
        if phase == "prepared" and not (normalized_collections or normalized_objects):
            raise ValueError("prepared special publication requires a complete plan")
        if phase == "prepared" and normalized_plan_hash is None:
            raise ValueError("prepared special publication requires plan_hash")
        if phase == "preparing" and normalized_plan_hash is not None:
            raise ValueError("preparing special publication cannot freeze plan_hash")
        if phase == "prepared" and sorted(normalized_objects) != normalized_object_keys:
            raise ValueError("prepared object receipts do not cover planned keys")

        async def _record(conn: Any) -> dict[str, Any]:
            document = await conn.fetchrow(
                """SELECT status, metadata FROM documents
                   WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE""",
                document_id, dataset_id,
            )
            execution = await conn.fetchrow(
                """SELECT status, manifest FROM document_pipeline_executions
                   WHERE execution_id = $1 AND document_id = $2 AND dataset_id = $3
                   FOR UPDATE""",
                execution_id, document_id, dataset_id,
            )
            if document is None or execution is None or execution["status"] != "running":
                raise RuntimeError("special publication execution is not the running owner")
            document_metadata = _json_object(document["metadata"])
            existing_generation = str(
                document_metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) or ""
            )
            if existing_generation and existing_generation != generation_id:
                raise RuntimeError("another special publication generation owns the document")
            manifest = _json_object(execution["manifest"])
            existing = _json_object(manifest.get(SPECIAL_PUBLICATION_MANIFEST_KEY))
            if existing:
                if existing.get("generation_id") != generation_id:
                    raise RuntimeError("special publication manifest belongs to another generation")
                if existing_generation != generation_id:
                    raise RuntimeError("special publication lost its document generation owner")
                if (
                    existing.get("phase") not in {"committed", "aborted"}
                    and document_metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY)
                    != execution_id
                ):
                    raise RuntimeError("special publication lost its document execution owner")
                if existing.get("source_hash") != normalized_source_hash:
                    raise RuntimeError("special publication source hash changed")
                if normalized_plan_hash and existing.get("plan_hash") != normalized_plan_hash:
                    raise RuntimeError("special publication candidate plan hash changed")
                if (
                    planned_object_keys is not None
                    and existing.get("planned_object_keys") != normalized_object_keys
                ):
                    raise RuntimeError("special publication planned object keys changed")
                if normalized_collections and existing.get("collections") != normalized_collections:
                    raise RuntimeError("special publication collection plan changed")
                if normalized_objects and existing.get("objects") != normalized_objects:
                    raise RuntimeError("special publication object plan changed")
                if phase == "prepared" and existing.get("phase") == "preparing":
                    raise RuntimeError("preparing publication requires a plan CAS")
                return existing
            if document_metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) != execution_id:
                raise RuntimeError("special publication lost its document execution owner")
            if document["status"] in {"completed", "error"}:
                raise RuntimeError("special publication cannot begin on a terminal document")
            special = {
                "schema_version": 1,
                "generation_id": generation_id,
                "source_hash": normalized_source_hash,
                "plan_hash": normalized_plan_hash,
                "planned_object_keys": normalized_object_keys,
                "phase": phase,
                "collections": normalized_collections,
                "objects": normalized_objects,
            }
            await conn.execute(
                """UPDATE document_pipeline_executions
                   SET manifest = jsonb_set(COALESCE(manifest, '{}'::jsonb),
                       '{special_publication}', $2::jsonb, TRUE)
                   WHERE execution_id = $1 AND status = 'running'""",
                execution_id, json.dumps(special),
            )
            await conn.execute(
                """UPDATE documents
                   SET metadata = jsonb_set(COALESCE(metadata, '{}'::jsonb),
                       '{_special_publication_generation_id}', to_jsonb($3::text), TRUE)
                   WHERE document_id = $1 AND dataset_id = $2""",
                document_id, dataset_id, generation_id,
            )
            return special

        if connection is not None:
            return await _record(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn, conn.transaction():
            return await _record(conn)

    async def resume_special_preparing_execution(
        self,
        execution_id: str,
        document_id: str,
        dataset_id: str,
        generation_id: str,
        source_hash: str,
        *,
        connection: Any,
    ) -> None:
        """Requeue a crash-stopped preparation under its original execution.

        The caller has verified cleanup of every predeclared object. No
        candidate points can exist in the preparing phase, so the original
        pinned input may be replayed without an external write duplication.
        This mutation and the revision-fence release share one transaction.
        """

        execution = await connection.fetchrow(
            "SELECT status, action, process_rule_id, input_snapshot, manifest "
            "FROM document_pipeline_executions WHERE execution_id = $1 "
            "AND document_id = $2 AND dataset_id = $3 FOR UPDATE",
            execution_id, document_id, dataset_id,
        )
        document = await connection.fetchrow(
            "SELECT status, metadata FROM documents "
            "WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE",
            document_id, dataset_id,
        )
        if execution is None or document is None or execution["status"] != "running":
            raise RuntimeError("special preparation replay lost its running owner")
        manifest = _json_object(execution["manifest"])
        special = _json_object(manifest.get(SPECIAL_PUBLICATION_MANIFEST_KEY))
        metadata = _json_object(document["metadata"])
        if (
            special.get("phase") != "preparing"
            or special.get("generation_id") != generation_id
            or special.get("source_hash") != source_hash
            or metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id
            or metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) != execution_id
            or document["status"] not in {"waiting", "parsing", "splitting", "indexing"}
            or not _json_object(execution["input_snapshot"])
            or not execution["process_rule_id"]
        ):
            raise RuntimeError("special preparation replay identity changed")
        updated_execution = await connection.execute(
            "UPDATE document_pipeline_executions "
            "SET manifest = (COALESCE(manifest, '{}'::jsonb) - $2::text) "
            "|| jsonb_build_object('preparing_replayed_after_restart', TRUE) "
            "WHERE execution_id = $1 AND status = 'running'",
            execution_id, SPECIAL_PUBLICATION_MANIFEST_KEY,
        )
        updated_document = await connection.execute(
            "UPDATE documents SET status = 'waiting', progress = 0, error = NULL, "
            "process_rule_id = $5, "
            "started_at = NULL, parsing_started_at = NULL, "
            "splitting_started_at = NULL, indexing_started_at = NULL, "
            "updated_at = NOW(), "
            "metadata = COALESCE(metadata, '{}'::jsonb) - $3::text "
            "WHERE document_id = $1 AND dataset_id = $2 "
            "AND metadata ->> $3::text = $4",
            document_id, dataset_id,
            DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY, generation_id,
            execution["process_rule_id"],
        )
        if updated_execution != "UPDATE 1" or updated_document != "UPDATE 1":
            raise RuntimeError("special preparation replay CAS failed")

    async def advance_special_publication_manifest(
        self,
        execution_id: str,
        document_id: str,
        dataset_id: str,
        generation_id: str,
        *,
        source_hash: str,
        plan_hash: str | None = None,
        planned_object_keys: list[str] | None = None,
        expected_phase: str,
        next_phase: str,
        collections: dict[str, dict[str, list[str]]] | None = None,
        objects: dict[str, str] | None = None,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        """CAS the generation phase and freeze its point/object plan.

        Only ``preparing`` permits additive plan entries. ``committed`` is a
        PostgreSQL publication proof and must be written in the same caller
        transaction as the document's completed state and source-version flip.
        """

        transitions = {
            "preparing": {"preparing", "prepared", "aborted"},
            "prepared": {"points_written", "aborted"},
            "points_written": {"committed", "aborted"},
        }
        if next_phase not in transitions.get(expected_phase, set()):
            raise ValueError("unsupported special publication phase transition")
        additions, new_objects = _special_publication_plan(collections, objects)
        normalized_source_hash = _special_source_hash(source_hash)
        normalized_plan_hash = _special_source_hash(plan_hash) if plan_hash is not None else None
        normalized_object_keys = _special_object_keys(planned_object_keys)
        if next_phase in {"prepared", "points_written", "committed"} and not normalized_plan_hash:
            raise ValueError("prepared special publication requires plan_hash")
        if next_phase == "preparing" and normalized_plan_hash is not None:
            raise ValueError("preparing special publication cannot freeze plan_hash")

        async def _advance(conn: Any) -> dict[str, Any]:
            document = await conn.fetchrow(
                """SELECT status, metadata FROM documents
                   WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE""",
                document_id, dataset_id,
            )
            execution = await conn.fetchrow(
                """SELECT status, manifest FROM document_pipeline_executions
                   WHERE execution_id = $1 AND document_id = $2 AND dataset_id = $3
                   FOR UPDATE""",
                execution_id, document_id, dataset_id,
            )
            if document is None or execution is None or execution["status"] != "running":
                raise RuntimeError("special publication execution is not the running owner")
            document_metadata = _json_object(document["metadata"])
            if document_metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id:
                raise RuntimeError("special publication lost its document generation owner")
            manifest = _json_object(execution["manifest"])
            special = _json_object(manifest.get(SPECIAL_PUBLICATION_MANIFEST_KEY))
            if special.get("generation_id") != generation_id:
                raise RuntimeError("special publication manifest generation differs")
            if special.get("source_hash") != normalized_source_hash:
                raise RuntimeError("special publication source hash changed")
            if normalized_plan_hash and special.get("plan_hash") not in {
                None, normalized_plan_hash
            }:
                raise RuntimeError("special publication candidate plan hash changed")
            frozen_object_keys = special.get("planned_object_keys")
            if not isinstance(frozen_object_keys, list):
                raise RuntimeError("special publication object key plan is missing")
            if (
                planned_object_keys is not None
                and frozen_object_keys != normalized_object_keys
            ):
                raise RuntimeError("special publication planned object keys changed")
            frozen_collections = dict(_json_object(special.get("collections")))
            frozen_objects = dict(_json_object(special.get("objects")))
            if special.get("phase") == next_phase and expected_phase != next_phase:
                if (
                    normalized_plan_hash is not None
                    and special.get("plan_hash") != normalized_plan_hash
                ):
                    raise RuntimeError("special publication candidate plan hash changed")
                if additions and additions != frozen_collections:
                    raise RuntimeError("special publication collection plan changed")
                if new_objects and new_objects != frozen_objects:
                    raise RuntimeError("special publication object plan changed")
                return special
            if special.get("phase") != expected_phase:
                raise RuntimeError("special publication phase CAS failed")
            if expected_phase == "preparing":
                for name, points in additions.items():
                    if name in frozen_collections and frozen_collections[name] != points:
                        raise RuntimeError("special publication collection plan changed")
                    frozen_collections[name] = points
                for key, digest in new_objects.items():
                    if key in frozen_objects and frozen_objects[key] != digest:
                        raise RuntimeError("special publication object plan changed")
                    frozen_objects[key] = digest
                if next_phase == "prepared" and not (frozen_collections or frozen_objects):
                    raise RuntimeError("special publication plan is incomplete")
                if next_phase == "prepared" and sorted(frozen_objects) != frozen_object_keys:
                    raise RuntimeError("prepared object receipts do not cover planned keys")
                if next_phase == "prepared" and special.get("plan_hash") not in {
                    None, normalized_plan_hash
                }:
                    raise RuntimeError("special publication candidate plan hash changed")
            elif additions or new_objects:
                if additions != frozen_collections or new_objects != frozen_objects:
                    raise RuntimeError("prepared special publication plan is immutable")
            committed_revision = special.get("publication_revision")
            if next_phase == "committed" and document["status"] != "completed":
                raise RuntimeError("special publication document is not completed")
            if next_phase == "committed":
                if type(committed_revision) is not int or committed_revision <= 0:
                    raise RuntimeError("special publication has no bound lease revision")
                # Migration 076 advances the negative revision for segment and
                # document writes. Freeze its final value with the PG authority
                # commit so a crash can still find this exact generation.
                dataset = await conn.fetchrow(
                    """SELECT content_revision FROM datasets
                       WHERE dataset_id = $1 AND is_deleted = FALSE FOR UPDATE""",
                    dataset_id,
                )
                if dataset is None or int(dataset["content_revision"]) >= 0:
                    raise RuntimeError("special publication lost its negative revision")
                current_revision = abs(int(dataset["content_revision"]))
                if current_revision > committed_revision:
                    raise RuntimeError("special publication negative revision changed owner")
                committed_revision = current_revision
            updated = {
                **special,
                "phase": next_phase,
                "plan_hash": normalized_plan_hash or special.get("plan_hash"),
                "collections": dict(sorted(frozen_collections.items())),
                "objects": dict(sorted(frozen_objects.items())),
            }
            if next_phase == "committed":
                updated["publication_revision"] = committed_revision
            await conn.execute(
                """UPDATE document_pipeline_executions
                   SET manifest = jsonb_set(COALESCE(manifest, '{}'::jsonb),
                       '{special_publication}', $2::jsonb, TRUE)
                   WHERE execution_id = $1 AND status = 'running'""",
                execution_id, json.dumps(updated),
            )
            return updated

        if connection is not None:
            return await _advance(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn, conn.transaction():
            return await _advance(conn)

    async def get_special_publication_manifest(
        self,
        execution_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """Read one durable special-generation owner without guessing from rows."""

        async def _read(conn: Any) -> dict[str, Any] | None:
            row = await conn.fetchrow(
                """SELECT execution_id, document_id, dataset_id, status, manifest
                   FROM document_pipeline_executions WHERE execution_id = $1""",
                execution_id,
            )
            if row is None:
                return None
            special = _json_object(
                _json_object(row["manifest"]).get(SPECIAL_PUBLICATION_MANIFEST_KEY)
            )
            if not special:
                return None
            return {
                "execution_id": str(row["execution_id"]),
                "document_id": str(row["document_id"]),
                "dataset_id": str(row["dataset_id"]),
                "execution_status": str(row["status"]),
                **special,
            }

        if connection is not None:
            return await _read(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            return await _read(conn)

    async def bind_special_publication_revision(
        self,
        execution_id: str,
        dataset_id: str,
        revision: int,
        *,
        connection: Any,
    ) -> dict[str, Any]:
        """Bind one preparing generation to this lease's negative revision.

        The caller owns the connection and transaction that wrote the negative
        dataset revision. No second connection may observe or bind the lease.
        """

        publication_revision = int(revision)
        if publication_revision >= 0 or connection is None:
            raise ValueError("special publication binding needs a negative lease revision")
        in_transaction = getattr(connection, "is_in_transaction", None)
        if not callable(in_transaction) or not in_transaction():
            raise RuntimeError("special publication binding requires the lease transaction")
        dataset = await connection.fetchrow(
            """SELECT content_revision FROM datasets
               WHERE dataset_id = $1 AND is_deleted = FALSE FOR UPDATE""",
            dataset_id,
        )
        if dataset is None or int(dataset["content_revision"]) != publication_revision:
            raise RuntimeError("special publication lease revision changed before binding")
        identity = await connection.fetchrow(
            """SELECT document_id FROM document_pipeline_executions
               WHERE execution_id = $1 AND dataset_id = $2""",
            execution_id, dataset_id,
        )
        if identity is None:
            raise RuntimeError("special publication execution owner is missing")
        document_id = str(identity["document_id"])
        document = await connection.fetchrow(
            """SELECT metadata FROM documents
               WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE""",
            document_id, dataset_id,
        )
        execution = await connection.fetchrow(
            """SELECT status, manifest FROM document_pipeline_executions
               WHERE execution_id = $1 AND document_id = $2 AND dataset_id = $3
               FOR UPDATE""",
            execution_id, document_id, dataset_id,
        )
        if document is None or execution is None or execution["status"] != "running":
            raise RuntimeError("special publication execution is not the running owner")
        metadata = _json_object(document["metadata"])
        special = _json_object(
            _json_object(execution["manifest"]).get(SPECIAL_PUBLICATION_MANIFEST_KEY)
        )
        generation_id = str(special.get("generation_id") or "")
        if (
            special.get("phase") != "preparing"
            or not generation_id
            or metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id
            or metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) != execution_id
        ):
            raise RuntimeError("special publication preparing owner changed")
        bound_revision = special.get("publication_revision")
        if bound_revision is not None and bound_revision != abs(publication_revision):
            raise RuntimeError("special publication is bound to another revision")
        conflicting = await connection.fetchval(
            """SELECT execution_id FROM document_pipeline_executions
               WHERE dataset_id = $1 AND execution_id <> $2
                 AND status = 'running'
                 AND manifest #>> '{special_publication,publication_revision}' = $3
                 AND manifest #>> '{special_publication,phase}'
                     IN ('preparing', 'prepared', 'points_written', 'committed')
               LIMIT 1""",
            dataset_id, execution_id, str(abs(publication_revision)),
        )
        if conflicting is not None:
            raise RuntimeError("another special publication owns this revision")
        if bound_revision is not None:
            return special
        bound = {**special, "publication_revision": abs(publication_revision)}
        await connection.execute(
            """UPDATE document_pipeline_executions
               SET manifest = jsonb_set(COALESCE(manifest, '{}'::jsonb),
                   '{special_publication}', $2::jsonb, TRUE)
               WHERE execution_id = $1 AND status = 'running'""",
            execution_id, json.dumps(bound),
        )
        return bound

    async def get_active_special_publication_for_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        """Return the unique running owner of a recovered negative fence.

        Only one running nonterminal generation may own this dataset's negative
        fence. Statement triggers can advance the negative revision after the
        bind, so its absolute value must remain no greater than the frozen
        binding. Other unbound preparations and terminal executions are not
        owners. Zero or multiple bound owners leave the revision untouched.
        """

        async def _read(conn: Any) -> dict[str, Any]:
            rows = await conn.fetch(
                """SELECT e.execution_id, e.document_id, e.dataset_id,
                          e.status, e.manifest, d.metadata AS document_metadata,
                          ds.content_revision, ds.tenant_id
                   FROM document_pipeline_executions AS e
                   JOIN documents AS d
                     ON d.document_id = e.document_id
                    AND d.dataset_id = e.dataset_id
                   JOIN datasets AS ds ON ds.dataset_id = e.dataset_id
                   WHERE e.dataset_id = $1 AND e.status = 'running'
                     AND ds.content_revision < 0 AND ds.is_deleted = FALSE
                     AND COALESCE(e.manifest, '{}'::jsonb) ? 'special_publication'
                     AND (e.manifest -> 'special_publication')
                         ? 'publication_revision'
                     AND e.manifest #>> '{special_publication,phase}'
                         IN ('preparing', 'prepared', 'points_written', 'committed')
                   ORDER BY e.created_at DESC, e.execution_id DESC
                   LIMIT 2""",
                dataset_id,
            )
            if len(rows) != 1:
                raise RuntimeError("negative publication has no unique running owner")
            row = rows[0]
            special = _json_object(
                _json_object(row["manifest"]).get(SPECIAL_PUBLICATION_MANIFEST_KEY)
            )
            metadata = _json_object(row["document_metadata"])
            generation_id = str(special.get("generation_id") or "")
            source_hash = str(special.get("source_hash") or "")
            plan_hash = special.get("plan_hash")
            planned_object_keys = special.get("planned_object_keys")
            publication_revision = special.get("publication_revision")
            if (
                not generation_id
                or not str(row["tenant_id"] or "").strip()
                or type(publication_revision) is not int
                or publication_revision <= 0
                or abs(int(row["content_revision"])) > publication_revision
                or len(source_hash) != 64
                or any(character not in "0123456789abcdef" for character in source_hash)
                or (
                    special.get("phase") != "preparing"
                    and (
                        not isinstance(plan_hash, str)
                        or len(plan_hash) != 64
                        or any(character not in "0123456789abcdef" for character in plan_hash)
                    )
                )
                or special.get("phase") not in {
                    "preparing", "prepared", "points_written", "committed"
                }
                or not isinstance(planned_object_keys, list)
                or planned_object_keys != _special_object_keys(planned_object_keys)
                or metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id
                or metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) != row["execution_id"]
            ):
                raise RuntimeError("negative publication owner manifest is invalid")
            return {
                "execution_id": str(row["execution_id"]),
                "document_id": str(row["document_id"]),
                "dataset_id": str(row["dataset_id"]),
                "tenant_id": str(row["tenant_id"]),
                "execution_status": str(row["status"]),
                **special,
            }

        if connection is not None:
            return await _read(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            return await _read(conn)

    async def link_special_publication_recovery(
        self,
        original_execution_id: str,
        recovery_execution_id: str,
        document_id: str,
        dataset_id: str,
        generation_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        """Carry the exact partial plan onto a newly claimed recover execution.

        The ordinary queue already creates an immutable recovery execution and
        marks its predecessor ``error``. This method only transfers its special
        plan and links both rows; it never decides that external writes succeeded.
        """

        async def _link(conn: Any) -> dict[str, Any]:
            document = await conn.fetchrow(
                """SELECT metadata FROM documents
                   WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE""",
                document_id, dataset_id,
            )
            rows = await conn.fetch(
                """SELECT execution_id, status, manifest
                   FROM document_pipeline_executions
                   WHERE execution_id = ANY($1::text[])
                     AND document_id = $2 AND dataset_id = $3
                   ORDER BY execution_id FOR UPDATE""",
                [original_execution_id, recovery_execution_id], document_id, dataset_id,
            )
            by_id = {str(row["execution_id"]): row for row in rows}
            original = by_id.get(original_execution_id)
            recovered = by_id.get(recovery_execution_id)
            if document is None or original is None or recovered is None:
                raise RuntimeError("special publication recovery lineage is incomplete")
            metadata = _json_object(document["metadata"])
            if (
                metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id
                or metadata.get(DOCUMENT_PIPELINE_EXECUTION_KEY) != recovery_execution_id
                or original["status"] != "error"
                or recovered["status"] != "running"
            ):
                raise RuntimeError("special publication recovery changed owner")
            old_manifest = _json_object(original["manifest"])
            new_manifest = _json_object(recovered["manifest"])
            special = _json_object(old_manifest.get(SPECIAL_PUBLICATION_MANIFEST_KEY))
            if (
                special.get("generation_id") != generation_id
                or special.get("phase") in {"committed", "aborted"}
                or new_manifest.get("recovered_from_execution_id") not in {
                    None, original_execution_id
                }
                or old_manifest.get("recovered_by_execution_id") not in {
                    None, recovery_execution_id
                }
            ):
                raise RuntimeError("special publication recovery manifest conflicts")
            existing = _json_object(new_manifest.get(SPECIAL_PUBLICATION_MANIFEST_KEY))
            if existing and existing != special:
                raise RuntimeError("recover execution already owns another generation")
            if not existing:
                new_manifest[SPECIAL_PUBLICATION_MANIFEST_KEY] = special
                new_manifest["recovered_from_execution_id"] = original_execution_id
                await conn.execute(
                    """UPDATE document_pipeline_executions
                       SET manifest = $2::jsonb
                       WHERE execution_id = $1 AND status = 'running'""",
                    recovery_execution_id, json.dumps(new_manifest),
                )
            if old_manifest.get("recovered_by_execution_id") is None:
                old_manifest["recovered_by_execution_id"] = recovery_execution_id
                await conn.execute(
                    """UPDATE document_pipeline_executions
                       SET manifest = $2::jsonb
                       WHERE execution_id = $1 AND status = 'error'""",
                    original_execution_id, json.dumps(old_manifest),
                )
            return special

        if connection is not None:
            return await _link(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn, conn.transaction():
            return await _link(conn)

    async def reconcile_special_publication_execution(
        self,
        execution_id: str,
        document_id: str,
        dataset_id: str,
        generation_id: str,
        *,
        connection: Any | None = None,
    ) -> str:
        """Idempotently close only an evidenced committed/aborted execution.

        A completed document by itself is not proof: old running ledger rows
        without this generation's committed manifest remain ``unproven``.
        """

        async def _reconcile(conn: Any) -> str:
            document = await conn.fetchrow(
                """SELECT status, metadata FROM documents
                   WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE""",
                document_id, dataset_id,
            )
            execution = await conn.fetchrow(
                """SELECT status, manifest FROM document_pipeline_executions
                   WHERE execution_id = $1 AND document_id = $2 AND dataset_id = $3
                   FOR UPDATE""",
                execution_id, document_id, dataset_id,
            )
            if document is None or execution is None:
                return "unproven"
            metadata = _json_object(document["metadata"])
            special = _json_object(
                _json_object(execution["manifest"]).get(SPECIAL_PUBLICATION_MANIFEST_KEY)
            )
            if (
                special.get("generation_id") != generation_id
            ):
                return "unproven"
            phase = special.get("phase")
            if phase == "committed" and document["status"] == "completed":
                terminal, error = "completed", None
            elif phase == "aborted" and document["status"] == "error":
                terminal, error = "error", "special publication aborted after cleanup"
            else:
                return "unproven"
            if execution["status"] == terminal:
                return "already_terminal"
            if execution["status"] != "running":
                return "unproven"
            if metadata.get(DOCUMENT_SPECIAL_PUBLICATION_GENERATION_KEY) != generation_id:
                return "unproven"
            closed = await self.complete_pipeline_execution(
                execution_id,
                status=terminal,
                error=error,
                connection=conn,
            )
            if not closed:
                raise RuntimeError("special publication execution close lost its CAS")
            await conn.execute(
                """UPDATE documents
                   SET metadata = COALESCE(metadata, '{}'::jsonb)
                       - '_special_publication_generation_id'
                   WHERE document_id = $1 AND dataset_id = $2
                     AND metadata ->> '_special_publication_generation_id' = $3""",
                document_id, dataset_id, generation_id,
            )
            return "reconciled"

        if connection is not None:
            return await _reconcile(connection)
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn, conn.transaction():
            return await _reconcile(conn)

    # ------------------------------------------------------------------
    # Process-rule snapshots (PRD T1 item 7). A rule row is an immutable,
    # content-addressed snapshot of the complete index configuration that
    # actually built a generation: {"index_config", "chunking",
    # "processing_mode"}. Rows
    # are pinned onto documents at generation-open and referenced from
    # document_pipeline_executions.process_rule_id; replay verbs (reprocess/
    # recover) require both snapshots to exist and agree before processing.
    # ------------------------------------------------------------------

    async def record_process_rule(
        self,
        dataset_id: str,
        *,
        mode: str,
        rules: dict[str, Any],
        created_by: str | None = None,
        connection: Any | None = None,
    ) -> str | None:
        """Return the rule id for this (dataset, mode, rules) content.

        Content-dedup by jsonb equality keeps the rule id stable while the
        dataset config is unchanged. The dedup is best-effort (select-then-
        insert): a concurrent race may leave one extra immutable row for the
        same content, which is harmless — every row is frozen by the
        migration-103 immutability trigger and pins resolve per row.
        """

        if not self._pool:
            return None
        normalized_dataset = str(dataset_id or "").strip()
        normalized_mode = str(mode or "").strip().lower()
        if not normalized_dataset or not normalized_mode:
            return None
        if not isinstance(rules, dict):
            raise ValueError("process rule snapshot must be a dict")
        payload = json.dumps(rules)
        rule_id = uuid.uuid4().hex

        async def _record(conn: Any) -> str:
            existing = await conn.fetchval(
                """
                SELECT id
                FROM dataset_process_rules
                WHERE dataset_id = $1 AND mode = $2 AND rules = $3::jsonb
                ORDER BY created_at ASC, id ASC
                LIMIT 1
                """,
                normalized_dataset,
                normalized_mode,
                payload,
            )
            if existing is not None:
                return str(existing)
            row = await conn.fetchrow(
                """
                INSERT INTO dataset_process_rules (
                    id, dataset_id, mode, rules, created_by
                )
                VALUES ($1, $2, $3, $4::jsonb, $5)
                RETURNING id
                """,
                rule_id,
                normalized_dataset,
                normalized_mode,
                payload,
                str(created_by or "").strip() or None,
            )
            return str(row["id"]) if row is not None else rule_id

        if connection is not None:
            return await _record(connection)
        async with self._pool.acquire() as conn:
            return await _record(conn)

    async def get_process_rule(
        self,
        process_rule_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        """Return one immutable rule snapshot row, if it exists."""

        if not self._pool:
            return None
        normalized = str(process_rule_id or "").strip()
        if not normalized:
            return None

        async def _fetch(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                SELECT id, dataset_id, mode, rules, created_by, created_at
                FROM dataset_process_rules
                WHERE id = $1
                """,
                normalized,
            )

        if connection is not None:
            row = await _fetch(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _fetch(conn)
        return self._row_to_dict(row) if row is not None else None

    async def pin_document_process_rule(
        self,
        document_id: str,
        process_rule_id: str,
        *,
        connection: Any | None = None,
        idle_only: bool = False,
    ) -> bool:
        """Pin the rule snapshot that governs this document's generations.

        The pin is cross-checked against the execution row during replay. A
        missing or disagreeing row is terminal; replay never uses live config.
        """

        if not self._pool:
            return False
        normalized_document = str(document_id or "").strip()
        normalized_rule = str(process_rule_id or "").strip()
        if not normalized_document or not normalized_rule:
            return False

        async def _pin(conn: Any) -> bool:
            row = await conn.fetchrow(
                """
                UPDATE documents
                SET process_rule_id = $2
                WHERE document_id = $1
                  AND (NOT $3::boolean OR status IN ('completed', 'error'))
                RETURNING document_id
                """,
                normalized_document,
                normalized_rule,
                idle_only,
            )
            return row is not None

        if connection is not None:
            return await _pin(connection)
        async with self._pool.acquire() as conn:
            return await _pin(conn)

    async def next_segment_position(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> int:
        """Allocate the next position while the caller holds the document lease."""

        if not self._pool:
            raise RuntimeError("database is not connected")

        async def _read(conn: Any) -> int:
            value = await conn.fetchval(
                """
                SELECT COALESCE(MAX(position), -1) + 1
                FROM segments
                WHERE dataset_id = $1 AND document_id = $2
                """,
                dataset_id,
                document_id,
            )
            return int(value or 0)

        if connection is not None:
            return await _read(connection)
        async with self._pool.acquire() as conn:
            return await _read(conn)

    async def _require_dataset_ingestion_identity(
        self,
        conn: Any,
        dataset_id: str,
        expected_ingestion_identity: str | None,
    ) -> dict[str, Any]:
        row = await conn.fetchrow(
            """
            SELECT tenant_id, collection_name,
                   embedding_provider, embedding_model, embedding_dimension,
                   embedding_config, index_config
            FROM datasets
            WHERE dataset_id = $1 AND is_deleted = FALSE AND is_archived = FALSE
            """,
            dataset_id,
        )
        if not row:
            raise RuntimeError("dataset was deleted or archived before index write; refusing orphan content")
        if dataset_index_deletion_fence(dict(row)) is not None:
            raise RuntimeError("dataset index deletion is pending; refusing indexed-content access")
        if (
            expected_ingestion_identity is not None
            and dataset_ingestion_identity(dict(row)) != expected_ingestion_identity
        ):
            raise RuntimeError(
                "dataset ingestion identity changed; refusing a mixed index generation"
            )
        return dict(row)

    @contextlib.asynccontextmanager
    async def dataset_index_write_lease(
        self,
        dataset_id: str,
        document_ids: list[str],
        *,
        expected_ingestion_identity: str | None = None,
    ):
        """Fence one Qdrant upsert against dataset/document deletion.

        The transaction-scoped shared advisory lock is held across the remote
        upsert. A writer that encounters an in-progress exclusive deletion
        fails closed instead of waiting until the deletion has cleared its
        marker and then recreating a point that was just removed.
        """
        if not self._pool:
            raise RuntimeError("database is not connected")
        lock_name = self._dataset_index_lock_name(dataset_id)
        normalized_ids = sorted(
            {str(document_id).strip() for document_id in document_ids if str(document_id).strip()}
        )
        async with self._pool.acquire() as conn, conn.transaction():
            acquired = await conn.fetchval(
                "SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1, 0))",
                lock_name,
            )
            if acquired is not True:
                raise RuntimeError(
                    "dataset index deletion is in progress; refusing a queued vector write"
                )
            await self._require_dataset_ingestion_identity(
                conn,
                dataset_id,
                expected_ingestion_identity,
            )
            if normalized_ids:
                count = await conn.fetchval(
                    f"""
                    SELECT COUNT(*)
                    FROM documents
                    WHERE dataset_id = $1
                      AND document_id = ANY($2::text[])
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
                            OR metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                ->> 'status' = 'pending'
                                AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                    ->> 'desired_enabled' = 'true'
                                AND metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'
                                    ->> 'desired_archived' = 'false'
                      )
                    """,
                    dataset_id,
                    normalized_ids,
                )
                if int(count or 0) != len(normalized_ids):
                    raise RuntimeError(
                        "document is missing or inactive before vector write; "
                        "refusing orphan or disabled points"
                    )
            yield

    @contextlib.asynccontextmanager
    async def dataset_index_delete_lease(self, dataset_id: str):
        """Exclude centrally routed index writes/schema mutation during deletion.

        This is deliberately a session advisory lock, rather than a
        transaction-scoped lock. The yielded connection can commit the durable
        deletion marker before the first remote mutation while retaining the
        exclusive barrier until the caller finishes or fails.
        """
        if not self._pool:
            raise RuntimeError("database is not connected")
        lock_name = self._dataset_index_lock_name(dataset_id)
        async with self._pool.acquire() as conn:
            acquired = False
            try:
                acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    lock_name,
                )
                if acquired is not True:
                    raise IndexLeaseUnavailableError(
                        "dataset index lifecycle work is already in progress"
                    )
                yield conn
            finally:
                if acquired:
                    unlock_task = asyncio.create_task(
                        conn.fetchval(
                            "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                            lock_name,
                        )
                    )
                    try:
                        unlocked = await asyncio.shield(unlock_task)
                    except asyncio.CancelledError:
                        unlocked = await unlock_task
                        raise
                    if unlocked is not True:
                        raise RuntimeError("dataset index deletion lease was not released")

    async def set_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> tuple[dict[str, Any], bool]:
        """CAS-set a durable deletion marker on the exclusive lease connection.

        An exact same-target retry reuses the existing marker. A different or
        malformed marker is never overwritten.
        """

        if connection is None:
            raise RuntimeError("exclusive dataset deletion lease connection is required")
        marker = make_dataset_index_deletion_fence(operation, target_id)
        marker_json = json.dumps(marker, separators=(",", ":"), sort_keys=True)
        row = await connection.fetchrow(
            f"""
            UPDATE datasets
            SET index_config = jsonb_set(
                    COALESCE(index_config, '{{}}'::jsonb),
                    '{{retrieval}}',
                    CASE
                        WHEN jsonb_typeof(index_config->'retrieval') = 'object'
                        THEN index_config->'retrieval'
                        ELSE '{{}}'::jsonb
                    END
                        || jsonb_build_object(
                            '{INDEX_DELETION_FENCE_KEY}', $2::jsonb
                        ),
                    TRUE
                ),
                content_revision = COALESCE(content_revision, 0) + 1,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND is_deleted = FALSE
              AND jsonb_typeof(COALESCE(index_config, '{{}}'::jsonb)) = 'object'
              AND NOT (
                  CASE
                      WHEN jsonb_typeof(index_config->'retrieval') = 'object'
                      THEN index_config->'retrieval'
                      ELSE '{{}}'::jsonb
                  END
                  ? '{INDEX_DELETION_FENCE_KEY}'
              )
            RETURNING *
            """,
            dataset_id,
            marker_json,
        )
        if row:
            created_dataset = self._row_to_dict(row)
            if dataset_index_deletion_fence(created_dataset) != marker:
                raise RuntimeError("dataset index deletion fence CAS verification failed")
            return created_dataset, True

        current = await connection.fetchrow(
            "SELECT * FROM datasets WHERE dataset_id = $1 AND is_deleted = FALSE",
            dataset_id,
        )
        if not current:
            raise RuntimeError("dataset was deleted before index deletion")
        current_dataset = self._row_to_dict(current)
        existing = dataset_index_deletion_fence(current_dataset)
        if existing != marker:
            raise RuntimeError("another dataset index deletion target is already pending")
        return current_dataset, False

    async def clear_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> bool:
        """Clear only the exact marker owned by the successful delete target."""

        if connection is None:
            raise RuntimeError("exclusive dataset deletion lease connection is required")
        marker = make_dataset_index_deletion_fence(operation, target_id)
        marker_json = json.dumps(marker, separators=(",", ":"), sort_keys=True)
        row = await connection.fetchrow(
            f"""
            UPDATE datasets
            SET index_config = jsonb_set(
                    COALESCE(index_config, '{{}}'::jsonb),
                    '{{retrieval}}',
                    CASE
                        WHEN jsonb_typeof(index_config->'retrieval') = 'object'
                        THEN index_config->'retrieval'
                        ELSE '{{}}'::jsonb
                    END
                        - '{INDEX_DELETION_FENCE_KEY}',
                    TRUE
                ),
                content_revision = COALESCE(content_revision, 0) + 1,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND is_deleted = FALSE
              AND jsonb_typeof(COALESCE(index_config, '{{}}'::jsonb)) = 'object'
              AND CASE
                      WHEN jsonb_typeof(index_config->'retrieval') = 'object'
                      THEN index_config->'retrieval'
                      ELSE '{{}}'::jsonb
                  END
                    ->'{INDEX_DELETION_FENCE_KEY}' = $2::jsonb
            RETURNING dataset_id
            """,
            dataset_id,
            marker_json,
        )
        return row is not None

    async def clear_dataset_needs_reindex(self, dataset_id: str) -> None:
        """Clear the needs_reindex flag after successful document reindexing."""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE datasets
                SET needs_reindex = false,
                    content_revision = content_revision + 1,
                    updated_at = NOW()
                WHERE dataset_id = $1
                """,
                dataset_id,
            )
            logger.info(f"Cleared needs_reindex flag for dataset {dataset_id}")

    async def bump_dataset_content_revision(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        """Advance the dataset's authoritative content revision.

        Retrieval cache keys and lexical-transition identities are bound to
        ``content_revision`` via the dataset revision fingerprint (PRD T1
        unified lifecycle contract, §885-886; retrieval cache contract §129:
        写后不可能读到旧值). Every transition that changes which content is
        visible for retrieval must advance the revision so a cached result
        can never outlive the transition. The restore direction bumps
        atomically inside the activation status write instead.

        Deployments also carry the 076 provenance triggers, which advance
        the revision on retrieval-effective documents/segments writes; this
        writer is the knowledge service's own explicit guarantee, kept
        independent of that platform trigger so the cache contract holds
        even on a schema provisioned without it. Extra advancement is
        harmless — nothing depends on revision adjacency.
        """

        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_dataset:
            raise ValueError("dataset_id is required")
        query = """
            UPDATE datasets
            SET content_revision = COALESCE(content_revision, 0) + 1,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND is_deleted = FALSE
            RETURNING dataset_id
        """
        if connection is not None:
            row = await connection.fetchrow(query, normalized_dataset)
            return row is not None
        if not self._pool:
            raise RuntimeError("database is not connected")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, normalized_dataset)
            return row is not None

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
        include_archived: bool = False,
    ) -> dict[str, Any] | None:
        """获取 Dataset"""
        if not self._pool:
            return None
        query = (
            "SELECT * FROM datasets WHERE dataset_id = $1 AND is_deleted = FALSE"
            + ("" if include_archived else " AND is_archived = FALSE")
        )
        if connection is not None:
            row = await connection.fetchrow(query, dataset_id)
            return self._row_to_dict(row) if row else None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(query, dataset_id)
            return self._row_to_dict(row) if row else None

    async def list_datasets(
        self,
        tenant_id: str | None = None,
        include_public: bool = True,
        limit: int = 100,
        offset: int = 0,
        before_created_at: Any | None = None,
        before_dataset_id: str | None = None,
        archived_only: bool = False,
    ) -> list[dict[str, Any]]:
        """列出 Dataset"""
        if not self._pool:
            return []

        query = (
            "SELECT * FROM datasets WHERE is_deleted = FALSE"
            + (" AND is_archived = TRUE" if archived_only else " AND is_archived = FALSE")
        )
        params: list[Any] = []
        param_idx = 1

        if tenant_id:
            if include_public:
                query += f" AND (tenant_id = ${param_idx} OR visibility = 'public')"
                params.append(tenant_id)
                param_idx += 1
            else:
                query += f" AND tenant_id = ${param_idx}"
                params.append(tenant_id)
                param_idx += 1
        else:
            if not include_public:
                query += " AND visibility != 'public'"

        if before_created_at is not None and before_dataset_id is not None:
            query += f" AND (created_at, dataset_id) < (${param_idx}::timestamptz, ${param_idx + 1}::text)"
            params.extend([before_created_at, before_dataset_id])
            param_idx += 2
        query += f" ORDER BY created_at DESC, dataset_id DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        params.extend([limit, offset])

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def set_dataset_archived(
        self,
        dataset_id: str,
        *,
        archived: bool,
        user_id: str,
        tenant_id: str,
        roles: list[str],
        tenant_admin: bool,
        connection: Any,
        reason: str | None = None,
    ) -> dict[str, Any] | None:
        """Change archive visibility without touching content, grants, or bindings.

        The owner proof, current state, and revision increment share one row
        lock and transaction. Repeating the desired state leaves the revision
        unchanged.
        """

        if not self._pool:
            raise RuntimeError("database is not connected")
        if not dataset_id or not user_id:
            raise ValueError("dataset and actor identity are required")
        if connection is None:
            raise RuntimeError("exclusive dataset index lease is required for archiving")
        conn = connection
        async with conn.transaction():
            row = await conn.fetchrow(
                "SELECT * FROM datasets WHERE dataset_id = $1 AND is_deleted = FALSE FOR UPDATE",
                dataset_id,
            )
            if row is None:
                return None
            dataset = self._row_to_dict(row)
            same_tenant = bool(tenant_id and tenant_id == str(dataset.get("tenant_id") or ""))
            owner_grant = await conn.fetchval(
                """SELECT EXISTS (
                       SELECT 1 FROM dataset_permissions
                       WHERE dataset_id = $1 AND permission = 'owner'
                         AND (
                           (subject_type = 'user' AND subject_id = $2)
                           OR (subject_type = 'tenant_role' AND $3::boolean
                               AND subject_tenant_id = $5
                               AND subject_id = ANY($4::text[]))
                         )
                       FOR SHARE
                   )""",
                dataset_id, user_id, same_tenant, list(roles or []), tenant_id,
            )
            if not (
                (same_tenant and tenant_admin)
                or str(dataset.get("created_by") or "") == user_id
                or owner_grant
            ):
                raise PermissionError("dataset owner permission changed")
            if bool(dataset.get("is_archived")) == archived:
                return dataset
            if archived:
                # The exclusive dataset lease proves no current writer can be
                # between ledger creation, link, and terminal completion.
                # Retire legacy/unlinked receipts and a completed document's
                # missed best-effort close before testing active ownership.
                # A nonterminal special manifest remains a hard blocker.
                await conn.execute(
                    f"""UPDATE document_pipeline_executions AS execution
                       SET status = CASE
                           WHEN document.metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                                = execution.execution_id
                                AND document.status = 'completed'
                           THEN 'completed' ELSE 'error' END,
                           error = CASE
                           WHEN document.metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                                = execution.execution_id
                                AND document.status = 'completed'
                           THEN NULL ELSE 'Execution was not active when dataset was archived' END,
                           completed_at = NOW()
                       FROM documents AS document
                       WHERE execution.dataset_id = $1
                         AND execution.document_id = document.document_id
                         AND execution.status = 'running'
                         AND COALESCE(execution.manifest -> 'special_publication'
                                      ->> 'phase', '') NOT IN
                             ('preparing', 'prepared', 'points_written')
                         AND (
                             document.metadata ->> '{DOCUMENT_PIPELINE_EXECUTION_KEY}'
                                 IS DISTINCT FROM execution.execution_id
                             OR document.status IN ('completed', 'error')
                         )""",
                    dataset_id,
                )
            if archived and await conn.fetchval(
                """SELECT EXISTS (
                       SELECT 1 FROM document_pipeline_executions
                       WHERE dataset_id = $1 AND status = 'running'
                   )""",
                dataset_id,
            ):
                raise RuntimeError(
                    "dataset has a running document execution; retry archive after recovery"
                )
            if int(dataset.get("content_revision") or 0) < 0:
                raise RuntimeError("dataset index publication is still pending")
            if dataset_index_deletion_fence(dataset) is not None:
                raise RuntimeError("dataset index deletion is pending")
            updated = await conn.fetchrow(
                """UPDATE datasets
                   SET is_archived = $2,
                       archived_at = CASE WHEN $2 THEN NOW() ELSE NULL END,
                       archived_by = CASE WHEN $2 THEN $3 ELSE NULL END,
                       archive_reason = CASE WHEN $2 THEN $4 ELSE NULL END,
                       content_revision = COALESCE(content_revision, 0) + 1,
                       updated_at = NOW()
                   WHERE dataset_id = $1 AND is_deleted = FALSE
                   RETURNING *""",
                dataset_id, archived, user_id, reason,
            )
            if updated is None:
                raise RuntimeError("dataset archive changed during update")
            return self._row_to_dict(updated)

    async def delete_dataset(
        self,
        dataset_id: str,
        deleted_by: str | None = None,
        delete_reason: str | None = None,
        *,
        connection: Any | None = None,
    ) -> bool:
        """软删除 Dataset，并清理关联数据。"""
        if not self._pool:
            return False

        async def _delete(conn: Any) -> bool:
            async with conn.transaction():
                target = await conn.fetchrow(
                    """
                    SELECT dataset_id
                    FROM datasets
                    WHERE dataset_id = $1
                      AND is_deleted = FALSE
                    FOR UPDATE
                    """,
                    dataset_id,
                )
                if not target:
                    return False

                await conn.execute(
                    """
                    UPDATE datasets
                    SET is_deleted = TRUE,
                        deleted_at = NOW(),
                        deleted_by = $2,
                        delete_reason = $3,
                        updated_at = NOW()
                    WHERE dataset_id = $1
                    """,
                    dataset_id,
                    deleted_by,
                    delete_reason,
                )

                # Keep dataset record for audit/compliance, remove active payload data.
                await conn.execute(
                    "DELETE FROM confluence_space_bindings WHERE dataset_id = $1", dataset_id
                )
                await conn.execute(
                    "DELETE FROM version_retention_policies WHERE dataset_id = $1", dataset_id
                )
                await conn.execute(
                    "DELETE FROM dataset_keyword_tables WHERE dataset_id = $1", dataset_id
                )
                await conn.execute(
                    "DELETE FROM dataset_process_rules WHERE dataset_id = $1", dataset_id
                )
                await conn.execute("DELETE FROM dataset_queries WHERE dataset_id = $1", dataset_id)
                await conn.execute("DELETE FROM child_chunks WHERE dataset_id = $1", dataset_id)
                await conn.execute(
                    "DELETE FROM dataset_permissions WHERE dataset_id = $1", dataset_id
                )
                await conn.execute("DELETE FROM documents WHERE dataset_id = $1", dataset_id)

            return True

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)

    async def record_dataset_query(
        self,
        *,
        dataset_id: str,
        content: str,
        source: str = "api",
        source_app_id: str | None = None,
        created_by_role: str | None = None,
        created_by: str | None = None,
        metadata: dict[str, Any] | None = None,
        trace_id: str | None = None,
        query_fingerprint: str | None = None,
        mode: str | None = None,
        top_k: int | None = None,
        hit_count: int | None = None,
        stage_timings: dict[str, Any] | None = None,
        segment_ids: list[str] | None = None,
    ) -> bool:
        """Append one retrieval-query telemetry row.

        Telemetry contract (PRD C1): independent transaction, never raises.
        A failure here must not surface to the retrieve() caller, so errors
        are logged and swallowed.
        """
        if not self._pool:
            return False
        try:
            async with self._pool.acquire() as conn:
                if trace_id:
                    async with conn.transaction():
                        inserted = await conn.fetchval(
                            """
                            INSERT INTO dataset_queries (
                                dataset_id, content, source, source_app_id,
                                created_by_role, created_by, metadata,
                                trace_id, query_fingerprint, mode, top_k,
                                hit_count, stage_timings
                            ) VALUES (
                                $1, $2, $3, $4, $5, $6, $7::jsonb,
                                $8::uuid, $9, $10, $11, $12, $13::jsonb
                            )
                            ON CONFLICT (trace_id) WHERE trace_id IS NOT NULL
                            DO NOTHING
                            RETURNING 1
                            """,
                            dataset_id,
                            content,
                            source,
                            source_app_id,
                            created_by_role,
                            created_by,
                            json.dumps(metadata or {}, ensure_ascii=False),
                            trace_id,
                            query_fingerprint,
                            mode,
                            top_k,
                            hit_count,
                            json.dumps(stage_timings or {}, ensure_ascii=False),
                        )
                        if inserted and segment_ids:
                            await conn.execute(
                                """
                                UPDATE segments
                                SET hit_count = hit_count + 1
                                WHERE dataset_id = $1
                                  AND segment_id = ANY($2::text[])
                                """,
                                dataset_id,
                                sorted(set(segment_ids)),
                            )
                    return True

                await conn.execute(
                    """
                    INSERT INTO dataset_queries (
                        dataset_id, content, source, source_app_id,
                        created_by_role, created_by, metadata
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
                    """,
                    dataset_id,
                    content,
                    source,
                    source_app_id,
                    created_by_role,
                    created_by,
                    json.dumps(metadata or {}, ensure_ascii=False),
                )
            return True
        except Exception as exc:  # telemetry must never break retrieval
            logger.warning("dataset query telemetry insert failed: %s", exc)
            return False

    async def list_dataset_queries(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        limit: int,
        zero_results: bool | None = None,
        mode: str | None = None,
        cursor_created_at: Any | None = None,
        cursor_id: str | None = None,
    ) -> list[dict[str, Any]]:
        """Return one tenant-bound keyset page of query observations."""

        if not self._pool:
            return []
        clauses = ["q.dataset_id = $1", "d.tenant_id = $2", "d.is_deleted = FALSE"]
        params: list[Any] = [dataset_id, tenant_id]
        if zero_results is not None:
            params.append(0)
            operator = "=" if zero_results else ">"
            clauses.append(f"q.hit_count {operator} ${len(params)}")
        if mode:
            params.append(mode)
            clauses.append(f"q.mode = ${len(params)}")
        if cursor_created_at is not None and cursor_id:
            params.extend([cursor_created_at, cursor_id])
            clauses.append(f"(q.created_at, q.id) < (${len(params) - 1}, ${len(params)})")
        params.append(limit)
        query = f"""
            SELECT q.id, q.dataset_id, q.content, q.source, q.source_app_id,
                   q.created_by_role, q.created_by, q.metadata, q.trace_id,
                   q.query_fingerprint, q.mode, q.top_k, q.hit_count,
                   q.stage_timings, q.created_at
            FROM dataset_queries AS q
            JOIN datasets AS d ON d.dataset_id = q.dataset_id
            WHERE {' AND '.join(clauses)}
            ORDER BY q.created_at DESC, q.id DESC
            LIMIT ${len(params)}
        """
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def get_dataset_query_fingerprint(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        trace_id: str,
    ) -> str | None:
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            return await conn.fetchval(
                """
                SELECT q.query_fingerprint
                FROM dataset_queries AS q
                JOIN datasets AS d ON d.dataset_id = q.dataset_id
                WHERE q.dataset_id = $1
                  AND d.tenant_id = $2
                  AND d.is_deleted = FALSE
                  AND q.trace_id = $3::uuid
                """,
                dataset_id,
                tenant_id,
                trace_id,
            )

    async def upsert_dataset_query_feedback(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        trace_id: str,
        query_fingerprint: str,
        target_type: str,
        target_id: str,
        rating: str,
        reason_code: str,
        comment: str | None,
        created_by: str,
    ) -> dict[str, Any] | None:
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO knowledge.dataset_query_feedback (
                    tenant_id, dataset_id, trace_id, query_fingerprint,
                    target_type, target_id, rating, reason_code, comment, created_by
                ) VALUES ($1, $2, $3::uuid, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (
                    tenant_id, dataset_id, trace_id, target_type, target_id, created_by
                ) DO UPDATE SET
                    query_fingerprint = EXCLUDED.query_fingerprint,
                    rating = EXCLUDED.rating,
                    reason_code = EXCLUDED.reason_code,
                    comment = EXCLUDED.comment,
                    updated_at = NOW()
                RETURNING *
                """,
                tenant_id,
                dataset_id,
                trace_id,
                query_fingerprint,
                target_type,
                target_id,
                rating,
                reason_code,
                comment,
                created_by,
            )
        return self._row_to_dict(row) if row else None

    async def list_dataset_query_feedback(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        limit: int,
        rating: str | None = None,
        reason_code: str | None = None,
        target_type: str | None = None,
        trace_id: str | None = None,
        cursor_created_at: Any | None = None,
        cursor_id: str | None = None,
    ) -> list[dict[str, Any]]:
        if not self._pool:
            return []
        clauses = [
            "f.tenant_id = $1",
            "f.dataset_id = $2",
            "d.tenant_id = $1",
            "d.is_deleted = FALSE",
        ]
        params: list[Any] = [tenant_id, dataset_id]
        for column, value in (
            ("rating", rating),
            ("reason_code", reason_code),
            ("target_type", target_type),
        ):
            if value:
                params.append(value)
                clauses.append(f"f.{column} = ${len(params)}")
        if trace_id:
            params.append(trace_id)
            clauses.append(f"f.trace_id = ${len(params)}::uuid")
        if cursor_created_at is not None and cursor_id:
            params.extend([cursor_created_at, cursor_id])
            clauses.append(
                f"(f.created_at, f.feedback_id) < "
                f"(${len(params) - 1}, ${len(params)}::uuid)"
            )
        params.append(limit)
        query = f"""
            SELECT f.*, q.content AS query_content
            FROM knowledge.dataset_query_feedback AS f
            JOIN datasets AS d ON d.dataset_id = f.dataset_id
            LEFT JOIN dataset_queries AS q
              ON q.dataset_id = f.dataset_id AND q.trace_id = f.trace_id
            WHERE {' AND '.join(clauses)}
            ORDER BY f.created_at DESC, f.feedback_id DESC
            LIMIT ${len(params)}
        """
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def get_datasets_statistics_batch(
        self, dataset_ids: list[str]
    ) -> dict[str, dict[str, int]]:
        """获取多个 Dataset 的统计数据（批量查询优化）"""
        if not self._pool or not dataset_ids:
            return {}

        async with self._pool.acquire() as conn:
            query = """
                WITH document_stats AS (
                    SELECT dataset_id,
                           COUNT(*)::bigint AS document_count,
                           COUNT(*) FILTER (
                               WHERE status = 'completed'
                                 AND enabled = TRUE
                                 AND archived = FALSE
                           )::bigint AS available_document_count,
                           COALESCE(SUM(word_count), 0)::bigint AS word_count
                    FROM documents
                    WHERE dataset_id = ANY($1)
                    GROUP BY dataset_id
                ), segment_stats AS (
                    SELECT dataset_id,
                           COUNT(*)::bigint AS segment_count,
                           COUNT(*) FILTER (
                               WHERE enabled = TRUE AND status = 'completed'
                           )::bigint AS available_segment_count,
                           COALESCE(SUM(hit_count), 0)::bigint AS hit_count
                    FROM segments
                    WHERE dataset_id = ANY($1)
                    GROUP BY dataset_id
                )
                SELECT
                    d.dataset_id,
                    COALESCE(doc.document_count, 0) AS document_count,
                    COALESCE(doc.available_document_count, 0) AS available_document_count,
                    COALESCE(doc.word_count, 0) AS word_count,
                    COALESCE(seg.segment_count, 0) AS segment_count,
                    COALESCE(seg.available_segment_count, 0) AS available_segment_count,
                    COALESCE(seg.hit_count, 0) AS hit_count
                FROM datasets d
                LEFT JOIN document_stats doc ON doc.dataset_id = d.dataset_id
                LEFT JOIN segment_stats seg ON seg.dataset_id = d.dataset_id
                WHERE d.dataset_id = ANY($1)
                  AND d.is_deleted = FALSE
            """
            rows = await conn.fetch(query, dataset_ids)

            result: dict[str, dict[str, int]] = {}
            for row in rows:
                result[row["dataset_id"]] = {
                    "document_count": row["document_count"] or 0,
                    "segment_count": row["segment_count"] or 0,
                    "available_document_count": row["available_document_count"] or 0,
                    "available_segment_count": row["available_segment_count"] or 0,
                    "word_count": row["word_count"] or 0,
                    "hit_count": row["hit_count"] or 0,
                }

            # Ensure all requested dataset_ids have entries (even if empty)
            for ds_id in dataset_ids:
                if ds_id not in result:
                    result[ds_id] = {
                        "document_count": 0,
                        "segment_count": 0,
                        "available_document_count": 0,
                        "available_segment_count": 0,
                        "word_count": 0,
                        "hit_count": 0,
                    }

            return result

    async def grant_dataset_permission(
        self,
        dataset_id: str,
        subject_type: str,
        subject_id: str,
        permission: str,
    ) -> None:
        """授予 Dataset 权限（subject_type=user|role）"""
        if not self._pool:
            return
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO dataset_permissions (
                    dataset_id, subject_type, subject_id, permission, subject_tenant_id
                ) SELECT dataset_id, $2, $3, $4,
                    CASE WHEN $2::varchar = 'tenant_role' THEN tenant_id ELSE NULL END
                  FROM datasets WHERE dataset_id = $1
                ON CONFLICT (dataset_id, subject_type, subject_id) DO UPDATE SET
                    permission = EXCLUDED.permission,
                    updated_at = NOW()
                """,
                dataset_id,
                "tenant_role" if subject_type == "role" else subject_type,
                subject_id,
                permission,
            )

    async def revoke_dataset_permission(
        self, dataset_id: str, subject_type: str, subject_id: str
    ) -> bool:
        """撤销 Dataset 权限"""
        if not self._pool:
            return False
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM dataset_permissions
                WHERE dataset_id = $1 AND subject_type = $2 AND subject_id = $3
                """,
                dataset_id,
                "tenant_role" if subject_type == "role" else subject_type,
                subject_id,
            )
            if result.startswith("DELETE "):
                return int(result.split()[-1]) > 0
            return False

    async def list_dataset_permissions(self, dataset_id: str) -> list[dict[str, Any]]:
        """列出 Dataset 权限"""
        if not self._pool:
            return []
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT id, dataset_id,
                    CASE WHEN subject_type = 'tenant_role' THEN 'role' ELSE subject_type END AS subject_type,
                    subject_id, permission, created_at, updated_at FROM dataset_permissions
                WHERE dataset_id = $1
                ORDER BY created_at ASC
                """,
                dataset_id,
            )
            return [self._row_to_dict(row) for row in rows]

    async def get_dataset_permissions_batch(
        self, dataset_ids: list[str], *, user_id: str,
        role_dataset_ids: list[str], roles: list[str],
    ) -> dict[str, dict[tuple[str, str], dict[str, Any]]]:
        """Read one bounded catalog page's actor grants in one statement snapshot."""
        if len(dataset_ids) > 200 or len(role_dataset_ids) > 200:
            raise ValueError("dataset permission page exceeds 200 rows")
        if not self._pool or not dataset_ids:
            return {}
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT dataset_id,
                    CASE WHEN subject_type = 'tenant_role' THEN 'role' ELSE subject_type END AS subject_type,
                    subject_id, permission
                FROM dataset_permissions
                WHERE dataset_id = ANY($1::text[])
                  AND ((subject_type = 'user' AND subject_id = $2)
                       OR (subject_type = 'tenant_role' AND dataset_id = ANY($3::text[])
                           AND subject_id = ANY($4::text[])))
                """,
                dataset_ids, user_id, role_dataset_ids, roles,
            )
        result: dict[str, dict[tuple[str, str], dict[str, Any]]] = {}
        for row in rows:
            result.setdefault(row["dataset_id"], {})[(row["subject_type"], row["subject_id"])] = {
                "permission": row["permission"],
            }
        return result

    async def get_dataset_permission(
        self, dataset_id: str, subject_type: str, subject_id: str
    ) -> dict[str, Any] | None:
        """获取指定 subject 对 Dataset 的权限记录"""
        if not self._pool:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT id, dataset_id,
                    CASE WHEN subject_type = 'tenant_role' THEN 'role' ELSE subject_type END AS subject_type,
                    subject_id, permission, created_at, updated_at FROM dataset_permissions
                WHERE dataset_id = $1 AND subject_type = $2 AND subject_id = $3
                """,
                dataset_id,
                "tenant_role" if subject_type == "role" else subject_type,
                subject_id,
            )
            return self._row_to_dict(row) if row else None
