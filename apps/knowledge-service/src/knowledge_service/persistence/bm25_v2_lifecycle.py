"""T6: durable BM25 v2 cutover/rollback lifecycle state (PostgreSQL side).

This module owns the ``kb_bm25_v2_lifecycle`` table created by
``database/migrations/105_kb_bm25_v2_lifecycle.sql`` and the exclusive
dataset-index barrier that fences every writer across the transition
lifecycle (PRD T6.1). Design notes:

* Lifecycle state lives only in this persistent table. There is deliberately
  no TTL flag, heartbeat, or Redis anywhere in the protocol (addendum
  §T6.3): mutual exclusion comes from a PostgreSQL advisory lock in the same
  key namespace as ``DatasetPersistenceMixin.dataset_index_write_lease``
  (``knowledge-dataset-index:<dataset_id>``). While the barrier is held, every
  Qdrant point write/delete fails closed at ``pg_try_advisory_xact_lock_shared``
  without waiting, so the fence cannot be bypassed by a writer that ignores
  table state.
* Crash recovery needs no expiry: a new executor acquires the barrier first,
  so any in-progress row it finds belongs to a dead executor and is reset via
  ``fail_transition`` — the ``(epoch, lock_token)`` CAS makes a late writer
  from the dead executor impossible.
* The PostgreSQL dataset lexical profile is flipped with a content-revision
  + current-value CAS here, never through the ordinary dataset update path,
  so the dataset-boundary "active_version request" hard rejection in
  ``dataset_service`` stays untouched for every other caller (PRD §9 BM25 v2
  boundary).
* Authority digests duplicate the frozen algorithms of
  ``scripts/backfill_bm25_v2.py`` on purpose: a service module must not import
  from ``scripts/``, and these two functions are versioned contract, not
  configuration. The SQL here matches the backfill authority query exactly.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import logging
import secrets
import time
import uuid
from dataclasses import dataclass
from typing import Any


def _decode_json_field(value: Any) -> Any:
    """asyncpg hands JSONB back as text unless a codec was set explicitly."""

    if isinstance(value, (bytes, bytearray)):
        value = value.decode("utf-8")
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return {}
    return value

logger = logging.getLogger(__name__)

LEXICAL_LIFECYCLE_STEADY_STATES = frozenset({"shadow", "active_v2"})
LEXICAL_LIFECYCLE_IN_PROGRESS_STATES = frozenset(
    {"cutover_in_progress", "rollback_in_progress"}
)
# Quiescence vocabulary: a transition waits for pipeline work *in flight*
# (parsing|splitting|indexing|uploading_images|syncing and legacy values) to
# settle. ``waiting`` rows are durable queue backlog, not in-flight work —
# claims are already fenced by the barrier, so the backlog is preserved
# untouched through the transition (PRD T6 "no data loss") and must never
# block a cutover.
_NON_BUSY_DOCUMENT_STATUSES = ("completed", "error", "waiting")

AUTHORITY_KIND = "postgres-segments-enabled-l3-text-base-v2"


class Bm25V2LifecycleDbError(RuntimeError):
    """Raised when the lifecycle persistence layer cannot serve a request."""


class LifecycleTransitionBusy(Bm25V2LifecycleDbError):
    """Another dataset-index lifecycle work holds the barrier."""


class LifecycleStateConflict(Bm25V2LifecycleDbError):
    """The persisted lifecycle row is not in the state the CAS expected."""


def point_ids_sha256(point_ids: Any) -> str:
    """Sorted-ID digest; must stay byte-identical to the backfill contract."""

    encoded = "".join(f"{point_id}\n" for point_id in sorted(map(str, point_ids)))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def source_text_sha256(entries: Any) -> str:
    """Sorted (id, sha256(text)) digest; must match the backfill contract."""

    lines: list[str] = []
    for point_id, text in sorted(entries, key=lambda item: str(item[0])):
        text_digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
        lines.append(f"{point_id}\0{text_digest}\n")
    return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()


@dataclass(frozen=True, slots=True)
class AuthoritySnapshot:
    """Immutable PostgreSQL authority for one base text collection."""

    collection_name: str
    tenant_id: str
    dataset_id: str
    content_revision: int
    point_count: int
    point_ids_sha256: str
    source_text_sha256: str
    authority_kind: str = AUTHORITY_KIND


class Bm25V2LifecycleStore:
    """Pool-backed lifecycle state, writer barrier, and CAS flips for T6."""

    def __init__(self, pool: Any) -> None:
        if pool is None:
            raise ValueError("Bm25V2LifecycleStore requires an asyncpg pool")
        self._pool = pool

    # ------------------------------------------------------------------ lock

    @staticmethod
    def dataset_lock_name(dataset_id: str) -> str:
        # Same key as DatasetPersistenceMixin._dataset_index_lock_name (and
        # EmbeddingVersionStore._dataset_lock_name): the exclusive barrier
        # conflicts with the shared xact lock of every dataset index write
        # lease, which is exactly the exclusion T6.1 requires.
        normalized = str(dataset_id or "").strip()
        if not normalized:
            raise ValueError("dataset_id is required for lifecycle locking")
        return f"knowledge-dataset-index:{normalized}"

    @contextlib.asynccontextmanager
    async def transition_barrier(self, dataset_id: str, *, wait_s: float = 0.0):
        """Hold the dataset-exclusive session advisory lock for one transition.

        Deliberately a session lock (not transaction-scoped): the yielded
        connection can commit the durable in-progress row while the barrier
        stays held across remote Qdrant work, mirroring
        ``dataset_index_delete_lease``.

        ``wait_s`` bounds how long acquisition tolerates transient holders —
        short ``dataset_index_write_lease`` transactions between upserts are
        legitimate, and a zero-wait try would fail closed on them; advisory
        locks have no queue, so this is polling with a deadline. Sustained
        contention still raises ``LifecycleTransitionBusy``.
        """

        if not self._pool:
            raise Bm25V2LifecycleDbError("database is not connected")
        lock_name = self.dataset_lock_name(dataset_id)
        deadline = time.monotonic() + max(0.0, float(wait_s))
        async with self._pool.acquire() as conn:
            acquired = False
            try:
                while True:
                    acquired = await conn.fetchval(
                        "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                        lock_name,
                    )
                    if acquired is True:
                        break
                    if time.monotonic() >= deadline:
                        raise LifecycleTransitionBusy(
                            "dataset index lifecycle work is already in progress; "
                            "refusing a concurrent BM25 v2 transition"
                        )
                    await asyncio.sleep(0.02)
                yield conn
            finally:
                if acquired:
                    unlocked = await conn.fetchval(
                        "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                        lock_name,
                    )
                    if unlocked is not True:
                        logger.error(
                            "BM25 v2 lifecycle barrier was not released",
                            extra={"dataset_id": dataset_id},
                        )

    # ------------------------------------------------------------------ rows

    async def get_state(self, dataset_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM kb_bm25_v2_lifecycle WHERE dataset_id = $1",
                str(dataset_id),
            )
        if row is None:
            return None
        return {
            "dataset_id": str(row["dataset_id"]),
            "tenant_id": str(row["tenant_id"]),
            "state": str(row["state"]),
            "epoch": int(row["epoch"]),
            "transition_kind": row["transition_kind"],
            "lock_token": row["lock_token"],
            "authority_content_revision": row["authority_content_revision"],
            "manifest_sha256": str(row["manifest_sha256"] or ""),
            "pre_evidence": dict(_decode_json_field(row["pre_evidence"]) or {}),
            "post_evidence": dict(_decode_json_field(row["post_evidence"]) or {}),
            "last_error": row["last_error"],
            "updated_at": row["updated_at"].isoformat() if row["updated_at"] else None,
        }

    async def ensure_row(self, *, dataset_id: str, tenant_id: str) -> None:
        """Create the steady 'shadow' row for a dataset new to the protocol."""

        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO kb_bm25_v2_lifecycle (dataset_id, tenant_id, state)
                VALUES ($1, $2, 'shadow')
                ON CONFLICT (dataset_id) DO NOTHING
                """,
                str(dataset_id),
                str(tenant_id),
            )

    async def begin_transition(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        kind: str,
        from_state: str,
        authority_content_revision: int | None = None,
    ) -> tuple[int, str]:
        """CAS a steady-state row into an in-progress transition.

        Returns ``(epoch, lock_token)``. A caller must already hold
        ``transition_barrier`` — the row CAS is defense-in-depth against a
        non-locking reader, not the fence itself. A stale in-progress row left
        by a dead executor is rejected here; the recovery executor resets it
        via ``fail_transition`` while holding the barrier.
        """

        in_progress = (
            "cutover_in_progress" if kind == "cutover" else "rollback_in_progress"
        )
        token = f"{uuid.uuid4().hex}{secrets.token_hex(8)}"[:64]
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                INSERT INTO kb_bm25_v2_lifecycle (
                    dataset_id, tenant_id, state, epoch, transition_kind,
                    lock_token, authority_content_revision
                )
                VALUES ($1, $2, $4, 1, $3, $5, $6)
                ON CONFLICT (dataset_id) DO UPDATE
                SET state = $4,
                    epoch = kb_bm25_v2_lifecycle.epoch + 1,
                    transition_kind = $3,
                    lock_token = $5,
                    authority_content_revision = $6,
                    manifest_sha256 = '',
                    pre_evidence = '{}'::jsonb,
                    post_evidence = '{}'::jsonb,
                    last_error = NULL,
                    updated_at = NOW()
                WHERE kb_bm25_v2_lifecycle.state = $7
                RETURNING epoch
                """,
                str(dataset_id),
                str(tenant_id),
                str(kind),
                in_progress,
                token,
                authority_content_revision,
                str(from_state),
            )
        if row is None:
            current = await self.get_state(dataset_id)
            raise LifecycleStateConflict(
                f"dataset {dataset_id} lifecycle cannot start a {kind} from "
                f"state {current['state'] if current else 'absent'}"
            )
        return int(row["epoch"]), token

    async def finish_transition(
        self,
        *,
        dataset_id: str,
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        target_state: str,
        pre_evidence: dict[str, Any] | None = None,
        post_evidence: dict[str, Any] | None = None,
        manifest_sha256: str = "",
        authority_content_revision: int | None = None,
    ) -> None:
        await self._settle(
            dataset_id=dataset_id,
            epoch=epoch,
            lock_token=lock_token,
            in_progress_state=in_progress_state,
            target_state=target_state,
            pre_evidence=pre_evidence,
            post_evidence=post_evidence,
            manifest_sha256=manifest_sha256,
            authority_content_revision=authority_content_revision,
            last_error=None,
        )

    async def fail_transition(
        self,
        *,
        dataset_id: str,
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        recovered_state: str,
        error: str,
        post_evidence: dict[str, Any] | None = None,
    ) -> None:
        await self._settle(
            dataset_id=dataset_id,
            epoch=epoch,
            lock_token=lock_token,
            in_progress_state=in_progress_state,
            target_state=recovered_state,
            pre_evidence=None,
            post_evidence=post_evidence,
            manifest_sha256="",
            authority_content_revision=None,
            last_error=str(error)[:2000],
        )

    async def _settle(
        self,
        *,
        dataset_id: str,
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        target_state: str,
        pre_evidence: dict[str, Any] | None,
        post_evidence: dict[str, Any] | None,
        manifest_sha256: str,
        authority_content_revision: int | None,
        last_error: str | None,
    ) -> None:
        row = None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                UPDATE kb_bm25_v2_lifecycle
                SET state = $3,
                    epoch = epoch + 1,
                    transition_kind = NULL,
                    lock_token = NULL,
                    authority_content_revision = COALESCE($7, CASE
                        WHEN $11::text = 'shadow' THEN NULL
                        ELSE authority_content_revision END),
                    manifest_sha256 = CASE WHEN $8::text = '' THEN manifest_sha256 ELSE $8::text END,
                    pre_evidence = COALESCE($5::jsonb, pre_evidence),
                    post_evidence = COALESCE($6::jsonb, post_evidence),
                    last_error = $9,
                    updated_at = NOW()
                WHERE dataset_id = $1
                  AND epoch = $2
                  AND lock_token = $4
                  AND state = $10
                RETURNING epoch
                """,
                str(dataset_id),
                int(epoch),
                str(target_state),
                str(lock_token),
                json.dumps(pre_evidence) if pre_evidence is not None else None,
                json.dumps(post_evidence) if post_evidence is not None else None,
                authority_content_revision,
                str(manifest_sha256 or ""),
                last_error,
                str(in_progress_state),
                str(target_state),
            )
        if row is None:
            raise LifecycleStateConflict(
                f"dataset {dataset_id} lifecycle transition CAS failed "
                "(stale epoch/token or state moved under us)"
            )

    async def reset_stale_transition(
        self,
        *,
        dataset_id: str,
        in_progress_state: str,
        recovered_state: str,
        error: str,
    ) -> bool:
        """Recovery-only: clear an in-progress row WITHOUT a token match.

        The caller must hold the transition barrier — the barrier guarantees
        no live executor can own the row, so the in-progress state is by
        definition stale. Returns False when the row is not in the expected
        in-progress state.
        """

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                UPDATE kb_bm25_v2_lifecycle
                SET state = $3,
                    epoch = epoch + 1,
                    transition_kind = NULL,
                    lock_token = NULL,
                    last_error = $4,
                    updated_at = NOW()
                WHERE dataset_id = $1 AND state = $2
                RETURNING epoch
                """,
                str(dataset_id),
                str(in_progress_state),
                str(recovered_state),
                str(error)[:2000],
            )
        return row is not None

    async def reconcile_steady_state(
        self,
        *,
        dataset_id: str,
        from_state: str,
        target_state: str,
        error: str,
    ) -> bool:
        """Repair a steady-state row that contradicts the storage truth.

        The lifecycle row is evidence, not authority: PostgreSQL's lexical
        profile (plus the collection metadata) is truth. A failed revert in
        ``_abort`` can leave the row at ``shadow`` while the profile is
        already ``bm25_v2`` (or vice versa), and a transition that completed
        before this table existed leaves no row at all. The caller must hold
        the transition barrier; the steady→steady CAS without a token is safe
        precisely because winning the barrier proves no live transition owns
        the row. Returns False when the row is not in ``from_state``.
        """

        if from_state not in LEXICAL_LIFECYCLE_STEADY_STATES or (
            target_state not in LEXICAL_LIFECYCLE_STEADY_STATES
        ):
            raise ValueError("reconcile_steady_state requires steady states")
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                UPDATE kb_bm25_v2_lifecycle
                SET state = $3,
                    epoch = epoch + 1,
                    last_error = $4,
                    updated_at = NOW()
                WHERE dataset_id = $1 AND state = $2
                  AND transition_kind IS NULL
                RETURNING epoch
                """,
                str(dataset_id),
                str(from_state),
                str(target_state),
                str(error)[:2000],
            )
        return row is not None

    async def certify_active_publication(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        expected_epoch: int,
        authority_content_revision: int,
        manifest_sha256: str,
        post_evidence: dict[str, Any],
        connection: Any,
    ) -> int:
        """CAS fresh runtime-publication evidence onto one steady active row.

        The caller closes the dataset's negative publication revision in the
        same PostgreSQL transaction.  Keeping this method connection-bound
        makes the positive dataset revision and its lifecycle evidence one
        atomic authority change; a stale epoch leaves the revision negative.
        """

        if int(authority_content_revision) <= 0:
            raise ValueError("active publication authority revision must be positive")
        row = await connection.fetchrow(
            """
            UPDATE kb_bm25_v2_lifecycle
            SET epoch = epoch + 1,
                authority_content_revision = $4,
                manifest_sha256 = $5,
                post_evidence = $6::jsonb,
                last_error = NULL,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND tenant_id = $2
              AND epoch = $3
              AND state = 'active_v2'
              AND transition_kind IS NULL
              AND lock_token IS NULL
            RETURNING epoch
            """,
            str(dataset_id),
            str(tenant_id),
            int(expected_epoch),
            int(authority_content_revision),
            str(manifest_sha256),
            json.dumps(post_evidence),
        )
        if row is None:
            raise LifecycleStateConflict(
                f"dataset {dataset_id} active publication CAS failed "
                "(stale epoch or transition started)"
            )
        return int(row["epoch"])

    # ------------------------------------------------------------- quiescence

    async def count_busy_documents(self, dataset_id: str) -> int:
        """Pipeline work in flight for one dataset (queue backlog excluded)."""

        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                """
                SELECT COUNT(*)
                FROM documents
                WHERE dataset_id = $1
                  AND status <> ALL($2::text[])
                """,
                str(dataset_id),
                list(_NON_BUSY_DOCUMENT_STATUSES),
            )
        return int(value or 0)

    async def count_dispatchable_documents(self, dataset_id: str) -> int:
        """Rows the durable dispatcher could hand to a worker right now.

        Mirrors ``list_queued_documents`` for one dataset (same waiting +
        generation-marker predicates) so the transition logs a meaningful
        queue depth beside the broader busy count.
        """

        async with self._pool.acquire() as conn:
            value = await conn.fetchval(
                """
                SELECT COUNT(*)
                FROM documents
                WHERE dataset_id = $1
                  AND status = 'waiting'
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_generation'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_failed'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_confluence_sync_generation'
                  )
                """,
                str(dataset_id),
            )
        return int(value or 0)

    # --------------------------------------------------------- authority + CAS

    async def dataset_snapshot(self, dataset_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT dataset_id, tenant_id, collection_name, content_revision,
                       index_config, is_deleted
                FROM datasets
                WHERE dataset_id = $1
                """,
                str(dataset_id),
            )
        if row is None:
            return None
        return {
            "dataset_id": str(row["dataset_id"]),
            "tenant_id": str(row["tenant_id"] or ""),
            "collection_name": str(row["collection_name"] or ""),
            "content_revision": int(row["content_revision"] or 0),
            "index_config": dict(_decode_json_field(row["index_config"]) or {}),
            "is_deleted": bool(row["is_deleted"]),
        }

    async def authority_snapshot(
        self,
        *,
        collection_name: str,
        tenant_id: str,
        dataset_id: str,
        connection: Any | None = None,
    ) -> AuthoritySnapshot:
        """Recompute the enabled-L3-text PostgreSQL authority set.

        Serving authority follows each completed, enabled L3 text segment
        under an enabled, non-archived, non-lifecycle-reindex document in the
        dataset's base collection. Document status does not hide an old serving
        segment during indexing or after a failed reprocess. With ``connection``,
        read on the publisher's transaction so the digest includes its
        uncommitted candidate rows. The caller owns that transaction and must
        not pass a connection from a different publisher.
        """

        async def _read(conn: Any) -> AuthoritySnapshot:
            dataset = await conn.fetchrow(
                """
                SELECT dataset_id, tenant_id, collection_name, content_revision,
                       index_config
                FROM datasets
                WHERE dataset_id = $1 AND is_deleted = FALSE
                """,
                str(dataset_id),
            )
            if dataset is None:
                raise Bm25V2LifecycleDbError(
                    "authoritative dataset does not exist or is deleted"
                )
            if str(dataset["tenant_id"] or "") != str(tenant_id):
                raise Bm25V2LifecycleDbError(
                    "authoritative dataset tenant_id does not match request"
                )
            if str(dataset["collection_name"] or "") != str(collection_name):
                raise Bm25V2LifecycleDbError(
                    "authoritative dataset base collection does not match request"
                )
            index_config = _decode_json_field(dataset["index_config"]) or {}
            if (index_config.get("retrieval") or {}).get("_index_deletion_fence") is not None:
                raise Bm25V2LifecycleDbError(
                    "authoritative dataset index deletion is pending"
                )
            rows = await conn.fetch(
                """
                SELECT s.vector_id::text AS point_id, s.text,
                       s.enabled AS segment_enabled,
                       s.status AS segment_status,
                       d.enabled AS document_enabled,
                       d.archived AS document_archived,
                       (
                           COALESCE(d.metadata, '{}'::jsonb)
                           ? '_document_lifecycle_reindex'
                       ) AS document_lifecycle_pending
                FROM segments AS s
                JOIN documents AS d
                  ON d.document_id = s.document_id
                 AND d.dataset_id = s.dataset_id
                JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                WHERE s.dataset_id = $1
                  AND ds.tenant_id = $2
                  AND ds.collection_name = $3
                  AND ds.is_deleted = FALSE
                  AND NOT (
                      COALESCE(ds.index_config -> 'retrieval', '{}'::jsonb)
                      ? '_index_deletion_fence'
                  )
                  AND s.vector_id IS NOT NULL
                  AND s.vector_id <> ''
                  AND COALESCE(s.enabled, TRUE) = TRUE
                  AND s.status = 'completed'
                  AND COALESCE(s.level, 3) = 3
                  AND COALESCE(s.content_type, 'text') = 'text'
                  AND COALESCE(d.enabled, TRUE) = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND NOT (
                      COALESCE(d.metadata, '{}'::jsonb)
                      ? '_document_lifecycle_reindex'
                  )
                ORDER BY s.vector_id::text
                """,
                str(dataset_id),
                str(tenant_id),
                str(collection_name),
            )
            point_ids = [str(row["point_id"]) for row in rows]
            if len(point_ids) != len(set(point_ids)):
                raise Bm25V2LifecycleDbError(
                    "authoritative segments contain duplicate vector_id values"
                )
            source_entries = [
                (str(row["point_id"]), str(row["text"] or "")) for row in rows
            ]
            return AuthoritySnapshot(
                collection_name=str(collection_name),
                tenant_id=str(tenant_id),
                dataset_id=str(dataset_id),
                content_revision=int(dataset["content_revision"] or 0),
                point_count=len(point_ids),
                point_ids_sha256=point_ids_sha256(point_ids),
                source_text_sha256=source_text_sha256(source_entries),
            )

        if connection is not None:
            return await _read(connection)
        async with self._pool.acquire() as conn, conn.transaction(
            isolation="repeatable_read", readonly=True
        ):
            return await _read(conn)

    async def flip_dataset_lexical_active_version(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        expected_active_version: str,
        target_active_version: str,
        shadow_write_enabled: bool,
        expected_content_revision: int,
    ) -> dict[str, Any]:
        """CAS the dataset's persisted lexical profile to a new selection.

        Ordinary dataset update paths keep their hard rejection of a
        requested ``bm25_v2`` active version; this CAS is the only writer of
        an active lexical profile and is called exclusively from the T6
        lifecycle protocol while holding the transition barrier. Refuses on
        deletion fence, wrong current selection, wrong revision, or a
        missing lexical block.
        """

        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                UPDATE datasets
                SET index_config = jsonb_set(
                        jsonb_set(
                            index_config,
                            '{retrieval,lexical,active_version}',
                            to_jsonb($3::text),
                            FALSE
                        ),
                        '{retrieval,lexical,bm25_v2,shadow_write_enabled}',
                        to_jsonb($4::boolean),
                        FALSE
                    ),
                    updated_at = NOW()
                WHERE dataset_id = $1
                  AND is_deleted = FALSE
                  AND tenant_id = $6
                  AND content_revision = $5
                  AND NOT (
                      COALESCE(index_config -> 'retrieval', '{}'::jsonb)
                      ? '_index_deletion_fence'
                  )
                  AND index_config #> '{retrieval,lexical}' IS NOT NULL
                  AND COALESCE(
                      index_config -> 'retrieval' -> 'lexical' ->> 'active_version',
                      'lexical_v1'
                  ) = $2
                RETURNING content_revision,
                          index_config -> 'retrieval' -> 'lexical' AS lexical
                """,
                str(dataset_id),
                str(expected_active_version),
                str(target_active_version),
                bool(shadow_write_enabled),
                int(expected_content_revision),
                str(tenant_id),
            )
        if row is None:
            raise LifecycleStateConflict(
                f"dataset {dataset_id} lexical CAS failed (revision moved, "
                "profile is stale, deletion fence, or wrong current active_version)"
            )
        return {
            "content_revision": int(row["content_revision"] or 0),
            "lexical": json.loads(row["lexical"]) if isinstance(row["lexical"], str) else dict(row["lexical"] or {}),
        }
