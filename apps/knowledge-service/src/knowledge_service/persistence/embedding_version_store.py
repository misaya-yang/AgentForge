"""Persistence surface for PRD T3 (embedding versioning + blue-green migration).

Owns the tables created by ``database/migrations/102_kb_embedding_versioning_blue_green.sql``:

* ``dataset_collection_bindings`` — the 1:N physical-collection indirection for a
  logical dataset (PRD §6.3 "活动别名"); exactly one ``serving`` row per dataset and
  live-name reservation matching migration 082's semantics on ``datasets``.
* ``embedding_migrations`` — the shadow_build → backfilling → verified → gating →
  ready → completed job ledger (plus the rollback/abandon terminals).
* ``embedding_migration_progress`` — the resumable, content-hash-keyed per-chunk
  receipt ledger. Completeness is decided against the PostgreSQL enabled-chunk
  rows (the authoritative source), never against Qdrant self-witness.
* ``embedding_vector_cache`` — batched content-hash embedding cache (T3 item 4);
  the vector identity is part of the key so a cached vector can never be replayed
  under a different model, and values are float8[] — never pickle.

The store takes an asyncpg pool at construction (the main executor wires it from
``DatabaseStorage``); it deliberately does not touch ``database.py``. Every write
that can race with ingestion or another cutover takes the same dataset advisory
lock used by the index-lease code, so a cutover can never interleave with an
in-flight generation of the same dataset.
"""

from __future__ import annotations

import hashlib
import json
import logging
import math
from collections.abc import AsyncIterator, Sequence
from contextlib import asynccontextmanager
from typing import Any

from .bm25_v2_lifecycle import point_ids_sha256, source_text_sha256

try:
    import asyncpg

    HAS_ASYNCPG = True
except ImportError:  # pragma: no cover - asyncpg is a hard runtime dependency
    HAS_ASYNCPG = False
    asyncpg = None

logger = logging.getLogger("knowledge_service.persistence.embedding_version_store")

BINDING_STATES = frozenset({"shadow", "serving", "retained", "retired"})
MIGRATION_STATES = frozenset(
    {
        "shadow_build",
        "backfilling",
        "verified",
        "gating",
        "gate_failed",
        "ready",
        "completed",
        "rolled_back",
        "failed",
        "abandoned",
    }
)
# A migration is "live" while any of these states holds; the unique partial
# index idx_kb_embedding_migrations_one_live_per_dataset enforces one per dataset.
LIVE_MIGRATION_STATES = ("shadow_build", "backfilling", "verified", "gating", "ready")
# States from which a shadow backfill may (re)start or resume.
BACKFILL_ENTRY_STATES = ("shadow_build", "backfilling", "failed")
# States a migration may be abandoned from: every live state plus the
# operator-actionable non-live ones (failed, gate_failed, and rolled_back
# with a retained shadow — the post-rollback wedge escape). 'completed' is
# deliberately absent: its target binding is serving, and retiring it would
# take the dataset offline (the correct exit there is rollback, not abort).
ABANDONABLE_MIGRATION_STATES = LIVE_MIGRATION_STATES + (
    "failed",
    "gate_failed",
    "rolled_back",
)
# Default retention for a demoted (old) collection after cutover; PRD §6.3:
# the old collection is retained, never deleted by the protocol itself.
DEFAULT_RETENTION_SECONDS = 7 * 24 * 3600
EMBEDDING_AUTHORITY_KIND = "postgres-enabled-text-segments-v1"
AUTHORITY_EVIDENCE_KEYS = (
    "authority_kind",
    "dataset_id",
    "tenant_id",
    "content_revision",
    "point_count",
    "point_ids_sha256",
    "source_text_sha256",
)
SCOPE_EVIDENCE_KEYS = (
    "point_count",
    "point_ids_sha256",
    "source_text_sha256",
)
EMBEDDING_ACTIONS = frozenset({"backfill", "verify", "gate"})
EMBEDDING_ACTION_JOB_STATES = frozenset(
    {"queued", "running", "succeeded", "failed"}
)


class EmbeddingVersionError(RuntimeError):
    """Base error for the T3 binding/migration store."""


class BindingConflictError(EmbeddingVersionError):
    """A collection name or serving slot is already reserved."""


class MigrationStateError(EmbeddingVersionError):
    """The migration is not in a state that permits the requested transition."""


def _loads_json(value: Any, fallback: Any) -> Any:
    if value is None:
        return fallback
    if isinstance(value, (dict, list)):
        return value
    try:
        return json.loads(str(value))
    except (TypeError, ValueError):
        return fallback


def evidence_matches(
    left: Any,
    right: Any,
    *,
    keys: Sequence[str],
) -> bool:
    """Compare a persisted evidence projection without accepting omissions."""

    if not isinstance(left, dict) or not isinstance(right, dict):
        return False
    return all(key in left and key in right and left[key] == right[key] for key in keys)


def authority_matches_scope(authority: Any, scope: Any) -> bool:
    return evidence_matches(authority, scope, keys=SCOPE_EVIDENCE_KEYS)


def action_request_hash(action: str, payload: dict[str, Any]) -> str:
    canonical = json.dumps(
        payload,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(f"{action}\0{canonical}".encode()).hexdigest()


def _normalize_action_payload(action: str, payload: dict[str, Any]) -> dict[str, Any]:
    if action != "gate":
        if payload:
            raise ValueError(f"'{action}' action does not accept parameters")
        return {}

    allowed = {"sample_size", "top_k", "tolerance", "floor"}
    unknown = sorted(set(payload) - allowed)
    if unknown:
        raise ValueError(f"unsupported gate parameters: {', '.join(unknown)}")
    normalized = {key: value for key, value in payload.items() if value is not None}
    for key, upper in (("sample_size", 256), ("top_k", 50)):
        if key not in normalized:
            continue
        value = normalized[key]
        if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= upper:
            raise ValueError(f"gate {key} must be an integer between 1 and {upper}")
    for key in ("tolerance", "floor"):
        if key not in normalized:
            continue
        value = normalized[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError(f"gate {key} must be a number between 0 and 1")
        value = float(value)
        if not math.isfinite(value) or not 0.0 <= value <= 1.0:
            raise ValueError(f"gate {key} must be a number between 0 and 1")
        normalized[key] = value
    return normalized


def _identity_fields(
    provider: str,
    model: str,
    model_version: str,
    dimension: int,
) -> tuple[str, str, str, int]:
    dim = int(dimension or 0)
    if dim <= 0:
        raise ValueError("embedding_dimension must be a positive integer")
    return (
        str(provider or "").strip(),
        str(model or "").strip(),
        str(model_version or "").strip(),
        dim,
    )


class EmbeddingVersionStore:
    """Pool-backed store for bindings, migrations, and the vector cache."""

    def __init__(self, pool: Any) -> None:
        if not HAS_ASYNCPG or pool is None:
            raise ValueError("EmbeddingVersionStore requires an asyncpg pool")
        self._pool = pool

    # ------------------------------------------------------------------ locks

    @staticmethod
    def _dataset_lock_name(dataset_id: str) -> str:
        # Same key as DatasetPersistenceMixin._dataset_index_lock_name so that
        # binding flips serialize with index leases and deletion fences.
        normalized = str(dataset_id or "").strip()
        if not normalized:
            raise ValueError("dataset_id is required for binding locks")
        return f"knowledge-dataset-index:{normalized}"

    async def _acquire_dataset_lock(self, conn: Any, dataset_id: str) -> None:
        await conn.fetchval(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            self._dataset_lock_name(dataset_id),
        )

    @asynccontextmanager
    async def dataset_exclusive_lease(self, dataset_id: str) -> AsyncIterator[Any]:
        """Hold the dataset writer barrier across an external-store check.

        Embedding cutover must inspect Qdrant and then flip PostgreSQL without
        an ingestion/reprocess writer publishing between those operations.
        The ordinary transaction-scoped advisory lock cannot span that
        external call, so cutover uses this session-scoped lease and passes
        the yielded connection back into :meth:`cutover_migration`.

        The key is identical to the shared writer-lease key. A competing
        cutover fails immediately with a caller-visible 409 instead of
        waiting while an operator request holds an unknown deadline.
        """

        normalized_dataset = str(dataset_id or "").strip()
        lock_name = self._dataset_lock_name(normalized_dataset)
        async with self._pool.acquire() as conn:
            acquired = bool(
                await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    lock_name,
                )
            )
            if not acquired:
                raise MigrationStateError(
                    "dataset index generation is busy; retry embedding cutover"
                )
            try:
                yield conn
            finally:
                released = bool(
                    await conn.fetchval(
                        "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                        lock_name,
                    )
                )
                if not released:  # pragma: no cover - PostgreSQL invariant
                    logger.error(
                        "embedding cutover dataset lock was not held on release",
                        extra={"dataset_id": normalized_dataset},
                    )

    @staticmethod
    def _backfill_lock_name(migration_id: str) -> str:
        normalized = str(migration_id or "").strip()
        if not normalized:
            raise ValueError("migration_id is required for backfill locks")
        return f"knowledge-embedding-backfill:{normalized}"

    @asynccontextmanager
    async def backfill_lease(self, migration_id: str) -> AsyncIterator[None]:
        """Hold the single cross-process claim for one paid backfill run.

        The migration state remains ``backfilling`` after a successful pass so
        it can be verified or resumed. A state-only CAS therefore cannot tell a
        legitimate resume from a concurrent duplicate. This session advisory
        lock spans the external embedding and Qdrant calls without holding a
        database transaction open; a competing request fails immediately.
        """

        lock_name = self._backfill_lock_name(migration_id)
        async with self._pool.acquire() as conn:
            acquired = bool(
                await conn.fetchval(
                    "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                    lock_name,
                )
            )
            if not acquired:
                raise MigrationStateError(
                    "embedding migration backfill is already running"
                )
            try:
                yield
            finally:
                released = bool(
                    await conn.fetchval(
                        "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                        lock_name,
                    )
                )
                if not released:  # pragma: no cover - PostgreSQL invariant
                    logger.error(
                        "embedding backfill advisory lock was not held on release",
                        extra={"migration_id": str(migration_id)},
                    )

    # -------------------------------------------------------------- bindings

    @staticmethod
    def _binding_to_dict(row: Any) -> dict[str, Any]:
        return {
            "binding_id": str(row["binding_id"]),
            "dataset_id": str(row["dataset_id"]),
            "tenant_id": str(row["tenant_id"] or ""),
            "collection_name": str(row["collection_name"] or ""),
            "embedding_provider": str(row["embedding_provider"] or ""),
            "embedding_model": str(row["embedding_model"] or ""),
            "embedding_model_version": str(row["embedding_model_version"] or ""),
            "embedding_dimension": int(row["embedding_dimension"] or 0),
            "capabilities": _loads_json(row["capabilities"], []),
            "state": str(row["state"]),
            "created_at": row["created_at"],
            "activated_at": row["activated_at"],
            "retired_at": row["retired_at"],
            "retained_until": row["retained_until"],
        }

    async def get_serving_binding(self, dataset_id: str) -> dict[str, Any] | None:
        if not self._pool:
            raise RuntimeError("embedding version store is not connected")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings"
                " WHERE dataset_id = $1 AND state = 'serving'",
                str(dataset_id or "").strip(),
            )
        return self._binding_to_dict(row) if row else None

    async def get_binding(self, binding_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(binding_id or "").strip(),
            )
        return self._binding_to_dict(row) if row else None

    async def get_binding_by_collection_name(
        self, collection_name: str
    ) -> dict[str, Any] | None:
        """The live (non-retired) binding reserving ``collection_name``, if any.

        Retired rows have released their claim, so they never count as a
        reservation; the newest remaining row wins.
        """
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings"
                " WHERE collection_name = $1 AND state <> 'retired'"
                " ORDER BY created_at DESC LIMIT 1",
                str(collection_name or "").strip(),
            )
        return self._binding_to_dict(row) if row else None

    async def list_bindings(self, dataset_id: str) -> list[dict[str, Any]]:
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT * FROM dataset_collection_bindings"
                " WHERE dataset_id = $1 ORDER BY created_at",
                str(dataset_id or "").strip(),
            )
        return [self._binding_to_dict(row) for row in rows]

    async def create_binding(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        collection_name: str,
        embedding_provider: str,
        embedding_model: str,
        embedding_dimension: int,
        embedding_model_version: str = "",
        capabilities: Sequence[str] | None = None,
        state: str = "shadow",
    ) -> dict[str, Any]:
        """Register one physical collection generation for a dataset.

        ``shadow`` is the only state creatable here: 'serving' is granted
        exclusively by a cutover (or the legacy seed), preserving the
        one-serving-per-dataset invariant at the storage layer.
        """
        if state not in {"shadow"}:
            raise ValueError("create_binding only accepts state='shadow'")
        normalized_dataset = str(dataset_id or "").strip()
        provider, model, version, dim = _identity_fields(
            embedding_provider, embedding_model, embedding_model_version, embedding_dimension
        )
        name = str(collection_name or "").strip()
        if not name:
            raise ValueError("collection_name is required")
        async with self._pool.acquire() as conn, conn.transaction():
            await self._acquire_dataset_lock(conn, normalized_dataset)
            try:
                row = await conn.fetchrow(
                    """
                    INSERT INTO dataset_collection_bindings (
                        dataset_id, tenant_id, collection_name,
                        embedding_provider, embedding_model, embedding_model_version,
                        embedding_dimension, capabilities, state
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, 'shadow')
                    RETURNING *
                    """,
                    normalized_dataset,
                    str(tenant_id or ""),
                    name,
                    provider,
                    model,
                    version,
                    dim,
                    json.dumps(sorted(set(capabilities or ())), ensure_ascii=False),
                )
            except asyncpg.UniqueViolationError as exc:
                raise BindingConflictError(
                    f"collection '{name}' is already reserved by a live binding"
                ) from exc
        return self._binding_to_dict(row)

    async def register_serving_binding_from_dataset_row(
        self, dataset: dict[str, Any]
    ) -> dict[str, Any] | None:
        """Seed a 'serving' binding from a legacy datasets row (idempotent).

        Datasets created before migration 102 (or by paths that have not been
        wired into the binding layer yet) are adopted lazily: the binding
        mirrors the datasets row, which remains the single source of truth
        until a cutover flips both together.
        """
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        name = str(dataset.get("collection_name") or "").strip()
        dim = int(dataset.get("embedding_dimension") or 0)
        if not dataset_id or not name or dim <= 0:
            raise ValueError(
                "dataset row needs collection_name and a positive embedding_dimension "
                "to register a serving binding"
            )
        async with self._pool.acquire() as conn, conn.transaction():
            await self._acquire_dataset_lock(conn, dataset_id)
            try:
                await conn.execute(
                    """
                    INSERT INTO dataset_collection_bindings (
                        dataset_id, tenant_id, collection_name,
                        embedding_provider, embedding_model, embedding_model_version,
                        embedding_dimension, state, activated_at
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, 'serving', NOW())
                    ON CONFLICT (dataset_id) WHERE state = 'serving' DO NOTHING
                    """,
                    dataset_id,
                    str(dataset.get("tenant_id") or ""),
                    name,
                    str(dataset.get("embedding_provider") or ""),
                    str(dataset.get("embedding_model") or ""),
                    str(dataset.get("embedding_model_version") or ""),
                    dim,
                )
            except asyncpg.UniqueViolationError as exc:
                # The live-collection-name reservation collides with another
                # dataset's binding: never steal it.
                raise BindingConflictError(
                    f"collection '{name}' is already reserved by another live binding"
                ) from exc
            row = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings"
                " WHERE dataset_id = $1 AND state = 'serving'",
                dataset_id,
            )
        return self._binding_to_dict(row) if row else None

    # ------------------------------------------------------------ migrations

    @staticmethod
    def _migration_to_dict(row: Any) -> dict[str, Any]:
        return {
            "migration_id": str(row["migration_id"]),
            "dataset_id": str(row["dataset_id"]),
            "source_binding_id": str(row["source_binding_id"])
            if row["source_binding_id"]
            else None,
            "target_binding_id": str(row["target_binding_id"]),
            "state": str(row["state"]),
            "checkpoint": _loads_json(row["checkpoint"], {}),
            "totals": _loads_json(row["totals"], {}),
            "gate": _loads_json(row["gate"], None),
            "error": row["error"],
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
        }

    @staticmethod
    def _action_job_to_dict(row: Any) -> dict[str, Any]:
        result = {
            "job_id": str(row["job_id"]),
            "migration_id": str(row["migration_id"]),
            "dataset_id": str(row["dataset_id"]),
            "action": str(row["action"]),
            "request_hash": str(row["request_hash"]),
            "state": str(row["state"]),
            "payload": _loads_json(row["payload"], {}),
            "result": _loads_json(row["result"], None),
            "error": str(row["error"]) if row["error"] else None,
            "requested_by": str(row["requested_by"] or ""),
            "attempt_count": int(row["attempt_count"] or 0),
            "claimed_by": str(row["claimed_by"] or ""),
            "claim_token": str(row["claim_token"]) if row["claim_token"] else None,
            "terminal_claim_token": str(row["terminal_claim_token"])
            if row["terminal_claim_token"]
            else None,
            "available_at": row["available_at"],
            "lease_expires_at": row["lease_expires_at"],
            "last_heartbeat_at": row["last_heartbeat_at"],
            "started_at": row["started_at"],
            "finished_at": row["finished_at"],
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
        }
        if "prior_state" in row:
            result["recovered_from_running"] = str(row["prior_state"]) == "running"
        return result

    async def begin_migration(
        self,
        *,
        dataset_id: str,
        source_binding_id: str | None,
        target_binding_id: str,
    ) -> dict[str, Any]:
        normalized_dataset = str(dataset_id or "").strip()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._acquire_dataset_lock(conn, normalized_dataset)
            try:
                row = await conn.fetchrow(
                    """
                    INSERT INTO embedding_migrations (
                        dataset_id, source_binding_id, target_binding_id, state
                    )
                    VALUES ($1, $2::uuid, $3::uuid, 'shadow_build')
                    RETURNING *
                    """,
                    normalized_dataset,
                    str(source_binding_id).strip() if source_binding_id else None,
                    str(target_binding_id).strip(),
                )
            except asyncpg.UniqueViolationError as exc:
                raise MigrationStateError(
                    f"dataset {normalized_dataset} already has a live embedding migration"
                ) from exc
        return self._migration_to_dict(row)

    async def get_migration(self, migration_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM embedding_migrations WHERE migration_id = $1::uuid",
                str(migration_id or "").strip(),
            )
        return self._migration_to_dict(row) if row else None

    async def get_live_migration(self, dataset_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE dataset_id = $1 AND state = ANY($2::text[])"
                " ORDER BY created_at DESC LIMIT 1",
                str(dataset_id or "").strip(),
                list(LIVE_MIGRATION_STATES),
            )
        return self._migration_to_dict(row) if row else None

    async def get_migration_by_target_binding(
        self, binding_id: str
    ) -> dict[str, Any] | None:
        """The most recent migration opened against ``binding_id`` as target."""
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE target_binding_id = $1::uuid"
                " ORDER BY created_at DESC LIMIT 1",
                str(binding_id or "").strip(),
            )
        return self._migration_to_dict(row) if row else None

    async def get_recoverable_migration(self, dataset_id: str) -> dict[str, Any] | None:
        """The most recent ``failed``/``gate_failed`` migration for a dataset.

        These states are operator-actionable (backfill resume, re-gate, abort)
        but deliberately stay OUT of ``LIVE_MIGRATION_STATES`` so they never
        block the one-live-per-dataset index. Surfacing them is what keeps a
        failed attempt discoverable through the API instead of wedging the
        dataset behind an invisible shadow reservation.
        """
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE dataset_id = $1 AND state = ANY($2::text[])"
                " ORDER BY created_at DESC LIMIT 1",
                str(dataset_id or "").strip(),
                ["failed", "gate_failed"],
            )
        return self._migration_to_dict(row) if row else None

    async def get_latest_migration(self, dataset_id: str) -> dict[str, Any] | None:
        """Newest migration regardless of state.

        Completed and rolled-back jobs are intentionally included: the
        management UI needs their migration id after a refresh in order to
        offer rollback/retry rather than losing the control-plane handle.
        """

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE dataset_id = $1"
                " ORDER BY created_at DESC, migration_id DESC LIMIT 1",
                str(dataset_id or "").strip(),
            )
        return self._migration_to_dict(row) if row else None

    async def list_recent_migrations(
        self, dataset_id: str, *, limit: int = 5
    ) -> list[dict[str, Any]]:
        """Bounded newest-first history for the embedding management plane."""

        bounded_limit = min(max(int(limit), 1), 20)
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT * FROM embedding_migrations"
                " WHERE dataset_id = $1"
                " ORDER BY created_at DESC, migration_id DESC LIMIT $2",
                str(dataset_id or "").strip(),
                bounded_limit,
            )
        return [self._migration_to_dict(row) for row in rows]

    # ------------------------------------------------------ durable actions

    async def require_action_job_store(self) -> None:
        async with self._pool.acquire() as conn:
            relation = await conn.fetchval(
                "SELECT to_regclass('knowledge.embedding_migration_action_jobs')::text"
            )
        if not relation:
            raise RuntimeError(
                "embedding migration action jobs require database migration 110"
            )

    @staticmethod
    def _action_can_enqueue(action: str, migration_state: str) -> bool:
        allowed_states = {
            "backfill": set(BACKFILL_ENTRY_STATES),
            "verify": {"backfilling", "verified", "failed"},
            "gate": {"verified", "gate_failed", "gating"},
        }
        return migration_state in allowed_states[action]

    @staticmethod
    async def _authority_revision_is_current(
        conn: Any,
        dataset_id: str,
        evidence: Any,
    ) -> bool:
        if not isinstance(evidence, dict) or "content_revision" not in evidence:
            return False
        current = await conn.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = $1",
            dataset_id,
        )
        try:
            return current is not None and int(current) == int(evidence["content_revision"])
        except (TypeError, ValueError):
            return False

    async def enqueue_action_job(
        self,
        migration_id: str,
        *,
        action: str,
        payload: dict[str, Any] | None = None,
        requested_by: str = "",
    ) -> tuple[dict[str, Any], bool]:
        """Atomically enqueue or reuse one durable migration action.

        The migration row serializes normal concurrent producers. At most one
        queued or running action of any kind may exist for a dataset: a
        same-action replay returns its job id; a different action/migration is
        a deterministic 409 (the partial unique index closes cross-row races).
        Failed actions are re-queued in place so their job id remains stable.
        """

        normalized_migration = str(migration_id or "").strip()
        normalized_action = str(action or "").strip().lower()
        if normalized_action not in EMBEDDING_ACTIONS:
            raise ValueError(f"unsupported embedding action '{normalized_action}'")
        normalized_payload = _normalize_action_payload(
            normalized_action, dict(payload or {})
        )
        request_hash = action_request_hash(normalized_action, normalized_payload)
        async with self._pool.acquire() as conn, conn.transaction():
            migration = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE migration_id = $1::uuid FOR UPDATE",
                normalized_migration,
            )
            if migration is None:
                raise MigrationStateError("migration not found")
            dataset_id = str(migration["dataset_id"])
            migration_state = str(migration["state"])

            active = await conn.fetchrow(
                """
                SELECT * FROM knowledge.embedding_migration_action_jobs
                WHERE dataset_id = $1 AND state IN ('queued', 'running')
                ORDER BY created_at DESC
                LIMIT 1
                FOR UPDATE
                """,
                dataset_id,
            )
            if active is not None:
                if str(active["migration_id"]) != normalized_migration:
                    raise MigrationStateError(
                        "dataset already has an active migration action job"
                    )
                if str(active["action"]) != normalized_action:
                    raise MigrationStateError(
                        f"migration already has active '{active['action']}' job"
                    )
                if str(active["request_hash"]) != request_hash or _loads_json(
                    active["payload"], {}
                ) != normalized_payload:
                    raise MigrationStateError(
                        f"active '{normalized_action}' job has different parameters"
                    )
                return self._action_job_to_dict(active), True

            latest = await conn.fetchrow(
                """
                SELECT * FROM knowledge.embedding_migration_action_jobs
                WHERE migration_id = $1::uuid AND action = $2
                ORDER BY created_at DESC
                LIMIT 1
                FOR UPDATE
                """,
                normalized_migration,
                normalized_action,
            )

            # A completed action remains idempotent while its state-machine
            # postcondition still holds. Backfill is special: new corpus rows
            # make another pass legitimate while the migration is backfilling.
            if latest is not None and str(latest["state"]) == "succeeded":
                satisfied = False
                if normalized_action == "backfill":
                    pending = await self._count_pending_segments_on_connection(
                        conn,
                        normalized_migration,
                        dataset_id=dataset_id,
                    )
                    satisfied = pending == 0 and migration_state in {
                        "backfilling",
                        "verified",
                        "gating",
                        "gate_failed",
                        "ready",
                        "completed",
                    }
                elif normalized_action == "verify":
                    totals = _loads_json(migration["totals"], {})
                    pending = await self._count_pending_segments_on_connection(
                        conn,
                        normalized_migration,
                        dataset_id=dataset_id,
                    )
                    satisfied = (
                        pending == 0
                        and migration_state
                        in {
                            "verified",
                            "gating",
                            "gate_failed",
                            "ready",
                            "completed",
                        }
                        and await self._authority_revision_is_current(
                            conn,
                            dataset_id,
                            totals.get("verified_authority"),
                        )
                    )
                else:
                    gate = _loads_json(migration["gate"], {})
                    pending = await self._count_pending_segments_on_connection(
                        conn,
                        normalized_migration,
                        dataset_id=dataset_id,
                    )
                    satisfied = (
                        pending == 0
                        and migration_state in {"ready", "completed"}
                        and await self._authority_revision_is_current(
                            conn,
                            dataset_id,
                            gate.get("authority_snapshot"),
                        )
                    )
                if satisfied:
                    if str(latest["request_hash"]) != request_hash or _loads_json(
                        latest["payload"], {}
                    ) != normalized_payload:
                        raise MigrationStateError(
                            f"completed '{normalized_action}' job has different "
                            "parameters"
                        )
                    return self._action_job_to_dict(latest), True

            if not self._action_can_enqueue(normalized_action, migration_state):
                raise MigrationStateError(
                    f"migration in state '{migration_state}' cannot enqueue "
                    f"'{normalized_action}'"
                )
            if normalized_action == "verify":
                pending = await self._count_pending_segments_on_connection(
                    conn,
                    normalized_migration,
                    dataset_id=dataset_id,
                )
                if pending:
                    raise MigrationStateError(
                        f"cannot enqueue verify while {pending} chunks remain pending"
                    )

            if (
                latest is not None
                and str(latest["state"]) == "failed"
                and str(latest["request_hash"]) == request_hash
                and _loads_json(latest["payload"], {}) == normalized_payload
            ):
                row = await conn.fetchrow(
                    """
                    UPDATE knowledge.embedding_migration_action_jobs
                    SET state = 'queued', payload = $2::jsonb,
                        requested_by = NULLIF($3, ''), result = NULL, error = NULL,
                        claimed_by = NULL, claim_token = NULL,
                        request_hash = $4, terminal_claim_token = NULL,
                        available_at = NOW(), lease_expires_at = NULL,
                        last_heartbeat_at = NULL, started_at = NULL,
                        finished_at = NULL, updated_at = NOW()
                    WHERE job_id = $1::uuid AND state = 'failed'
                    RETURNING *
                    """,
                    str(latest["job_id"]),
                    json.dumps(normalized_payload, ensure_ascii=False),
                    str(requested_by or "").strip(),
                    request_hash,
                )
                if row is None:  # pragma: no cover - migration row serializes producers
                    raise MigrationStateError("failed action job changed during retry")
                return self._action_job_to_dict(row), True

            try:
                row = await conn.fetchrow(
                    """
                    INSERT INTO knowledge.embedding_migration_action_jobs (
                        migration_id, dataset_id, action, payload,
                        request_hash, requested_by
                    )
                    VALUES ($1::uuid, $2, $3, $4::jsonb, $5, NULLIF($6, ''))
                    RETURNING *
                    """,
                    normalized_migration,
                    dataset_id,
                    normalized_action,
                    json.dumps(normalized_payload, ensure_ascii=False),
                    request_hash,
                    str(requested_by or "").strip(),
                )
            except asyncpg.UniqueViolationError as exc:  # pragma: no cover - defensive
                raise MigrationStateError(
                    "migration action was concurrently enqueued; retry the request"
                ) from exc
        return self._action_job_to_dict(row), False

    async def get_action_job(self, job_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM knowledge.embedding_migration_action_jobs"
                " WHERE job_id = $1::uuid",
                str(job_id or "").strip(),
            )
        return self._action_job_to_dict(row) if row else None

    async def get_scoped_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        """Fetch a job only inside its already-authorized object scope."""

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM knowledge.embedding_migration_action_jobs"
                " WHERE job_id = $1::uuid AND migration_id = $2::uuid"
                " AND dataset_id = $3",
                str(job_id or "").strip(),
                str(migration_id or "").strip(),
                str(dataset_id or "").strip(),
            )
        return self._action_job_to_dict(row) if row else None

    async def cancel_scoped_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        """Cancel a scoped queued/running action and fence its claim token.

        The existing schema uses ``failed`` as its durable cancellation
        terminal. Clearing the live claim makes the worker heartbeat fail and
        prevents a late result CAS from overwriting that terminal receipt.
        """

        async with self._pool.acquire() as conn, conn.transaction():
            current = await conn.fetchrow(
                "SELECT * FROM knowledge.embedding_migration_action_jobs"
                " WHERE job_id = $1::uuid AND migration_id = $2::uuid"
                " AND dataset_id = $3 FOR UPDATE",
                str(job_id or "").strip(),
                str(migration_id or "").strip(),
                str(dataset_id or "").strip(),
            )
            if current is None:
                return None
            state = str(current["state"])
            error = str(current["error"] or "")
            if state == "failed" and error == "cancelled by operator":
                return self._action_job_to_dict(current)
            if state not in {"queued", "running"}:
                raise MigrationStateError(
                    f"migration action job in state '{state}' cannot be cancelled"
                )
            row = await conn.fetchrow(
                """
                UPDATE knowledge.embedding_migration_action_jobs
                SET state = 'failed', result = NULL,
                    error = 'cancelled by operator',
                    claimed_by = NULL, claim_token = NULL,
                    terminal_claim_token = CASE
                        WHEN state = 'running' THEN claim_token
                        ELSE terminal_claim_token
                    END,
                    lease_expires_at = NULL,
                    last_heartbeat_at = CASE
                        WHEN state = 'running' THEN NOW()
                        ELSE last_heartbeat_at
                    END,
                    finished_at = NOW(), updated_at = NOW()
                WHERE job_id = $1::uuid AND state IN ('queued', 'running')
                RETURNING *
                """,
                str(job_id or "").strip(),
            )
        return self._action_job_to_dict(row) if row else None

    async def describe_action_jobs(
        self, dataset_id: str, *, terminal_limit: int = 10
    ) -> tuple[dict[str, Any] | None, list[dict[str, Any]]]:
        """Return one dataset-scoped active/recent-terminal refresh snapshot."""

        bounded_limit = min(max(int(terminal_limit), 1), 50)
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM knowledge.embedding_migration_action_jobs
                WHERE dataset_id = $1
                ORDER BY
                    CASE WHEN state IN ('queued', 'running') THEN 0 ELSE 1 END,
                    COALESCE(finished_at, updated_at) DESC,
                    created_at DESC,
                    job_id DESC
                LIMIT $2
                """,
                str(dataset_id or "").strip(),
                bounded_limit + 1,
            )
        jobs = [self._action_job_to_dict(row) for row in rows]
        active = next(
            (job for job in jobs if job["state"] in {"queued", "running"}),
            None,
        )
        terminal = [
            job for job in jobs if job["state"] in {"succeeded", "failed"}
        ][:bounded_limit]
        return active, terminal

    async def list_recent_action_jobs(
        self, migration_id: str, *, limit: int = 10
    ) -> list[dict[str, Any]]:
        bounded_limit = min(max(int(limit), 1), 50)
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT * FROM knowledge.embedding_migration_action_jobs"
                " WHERE migration_id = $1::uuid"
                " ORDER BY created_at DESC, job_id DESC LIMIT $2",
                str(migration_id or "").strip(),
                bounded_limit,
            )
        return [self._action_job_to_dict(row) for row in rows]

    async def claim_next_action_job(
        self,
        *,
        worker_id: str,
        lease_seconds: int,
    ) -> dict[str, Any] | None:
        """Claim one queued or lease-expired job with a fresh token."""

        normalized_worker = str(worker_id or "").strip()
        if not normalized_worker:
            raise ValueError("worker_id is required")
        lease_seconds = max(int(lease_seconds), 1)
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                WITH candidate AS (
                    SELECT job_id, state AS prior_state
                    FROM knowledge.embedding_migration_action_jobs
                    WHERE (
                        state = 'queued' AND available_at <= NOW()
                    ) OR (
                        state = 'running' AND lease_expires_at < NOW()
                    )
                    ORDER BY
                        CASE
                            WHEN state = 'running' THEN lease_expires_at
                            ELSE available_at
                        END,
                        created_at
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                )
                UPDATE knowledge.embedding_migration_action_jobs AS job
                SET state = 'running', claimed_by = $1,
                    claim_token = gen_random_uuid(),
                    terminal_claim_token = NULL,
                    lease_expires_at = NOW() + make_interval(secs => $2),
                    last_heartbeat_at = NOW(),
                    started_at = COALESCE(started_at, NOW()),
                    finished_at = NULL, error = NULL,
                    attempt_count = attempt_count + 1,
                    updated_at = NOW()
                FROM candidate
                WHERE job.job_id = candidate.job_id
                RETURNING job.*, candidate.prior_state
                """,
                normalized_worker,
                lease_seconds,
            )
        return self._action_job_to_dict(row) if row else None

    async def heartbeat_action_job(
        self,
        job_id: str,
        *,
        claim_token: str,
        lease_seconds: int,
    ) -> bool:
        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                """
                UPDATE knowledge.embedding_migration_action_jobs
                SET lease_expires_at = NOW() + make_interval(secs => $3),
                    last_heartbeat_at = NOW(), updated_at = NOW()
                WHERE job_id = $1::uuid AND state = 'running'
                  AND claim_token = $2::uuid
                RETURNING TRUE
                """,
                str(job_id or "").strip(),
                str(claim_token or "").strip(),
                max(int(lease_seconds), 1),
            )
        return bool(value)

    async def finish_action_job(
        self,
        job_id: str,
        *,
        claim_token: str,
        result: dict[str, Any],
    ) -> dict[str, Any] | None:
        normalized_job = str(job_id or "").strip()
        normalized_token = str(claim_token or "").strip()
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                UPDATE knowledge.embedding_migration_action_jobs
                SET state = 'succeeded', result = $3::jsonb, error = NULL,
                    claimed_by = NULL, claim_token = NULL,
                    terminal_claim_token = $2::uuid,
                    lease_expires_at = NULL, last_heartbeat_at = NOW(),
                    finished_at = NOW(), updated_at = NOW()
                WHERE job_id = $1::uuid AND state = 'running'
                  AND claim_token = $2::uuid
                RETURNING *
                """,
                normalized_job,
                normalized_token,
                json.dumps(result, ensure_ascii=False, default=str),
            )
            if row is None:
                row = await conn.fetchrow(
                    """
                    SELECT * FROM knowledge.embedding_migration_action_jobs
                    WHERE job_id = $1::uuid AND state = 'succeeded'
                      AND terminal_claim_token = $2::uuid
                    """,
                    normalized_job,
                    normalized_token,
                )
        return self._action_job_to_dict(row) if row else None

    async def fail_action_job(
        self,
        job_id: str,
        *,
        claim_token: str,
        error: str,
    ) -> dict[str, Any] | None:
        normalized_job = str(job_id or "").strip()
        normalized_token = str(claim_token or "").strip()
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                UPDATE knowledge.embedding_migration_action_jobs
                SET state = 'failed', result = NULL, error = $3,
                    claimed_by = NULL, claim_token = NULL,
                    terminal_claim_token = $2::uuid,
                    lease_expires_at = NULL, last_heartbeat_at = NOW(),
                    finished_at = NOW(), updated_at = NOW()
                WHERE job_id = $1::uuid AND state = 'running'
                  AND claim_token = $2::uuid
                RETURNING *
                """,
                normalized_job,
                normalized_token,
                str(error or "")[:4000],
            )
            if row is None:
                row = await conn.fetchrow(
                    """
                    SELECT * FROM knowledge.embedding_migration_action_jobs
                    WHERE job_id = $1::uuid AND state = 'failed'
                      AND terminal_claim_token = $2::uuid
                    """,
                    normalized_job,
                    normalized_token,
                )
        return self._action_job_to_dict(row) if row else None

    async def requeue_action_job(
        self,
        job_id: str,
        *,
        claim_token: str,
    ) -> bool:
        """Return an interrupted owned job to the durable queue."""

        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                """
                UPDATE knowledge.embedding_migration_action_jobs
                SET state = 'queued', claimed_by = NULL, claim_token = NULL,
                    terminal_claim_token = NULL,
                    available_at = NOW(), lease_expires_at = NULL,
                    last_heartbeat_at = NULL, updated_at = NOW()
                WHERE job_id = $1::uuid AND state = 'running'
                  AND claim_token = $2::uuid
                RETURNING TRUE
                """,
                str(job_id or "").strip(),
                str(claim_token or "").strip(),
            )
        return bool(value)

    async def transition_migration(
        self,
        migration_id: str,
        *,
        to_state: str,
        from_states: Sequence[str],
        error: str | None = None,
    ) -> dict[str, Any] | None:
        """CAS a migration into ``to_state``; returns the new row or None.

        Moving INTO a live state can collide with the one-live-per-dataset
        index when another migration went live between the caller's check and
        this UPDATE; that race is a caller-visible conflict, not a server
        error, so it is mapped to MigrationStateError (409) here.
        """
        if to_state not in MIGRATION_STATES:
            raise ValueError(f"unknown migration state '{to_state}'")
        async with self._pool.acquire() as conn:
            try:
                row = await conn.fetchrow(
                    """
                    UPDATE embedding_migrations
                    SET state = $2, error = $3, updated_at = NOW()
                    WHERE migration_id = $1::uuid AND state = ANY($4::text[])
                    RETURNING *
                    """,
                    str(migration_id or "").strip(),
                    to_state,
                    str(error).strip() if error else None,
                    list(from_states),
                )
            except asyncpg.UniqueViolationError as exc:
                raise MigrationStateError(
                    f"cannot move migration into '{to_state}': dataset already "
                    "has a live embedding migration"
                ) from exc
        return self._migration_to_dict(row) if row else None

    async def merge_migration_progress(
        self,
        migration_id: str,
        *,
        checkpoint: dict[str, Any] | None = None,
        totals: dict[str, Any] | None = None,
    ) -> None:
        """JSONB-merge checkpoint/totals for resumability bookkeeping."""
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE embedding_migrations
                SET checkpoint = checkpoint || COALESCE($2::jsonb, '{}'::jsonb),
                    totals = totals || COALESCE($3::jsonb, '{}'::jsonb),
                    updated_at = NOW()
                WHERE migration_id = $1::uuid
                """,
                str(migration_id or "").strip(),
                json.dumps(checkpoint, ensure_ascii=False) if checkpoint is not None else None,
                json.dumps(totals, ensure_ascii=False) if totals is not None else None,
            )

    async def record_gate_verdict(
        self, migration_id: str, *, verdict: dict[str, Any], passed: bool
    ) -> dict[str, Any] | None:
        """Store the T0 evaluation verdict and move gating → ready|gate_failed."""
        if not isinstance(verdict, dict):
            raise ValueError("gate verdict must be a dict")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                UPDATE embedding_migrations
                SET gate = $2::jsonb,
                    state = CASE WHEN $3::boolean THEN 'ready' ELSE 'gate_failed' END,
                    updated_at = NOW()
                WHERE migration_id = $1::uuid AND state = 'gating'
                RETURNING *
                """,
                str(migration_id or "").strip(),
                json.dumps(verdict, ensure_ascii=False),
                bool(passed),
            )
        return self._migration_to_dict(row) if row else None

    # ---------------------------------------------------- progress ledger

    def _pending_sql(self) -> str:
        return """
                FROM segments s
                JOIN documents d ON d.document_id = s.document_id
                LEFT JOIN embedding_migration_progress p
                    ON p.migration_id = $1::uuid AND p.segment_id = s.segment_id
                WHERE s.dataset_id = $2
                  AND s.enabled = TRUE
                  AND d.enabled = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND COALESCE(s.content_type, 'text') = 'text'
                  AND (
                      p.segment_id IS NULL
                      OR p.content_hash IS DISTINCT FROM
                         COALESCE(s.content_hash, s.index_node_hash, '')
                  )
                """

    async def list_pending_segments(
        self,
        migration_id: str,
        *,
        dataset_id: str,
        limit: int = 200,
    ) -> list[dict[str, Any]]:
        """Authoritative backfill queue: enabled PG chunk rows without a
        matching content-hash receipt in the migration ledger.

        Hierarchical parent chunks embed their stored summary text, so the
        same enumeration covers summary vectors — the blue-green contract is
        "re-embed persisted text, never regenerate content" (addendum §1-T3.1).
        Image segments are owned by their own receipt path and gated on the
        target model's vision capability, so they are excluded here.
        """
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                SELECT
                    s.segment_id,
                    s.document_id,
                    s.position,
                    s.text,
                    s.token_count,
                    COALESCE(NULLIF(s.vector_id, ''), s.segment_id) AS vector_id,
                    COALESCE(s.content_hash, s.index_node_hash, '') AS content_hash,
                    s.metadata
                {self._pending_sql()}
                ORDER BY s.document_id, s.position
                LIMIT $3
                """,
                str(migration_id or "").strip(),
                str(dataset_id or "").strip(),
                int(limit),
            )
        return [dict(row) for row in rows]

    async def count_pending_segments(
        self, migration_id: str, *, dataset_id: str
    ) -> int:
        """Completeness measure: enabled chunks still lacking a receipt.

        Zero means the target collection has provably received a vector for
        every authoritative chunk row (PRD §6.3: Qdrant cannot self-witness).
        """
        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                f"SELECT COUNT(*) {self._pending_sql()}",
                str(migration_id or "").strip(),
                str(dataset_id or "").strip(),
            )
        return int(value or 0)

    async def count_enabled_segments(self, dataset_id: str) -> int:
        """Denominator for totals: all authoritative enabled chunk rows."""
        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                """
                SELECT COUNT(*)
                FROM segments s
                JOIN documents d ON d.document_id = s.document_id
                WHERE s.dataset_id = $1
                  AND s.enabled = TRUE
                  AND d.enabled = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND COALESCE(s.content_type, 'text') = 'text'
                """,
                str(dataset_id or "").strip(),
            )
        return int(value or 0)

    async def _count_pending_segments_on_connection(
        self,
        conn: Any,
        migration_id: str,
        *,
        dataset_id: str,
    ) -> int:
        value = await conn.fetchval(
            f"SELECT COUNT(*) {self._pending_sql()}",
            str(migration_id or "").strip(),
            str(dataset_id or "").strip(),
        )
        return int(value or 0)

    async def _authority_snapshot_on_connection(
        self,
        conn: Any,
        dataset_id: str,
    ) -> dict[str, Any]:
        """Compute the exact PostgreSQL text-point authority on ``conn``."""

        normalized_dataset = str(dataset_id or "").strip()
        dataset = await conn.fetchrow(
            """
            SELECT dataset_id, tenant_id, collection_name, content_revision
            FROM datasets
            WHERE dataset_id = $1 AND is_deleted = FALSE
            """,
            normalized_dataset,
        )
        if dataset is None:
            raise MigrationStateError(
                "authoritative dataset does not exist or is deleted"
            )
        revision = int(dataset["content_revision"] or 0)
        if revision < 0:
            raise MigrationStateError(
                "dataset index publication is in progress; embedding cutover is blocked"
            )
        rows = await conn.fetch(
            """
            SELECT COALESCE(NULLIF(s.vector_id, ''), s.segment_id) AS point_id,
                   s.text
            FROM segments s
            JOIN documents d ON d.document_id = s.document_id
            WHERE s.dataset_id = $1
              AND s.enabled = TRUE
              AND d.enabled = TRUE
              AND COALESCE(d.archived, FALSE) = FALSE
              AND COALESCE(s.content_type, 'text') = 'text'
            ORDER BY COALESCE(NULLIF(s.vector_id, ''), s.segment_id)
            """,
            normalized_dataset,
        )
        point_ids = [str(row["point_id"]) for row in rows]
        if len(point_ids) != len(set(point_ids)):
            raise MigrationStateError(
                "authoritative segments contain duplicate vector_id values"
            )
        source_entries = [
            (str(row["point_id"]), str(row["text"] or "")) for row in rows
        ]
        return {
            "authority_kind": EMBEDDING_AUTHORITY_KIND,
            "dataset_id": normalized_dataset,
            "tenant_id": str(dataset["tenant_id"] or ""),
            "serving_collection_name": str(dataset["collection_name"] or ""),
            "content_revision": revision,
            "point_count": len(point_ids),
            "point_ids_sha256": point_ids_sha256(point_ids),
            "source_text_sha256": source_text_sha256(source_entries),
        }

    async def authority_snapshot(self, dataset_id: str) -> dict[str, Any]:
        """Repeatable-read corpus evidence persisted by verify and gate."""

        async with self._pool.acquire() as conn, conn.transaction(
            isolation="repeatable_read", readonly=True
        ):
            return await self._authority_snapshot_on_connection(conn, dataset_id)

    async def record_progress_receipts(
        self, migration_id: str, receipts: Sequence[dict[str, Any]]
    ) -> None:
        """Idempotently record chunk receipts after their target points are upserted."""
        if not receipts:
            return
        normalized = str(migration_id or "").strip()
        async with self._pool.acquire() as conn:
            await conn.executemany(
                """
                INSERT INTO embedding_migration_progress (
                    migration_id, segment_id, document_id, position, vector_id, content_hash
                )
                VALUES ($1::uuid, $2, $3, $4, $5, $6)
                ON CONFLICT (migration_id, segment_id)
                DO UPDATE SET
                    content_hash = EXCLUDED.content_hash,
                    vector_id = EXCLUDED.vector_id,
                    written_at = NOW()
                """,
                [
                    (
                        normalized,
                        str(item["segment_id"]),
                        str(item["document_id"]),
                        int(item.get("position") or 0),
                        str(item["vector_id"]),
                        str(item.get("content_hash") or ""),
                    )
                    for item in receipts
                ],
            )

    async def clear_migration_progress(self, migration_id: str) -> int:
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM embedding_migration_progress WHERE migration_id = $1::uuid",
                str(migration_id or "").strip(),
            )
        try:
            return int(str(result).split()[-1])
        except (ValueError, IndexError):  # pragma: no cover
            return 0

    # ------------------------------------------------------- cutover path

    async def cutover_migration(
        self,
        migration_id: str,
        *,
        retention_seconds: int = DEFAULT_RETENTION_SECONDS,
        target_scope: dict[str, Any] | None = None,
        _connection: Any | None = None,
        _dataset_lock_held: bool = False,
    ) -> dict[str, Any]:
        """Atomically flip the dataset to the target binding (PRD §6.3).

        The gate report pins the exact PostgreSQL corpus and target-collection
        digests it evaluated. This method recomputes pending + authority under
        the exclusive dataset lock and requires a fresh Qdrant scope from the
        caller, closing the verify -> gate -> cutover TOCTOU window.

        One transaction, under that lock, then:
          1. CAS re-points datasets.collection_name + embedding identity at the
             target binding (guarded by the current serving collection name —
             and authority content revision);
          2. demotes the old binding to 'retained' with a retention deadline
             (never deleted — the rollback path needs it);
          3. promotes the target binding to 'serving';
          4. stamps per-document embedding provenance (T3 item 1);
          5. moves the migration to 'completed'.
        """
        if _connection is not None:
            return await self._cutover_migration_on_connection(
                _connection,
                migration_id,
                retention_seconds=retention_seconds,
                target_scope=target_scope,
                dataset_lock_held=_dataset_lock_held,
            )
        async with self._pool.acquire() as conn:
            return await self._cutover_migration_on_connection(
                conn,
                migration_id,
                retention_seconds=retention_seconds,
                target_scope=target_scope,
                dataset_lock_held=False,
            )

    async def _cutover_migration_on_connection(
        self,
        conn: Any,
        migration_id: str,
        *,
        retention_seconds: int,
        target_scope: dict[str, Any] | None,
        dataset_lock_held: bool,
    ) -> dict[str, Any]:
        normalized_migration = str(migration_id or "").strip()
        async with conn.transaction():
            preliminary = await conn.fetchrow(
                "SELECT dataset_id FROM embedding_migrations"
                " WHERE migration_id = $1::uuid",
                normalized_migration,
            )
            if preliminary is None:
                raise MigrationStateError("migration not found")
            dataset_id = str(preliminary["dataset_id"])
            if not dataset_lock_held:
                await self._acquire_dataset_lock(conn, dataset_id)

            mig = await conn.fetchrow(
                "SELECT * FROM embedding_migrations"
                " WHERE migration_id = $1::uuid FOR UPDATE",
                normalized_migration,
            )
            if mig is None:  # pragma: no cover - protected by the dataset lock
                raise MigrationStateError("migration not found")
            if str(mig["state"]) != "ready":
                raise MigrationStateError(
                    f"migration must be 'ready' to cut over, is '{mig['state']}'"
                )
            if not mig["source_binding_id"]:
                raise MigrationStateError(
                    "migration has no source binding to cut over from"
                )

            gate_report = _loads_json(mig["gate"], {})
            gate_authority = gate_report.get("authority_snapshot")
            gate_target_scope = gate_report.get("target_scope")
            if not gate_report.get("passed") or not isinstance(
                gate_authority, dict
            ) or not isinstance(gate_target_scope, dict):
                raise MigrationStateError(
                    "cutover requires a passing gate with pinned authority evidence; "
                    "run verify and gate again"
                )

            pending = await self._count_pending_segments_on_connection(
                conn,
                normalized_migration,
                dataset_id=dataset_id,
            )
            if pending:
                raise MigrationStateError(
                    f"embedding corpus drifted after gate: {pending} enabled chunks "
                    "are not present in the target generation"
                )
            authority = await self._authority_snapshot_on_connection(conn, dataset_id)
            if not evidence_matches(
                authority,
                gate_authority,
                keys=AUTHORITY_EVIDENCE_KEYS,
            ):
                raise MigrationStateError(
                    "embedding corpus authority drifted after gate; cutover refused"
                )
            if not isinstance(target_scope, dict):
                raise MigrationStateError(
                    "fresh target collection evidence is unavailable; cutover refused"
                )
            if not evidence_matches(
                target_scope,
                gate_target_scope,
                keys=SCOPE_EVIDENCE_KEYS,
            ) or not authority_matches_scope(authority, target_scope):
                raise MigrationStateError(
                    "target collection drifted after gate (point/source digest mismatch); "
                    "cutover refused"
                )

            src = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(mig["source_binding_id"]),
            )
            tgt = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(mig["target_binding_id"]),
            )
            if src is None or str(src["state"]) != "serving":
                raise MigrationStateError("source binding is no longer serving")
            if tgt is None or str(tgt["state"]) != "shadow":
                raise MigrationStateError("target binding is no longer a shadow")
            flipped = await conn.fetchrow(
                """
                UPDATE datasets
                SET collection_name = $3,
                    embedding_provider = $4,
                    embedding_model = $5,
                    embedding_model_version = $6,
                    embedding_dimension = $7,
                    updated_at = NOW()
                WHERE dataset_id = $1
                  AND is_deleted = FALSE
                  AND COALESCE(collection_name, '') = $2
                  AND content_revision = $8
                RETURNING dataset_id
                """,
                dataset_id,
                str(src["collection_name"]),
                str(tgt["collection_name"]),
                str(tgt["embedding_provider"]),
                str(tgt["embedding_model"]),
                str(tgt["embedding_model_version"]),
                int(tgt["embedding_dimension"]),
                int(authority["content_revision"]),
            )
            if flipped is None:
                raise MigrationStateError(
                    "datasets row no longer points at the serving binding; cutover aborted"
                )
            await conn.execute(
                """
                UPDATE dataset_collection_bindings
                SET state = 'retained',
                    retired_at = NOW(),
                    retained_until = NOW() + make_interval(secs => $2)
                WHERE binding_id = $1::uuid AND state = 'serving'
                """,
                str(src["binding_id"]),
                int(retention_seconds),
            )
            await conn.execute(
                """
                UPDATE dataset_collection_bindings
                SET state = 'serving', activated_at = NOW()
                WHERE binding_id = $1::uuid AND state = 'shadow'
                """,
                str(tgt["binding_id"]),
            )
            # T3 item 1: per-document provenance for the generation that now
            # serves the corpus.
            await conn.execute(
                """
                UPDATE documents
                SET embedding_model = $2,
                    embedding_model_version = $3,
                    embedding_dimension = $4
                WHERE dataset_id = $1
                """,
                dataset_id,
                str(tgt["embedding_model"]),
                str(tgt["embedding_model_version"]),
                int(tgt["embedding_dimension"]),
            )
            row = await conn.fetchrow(
                """
                UPDATE embedding_migrations
                SET state = 'completed', error = NULL, updated_at = NOW()
                WHERE migration_id = $1::uuid AND state = 'ready'
                RETURNING *
                """,
                normalized_migration,
            )
            if row is None:  # pragma: no cover — guarded above
                raise MigrationStateError("cutover lost the migration race")
        return self._migration_to_dict(row)

    async def rollback_migration(
        self,
        migration_id: str,
        *,
        target_state: str = "shadow",
    ) -> dict[str, Any]:
        """Flip the pointer back to the retained (old) binding.

        Post-cutover rollback (PRD §6.4-4): the old collection was never
        deleted, so restoring service is a pointer flip; the target binding
        goes back to ``target_state`` ('shadow' keeps its completed vectors
        for a retried cutover, 'retired' releases it for reclamation).
        """
        if target_state not in {"shadow", "retired"}:
            raise ValueError("rollback target_state must be 'shadow' or 'retired'")
        async with self._pool.acquire() as conn, conn.transaction():
            mig = await conn.fetchrow(
                "SELECT * FROM embedding_migrations WHERE migration_id = $1::uuid FOR UPDATE",
                str(migration_id or "").strip(),
            )
            if mig is None:
                raise MigrationStateError("migration not found")
            if str(mig["state"]) != "completed":
                raise MigrationStateError(
                    f"only a completed migration can roll back, is '{mig['state']}'"
                )
            dataset_id = str(mig["dataset_id"])
            if not mig["source_binding_id"]:
                raise MigrationStateError("migration has no source binding to roll back to")
            await self._acquire_dataset_lock(conn, dataset_id)
            src = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(mig["source_binding_id"]),
            )
            tgt = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(mig["target_binding_id"]),
            )
            if src is None or str(src["state"]) != "retained":
                raise MigrationStateError("old binding was already retired; cannot roll back")
            if tgt is None or str(tgt["state"]) != "serving":
                raise MigrationStateError(
                    "a newer generation has taken over; this rollback is stale"
                )
            flipped = await conn.fetchrow(
                """
                UPDATE datasets
                SET collection_name = $3,
                    embedding_provider = $4,
                    embedding_model = $5,
                    embedding_model_version = $6,
                    embedding_dimension = $7,
                    updated_at = NOW()
                WHERE dataset_id = $1
                  AND is_deleted = FALSE
                  AND COALESCE(collection_name, '') = $2
                RETURNING dataset_id
                """,
                dataset_id,
                str(tgt["collection_name"]),
                str(src["collection_name"]),
                str(src["embedding_provider"]),
                str(src["embedding_model"]),
                str(src["embedding_model_version"]),
                int(src["embedding_dimension"]),
            )
            if flipped is None:
                raise MigrationStateError(
                    "datasets row no longer points at the migrated binding; rollback aborted"
                )
            # Demote the migrated binding first: promoting the retained one
            # before would violate the one-serving-per-dataset invariant.
            retire_now = target_state == "retired"
            await conn.execute(
                """
                UPDATE dataset_collection_bindings
                SET state = $2,
                    retired_at = CASE WHEN $3::boolean THEN NOW() ELSE retired_at END
                WHERE binding_id = $1::uuid AND state = 'serving'
                """,
                str(tgt["binding_id"]),
                target_state,
                retire_now,
            )
            promoted = await conn.fetchrow(
                """
                UPDATE dataset_collection_bindings
                SET state = 'serving', activated_at = NOW(), retained_until = NULL
                WHERE binding_id = $1::uuid AND state = 'retained'
                RETURNING binding_id
                """,
                str(src["binding_id"]),
            )
            if promoted is None:
                raise MigrationStateError(
                    "retained binding could not be promoted; rollback aborted"
                )
            # Re-stamp document provenance to the restored generation.
            await conn.execute(
                """
                UPDATE documents
                SET embedding_model = $2,
                    embedding_model_version = $3,
                    embedding_dimension = $4
                WHERE dataset_id = $1
                """,
                dataset_id,
                str(src["embedding_model"]),
                str(src["embedding_model_version"]),
                int(src["embedding_dimension"]),
            )
            row = await conn.fetchrow(
                """
                UPDATE embedding_migrations
                SET state = 'rolled_back', updated_at = NOW()
                WHERE migration_id = $1::uuid AND state = 'completed'
                RETURNING *
                """,
                str(migration_id or "").strip(),
            )
            if row is None:  # pragma: no cover
                raise MigrationStateError("rollback lost the migration race")
            if retire_now:
                await conn.execute(
                    "DELETE FROM embedding_migration_progress WHERE migration_id = $1::uuid",
                    str(migration_id or "").strip(),
                )
        return self._migration_to_dict(row)

    async def reopen_migration_for_retry(self, migration_id: str) -> dict[str, Any] | None:
        """Reopen a rolled-back migration whose target binding is still shadow.

        This is the escape hatch that keeps a rolled-back (keep_shadow) attempt
        operator-actionable: start_migration re-adopts it instead of wedging on
        the reserved shadow name. Returns None when the migration is not
        rolled_back or its target binding no longer holds the vectors (only a
        'shadow' binding is retryable). Moving back into 'backfilling' (a live
        state) can collide with the one-live-per-dataset index; that race maps
        to MigrationStateError (409), never a raw unique-violation 500.
        """
        async with self._pool.acquire() as conn, conn.transaction():
            mig = await conn.fetchrow(
                "SELECT * FROM embedding_migrations WHERE migration_id = $1::uuid FOR UPDATE",
                str(migration_id or "").strip(),
            )
            if mig is None or str(mig["state"]) != "rolled_back":
                return None
            await self._acquire_dataset_lock(conn, str(mig["dataset_id"]))
            tgt = await conn.fetchrow(
                "SELECT * FROM dataset_collection_bindings WHERE binding_id = $1::uuid",
                str(mig["target_binding_id"]),
            )
            if tgt is None or str(tgt["state"]) != "shadow":
                return None
            try:
                row = await conn.fetchrow(
                    """
                    UPDATE embedding_migrations
                    SET state = 'backfilling', error = NULL, updated_at = NOW()
                    WHERE migration_id = $1::uuid AND state = 'rolled_back'
                    RETURNING *
                    """,
                    str(migration_id or "").strip(),
                )
            except asyncpg.UniqueViolationError as exc:
                raise MigrationStateError(
                    "cannot reopen migration: dataset already has a live "
                    "embedding migration"
                ) from exc
        return self._migration_to_dict(row) if row else None

    async def abandon_migration(
        self, migration_id: str, *, reason: str | None = None
    ) -> dict[str, Any]:
        """Terminate a pre-cutover migration and release the shadow binding.

        The target collection becomes reclaimable (state 'retired' frees its
        name reservation and marks the physical collection for explicit
        deletion); the serving binding and datasets row are untouched, so the
        dataset never stops being served (zero-window rule).

        Admissible states: every live state plus the operator-actionable
        non-live ones — ``failed`` (backfill never finished), ``gate_failed``
        (eval rejected the generation), and ``rolled_back`` with a retained
        shadow (the post-cutover-rollback wedge: without this escape the
        reserved shadow name would make the dataset un-startable and
        un-abortable). ``completed`` is deliberately excluded: its target
        binding is the serving one, and retiring it would take the dataset
        offline — the correct exit from ``completed`` is rollback, not abort.
        """
        async with self._pool.acquire() as conn, conn.transaction():
            mig = await conn.fetchrow(
                "SELECT * FROM embedding_migrations WHERE migration_id = $1::uuid FOR UPDATE",
                str(migration_id or "").strip(),
            )
            if mig is None:
                raise MigrationStateError("migration not found")
            if str(mig["state"]) not in ABANDONABLE_MIGRATION_STATES:
                raise MigrationStateError(
                    f"migration in state '{mig['state']}' cannot be abandoned"
                )
            jobs_table = await conn.fetchval(
                "SELECT to_regclass('knowledge.embedding_migration_action_jobs')::text"
            )
            if jobs_table:
                active_job = await conn.fetchrow(
                    """
                    SELECT job_id, action, state
                    FROM knowledge.embedding_migration_action_jobs
                    WHERE migration_id = $1::uuid
                      AND state IN ('queued', 'running')
                    LIMIT 1
                    FOR UPDATE
                    """,
                    str(migration_id or "").strip(),
                )
                if active_job is not None and str(active_job["state"]) == "running":
                    raise MigrationStateError(
                        f"cannot abandon while '{active_job['action']}' job is running"
                    )
                if active_job is not None:
                    await conn.execute(
                        """
                        UPDATE knowledge.embedding_migration_action_jobs
                        SET state = 'failed', error = 'cancelled by migration abort',
                            result = NULL, terminal_claim_token = NULL,
                            finished_at = NOW(), updated_at = NOW()
                        WHERE job_id = $1::uuid AND state = 'queued'
                        """,
                        str(active_job["job_id"]),
                    )
            dataset_id = str(mig["dataset_id"])
            await self._acquire_dataset_lock(conn, dataset_id)
            await conn.execute(
                """
                UPDATE dataset_collection_bindings
                SET state = 'retired', retired_at = NOW()
                WHERE binding_id = $1::uuid AND state = 'shadow'
                """,
                str(mig["target_binding_id"]),
            )
            row = await conn.fetchrow(
                """
                UPDATE embedding_migrations
                SET state = 'abandoned', error = $2, updated_at = NOW()
                WHERE migration_id = $1::uuid
                  AND state = ANY($3::text[])
                RETURNING *
                """,
                str(migration_id or "").strip(),
                str(reason or "abandoned").strip()[:2000],
                list(ABANDONABLE_MIGRATION_STATES),
            )
            if row is None:  # pragma: no cover
                raise MigrationStateError("abandon lost the migration race")
            await conn.execute(
                "DELETE FROM embedding_migration_progress WHERE migration_id = $1::uuid",
                str(migration_id or "").strip(),
            )
        return self._migration_to_dict(row)

    # ------------------------------------------------------- retention path

    async def list_reclaimable_bindings(self) -> list[dict[str, Any]]:
        """Retained bindings whose retention period has elapsed."""
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT * FROM dataset_collection_bindings
                WHERE state = 'retained'
                  AND retained_until IS NOT NULL
                  AND retained_until <= NOW()
                ORDER BY retained_until
                """
            )
        return [self._binding_to_dict(row) for row in rows]

    async def retire_binding(self, binding_id: str) -> dict[str, Any] | None:
        """Mark a retained binding retired (its physical collection becomes
        deletable by the caller). Serving bindings can never be retired here."""
        normalized_binding = str(binding_id or "").strip()
        async with self._pool.acquire() as conn, conn.transaction():
            binding = await conn.fetchrow(
                """
                SELECT dataset_id
                FROM dataset_collection_bindings
                WHERE binding_id = $1::uuid
                """,
                normalized_binding,
            )
            if binding is None:
                return None
            # Reclamation must serialize with rollback. Otherwise it can retire
            # the retained source after rollback validates it but before the
            # source is promoted, leaving datasets.collection_name pointing at
            # a binding that the caller is about to delete physically.
            await self._acquire_dataset_lock(conn, str(binding["dataset_id"]))
            row = await conn.fetchrow(
                """
                UPDATE dataset_collection_bindings
                SET state = 'retired', retired_at = NOW()
                WHERE binding_id = $1::uuid AND state = 'retained'
                RETURNING *
                """,
                normalized_binding,
            )
        return self._binding_to_dict(row) if row else None

    # ------------------------------------------------------ vector cache (T3.4)

    async def lookup_embeddings_batch(
        self,
        *,
        embedding_provider: str,
        embedding_model: str,
        embedding_model_version: str,
        content_hashes: Sequence[str],
    ) -> dict[str, list[float]]:
        """One `WHERE content_hash = ANY(...)` batch read, keyed by content hash.

        The (provider, model, version) identity is part of the key, so entries
        from a previous embedding generation are never returned for a new one.
        """
        provider = str(embedding_provider or "").strip()
        model = str(embedding_model or "").strip()
        version = str(embedding_model_version or "").strip()
        if not provider or not model:
            raise ValueError("vector-cache identity requires provider and model")
        hashes = [str(item) for item in content_hashes if str(item or "").strip()]
        if not hashes:
            return {}
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT content_hash, vector
                FROM embedding_vector_cache
                WHERE embedding_provider = $1
                  AND embedding_model = $2
                  AND embedding_model_version = $3
                  AND content_hash = ANY($4::text[])
                """,
                provider,
                model,
                version,
                hashes,
            )
        return {str(row["content_hash"]): [float(v) for v in row["vector"]] for row in rows}

    async def store_embeddings_batch(
        self,
        *,
        embedding_provider: str,
        embedding_model: str,
        embedding_model_version: str,
        entries: Sequence[tuple[str, Sequence[float]]],
    ) -> int:
        """Upsert (hash → vector) entries; float8[] storage, never pickle."""
        provider = str(embedding_provider or "").strip()
        model = str(embedding_model or "").strip()
        version = str(embedding_model_version or "").strip()
        if not provider or not model:
            raise ValueError("vector-cache identity requires provider and model")
        rows = [
            (
                provider,
                model,
                version,
                str(hash_value),
                [float(component) for component in vector],
            )
            for hash_value, vector in entries
            if str(hash_value or "").strip() and vector
        ]
        if not rows:
            return 0
        async with self._pool.acquire() as conn:
            await conn.executemany(
                """
                INSERT INTO embedding_vector_cache (
                    embedding_provider, embedding_model, embedding_model_version,
                    content_hash, vector, dimension, updated_at
                )
                VALUES ($1, $2, $3, $4, $5::double precision[], cardinality($5::double precision[]), NOW())
                ON CONFLICT (
                    embedding_provider, embedding_model, embedding_model_version, content_hash
                )
                DO UPDATE SET
                    vector = EXCLUDED.vector,
                    dimension = EXCLUDED.dimension,
                    updated_at = NOW()
                """,
                rows,
            )
        return len(rows)

    async def purge_vector_cache_for_model(
        self,
        *,
        embedding_provider: str,
        embedding_model: str,
        embedding_model_version: str,
    ) -> int:
        """Drop all cached vectors of one embedding identity (e.g. a model was
        found to produce corrupted vectors). Returns deleted row count."""
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                DELETE FROM embedding_vector_cache
                WHERE embedding_provider = $1
                  AND embedding_model = $2
                  AND embedding_model_version = $3
                """,
                str(embedding_provider or "").strip(),
                str(embedding_model or "").strip(),
                str(embedding_model_version or "").strip(),
            )
        try:
            return int(str(result).split()[-1])
        except (ValueError, IndexError):  # pragma: no cover
            return 0
