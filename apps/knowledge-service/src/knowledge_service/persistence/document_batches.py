"""Durable document batch persistence with fair, crash-safe item claims."""

from __future__ import annotations

import json
import uuid
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

_TERMINAL_ITEM_STATES = frozenset({"queued", "skipped", "failed"})


@dataclass(frozen=True)
class ClaimedDocumentBatchItem:
    operation_id: str
    tenant_id: str
    dataset_id: str
    operation: str
    document_id: str
    created_by: str
    actor_roles: tuple[str, ...]


class DocumentBatchStore:
    """Small asyncpg-backed store shared by API and worker processes."""

    def __init__(self, pool: Any) -> None:
        if pool is None:
            raise RuntimeError("document batch persistence requires PostgreSQL")
        self._pool = pool

    async def create_operation(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        operation: str,
        created_by: str,
        actor_roles: Sequence[str],
        document_ids: Sequence[str] | None,
        all_documents: bool = False,
        operation_id: str | None = None,
    ) -> dict[str, Any]:
        if operation not in {"reembed", "delete"}:
            raise ValueError("unsupported document batch operation")
        if all_documents and operation != "reembed":
            raise ValueError("all_documents is supported only for reembed")

        normalized_ids = list(
            dict.fromkeys(str(value or "").strip() for value in document_ids or ())
        )
        normalized_ids = [value for value in normalized_ids if value]
        if not all_documents and not normalized_ids:
            raise ValueError("document_ids must not be empty")

        normalized_operation_id = str(operation_id or uuid.uuid4()).strip()
        try:
            uuid.UUID(normalized_operation_id)
        except ValueError as exc:
            raise ValueError("operation_id must be a UUID") from exc
        normalized_roles = sorted(
            {str(role or "").strip() for role in actor_roles if str(role or "").strip()}
        )
        async with self._pool.acquire() as conn, conn.transaction():
            dataset = await conn.fetchrow(
                """
                    SELECT dataset_id, tenant_id
                    FROM knowledge.datasets
                    WHERE dataset_id = $1 AND is_deleted = FALSE
                    FOR SHARE
                    """,
                dataset_id,
            )
            if not dataset or str(dataset["tenant_id"] or "") != tenant_id:
                raise ValueError("dataset not found")

            inserted = await conn.fetchval(
                """
                    INSERT INTO knowledge.kb_document_batch_operations (
                        operation_id, tenant_id, dataset_id, operation,
                        created_by, actor_roles
                    ) VALUES ($1::uuid, $2, $3, $4, $5, $6::jsonb)
                    ON CONFLICT (operation_id) DO NOTHING
                    RETURNING TRUE
                    """,
                normalized_operation_id,
                tenant_id,
                dataset_id,
                operation,
                created_by,
                json.dumps(normalized_roles),
            )

            if not inserted:
                existing = await conn.fetchrow(
                    """
                    SELECT tenant_id, dataset_id, operation, status
                    FROM knowledge.kb_document_batch_operations
                    WHERE operation_id = $1::uuid
                    FOR SHARE
                    """,
                    normalized_operation_id,
                )
                if existing is None or (
                    str(existing["tenant_id"]) != tenant_id
                    or str(existing["dataset_id"]) != dataset_id
                    or str(existing["operation"]) != operation
                ):
                    raise ValueError("operation_id is already bound to another job")
                if str(existing["status"]) == "failed":
                    claiming = int(
                        await conn.fetchval(
                            "SELECT COUNT(*) FROM knowledge.kb_document_batch_items"
                            " WHERE operation_id = $1::uuid AND status = 'claiming'",
                            normalized_operation_id,
                        )
                        or 0
                    )
                    if claiming:
                        raise ValueError(
                            "cancelled document batch is still draining an owned claim"
                        )
                    await conn.execute(
                        """
                        UPDATE knowledge.kb_document_batch_items
                        SET status = 'pending', claimed_by = NULL,
                            claimed_at = NULL, completed_at = NULL,
                            error_code = NULL, error = NULL
                        WHERE operation_id = $1::uuid
                          AND status = 'failed'
                        """,
                        normalized_operation_id,
                    )
                    counts = await conn.fetchrow(
                        """
                        SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                               COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                               COUNT(*) FILTER (WHERE status = 'failed') AS failed,
                               COUNT(*) FILTER (WHERE status = 'pending') AS pending
                        FROM knowledge.kb_document_batch_items
                        WHERE operation_id = $1::uuid
                        """,
                        normalized_operation_id,
                    )
                    await conn.execute(
                        """
                        UPDATE knowledge.kb_document_batch_operations
                        SET status = CASE
                                WHEN $5 > 0 THEN 'pending'
                                WHEN $2 > 0 AND $3 = 0 AND $4 = 0 THEN 'completed'
                                WHEN $2 > 0 OR $3 > 0 THEN 'partial'
                                ELSE 'failed'
                            END,
                            queued_count = $2,
                            skipped_count = $3, failed_count = $4,
                            error = NULL, last_claimed_at = NULL,
                            completed_at = CASE WHEN $5 > 0 THEN NULL ELSE NOW() END,
                            updated_at = NOW()
                        WHERE operation_id = $1::uuid AND status = 'failed'
                        """,
                        normalized_operation_id,
                        int(counts["queued"] or 0),
                        int(counts["skipped"] or 0),
                        int(counts["failed"] or 0),
                        int(counts["pending"] or 0),
                    )
                # A deterministic producer replay reuses the original durable
                # item snapshot instead of enqueuing duplicate paid work.
                return await self._get_operation_on_connection(
                    conn,
                    operation_id=normalized_operation_id,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                )

            if all_documents:
                await conn.execute(
                    """
                        INSERT INTO knowledge.kb_document_batch_items (
                            operation_id, document_id, ordinal
                        )
                        SELECT $1::uuid, document_id,
                               ROW_NUMBER() OVER (ORDER BY created_at, document_id) - 1
                        FROM knowledge.documents
                        WHERE dataset_id = $2
                        ORDER BY created_at, document_id
                        """,
                    normalized_operation_id,
                    dataset_id,
                )
            else:
                await conn.executemany(
                    """
                        INSERT INTO knowledge.kb_document_batch_items (
                            operation_id, document_id, ordinal
                        ) VALUES ($1::uuid, $2, $3)
                        """,
                    [
                        (normalized_operation_id, document_id, ordinal)
                        for ordinal, document_id in enumerate(normalized_ids)
                    ],
                )

            total = int(
                await conn.fetchval(
                    """
                        SELECT COUNT(*)
                        FROM knowledge.kb_document_batch_items
                        WHERE operation_id = $1::uuid
                        """,
                    normalized_operation_id,
                )
                or 0
            )
            status = "completed" if total == 0 else "pending"
            await conn.execute(
                """
                    UPDATE knowledge.kb_document_batch_operations
                    SET total_count = $2,
                        status = $3,
                        completed_at = CASE WHEN $2 = 0 THEN NOW() ELSE NULL END,
                        updated_at = NOW()
                    WHERE operation_id = $1::uuid
                    """,
                normalized_operation_id,
                total,
                status,
            )

        result = await self.get_operation(
            operation_id=normalized_operation_id,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        if result is None:
            raise RuntimeError("document batch operation was not persisted")
        return result

    async def get_operation(
        self,
        *,
        operation_id: str,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            return await self._get_operation_on_connection(
                conn,
                operation_id=operation_id,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )

    async def _get_operation_on_connection(
        self,
        conn: Any,
        *,
        operation_id: str,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        row = await conn.fetchrow(
            """
            SELECT operation_id::text, tenant_id, dataset_id, operation,
                   status, total_count, queued_count, skipped_count,
                   failed_count, created_by, error, created_at, updated_at,
                   completed_at
            FROM knowledge.kb_document_batch_operations
            WHERE operation_id = $1::uuid
              AND tenant_id = $2
              AND dataset_id = $3
            """,
            operation_id,
            tenant_id,
            dataset_id,
        )
        if row is None:
            return None
        failures = await conn.fetch(
            """
            SELECT document_id, status, error_code, error
            FROM knowledge.kb_document_batch_items
            WHERE operation_id = $1::uuid
              AND status IN ('skipped', 'failed')
            ORDER BY ordinal
            LIMIT 1000
            """,
            operation_id,
        )
        result = dict(row)
        result["problem_items"] = [dict(item) for item in failures]
        result["problem_items_truncated"] = int(row["skipped_count"] or 0) + int(
            row["failed_count"] or 0
        ) > len(failures)
        return result

    async def cancel_operation(
        self,
        *,
        operation_id: str,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        """Fence new claims and retain a terminal cancellation receipt."""

        async with self._pool.acquire() as conn, conn.transaction():
            operation = await conn.fetchrow(
                """
                SELECT status, error
                FROM knowledge.kb_document_batch_operations
                WHERE operation_id = $1::uuid
                  AND tenant_id = $2 AND dataset_id = $3
                FOR UPDATE
                """,
                operation_id,
                tenant_id,
                dataset_id,
            )
            if operation is None:
                return None
            status = str(operation["status"])
            error = str(operation["error"] or "")
            if not (status == "failed" and error == "cancelled by operator"):
                if status in {"completed", "partial", "failed"}:
                    raise ValueError(
                        f"document batch in state '{status}' cannot be cancelled"
                    )
                await conn.execute(
                    """
                    UPDATE knowledge.kb_document_batch_items
                    SET status = 'failed', error_code = 'cancelled',
                        error = 'cancelled by operator', completed_at = NOW()
                    WHERE operation_id = $1::uuid AND status = 'pending'
                    """,
                    operation_id,
                )
                counts = await conn.fetchrow(
                    """
                    SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                           COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                           COUNT(*) FILTER (WHERE status = 'failed') AS failed
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                    """,
                    operation_id,
                )
                await conn.execute(
                    """
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = 'failed', queued_count = $2,
                        skipped_count = $3, failed_count = $4,
                        error = 'cancelled by operator',
                        completed_at = NOW(), updated_at = NOW()
                    WHERE operation_id = $1::uuid
                    """,
                    operation_id,
                    int(counts["queued"] or 0),
                    int(counts["skipped"] or 0),
                    int(counts["failed"] or 0),
                )
            return await self._get_operation_on_connection(
                conn,
                operation_id=operation_id,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )

    async def claim_next_item(
        self,
        *,
        worker_id: str,
        stale_after_seconds: int = 300,
    ) -> ClaimedDocumentBatchItem | None:
        """Claim one item and rotate the owning operation fairly.

        ``last_claimed_at`` moves after every claim. Ordering NULL first and
        then oldest timestamp means every runnable operation gets one turn
        before a large operation receives its next turn. Both operation and
        item rows use SKIP LOCKED so multiple worker replicas do not contend.
        """

        async with self._pool.acquire() as conn, conn.transaction():
            operation = await conn.fetchrow(
                """
                    SELECT op.*
                    FROM knowledge.kb_document_batch_operations AS op
                    WHERE op.status IN ('pending', 'running')
                      AND EXISTS (
                          SELECT 1
                          FROM knowledge.kb_document_batch_items AS item
                          WHERE item.operation_id = op.operation_id
                            AND (
                                item.status = 'pending'
                                OR (
                                    item.status = 'claiming'
                                    AND item.claimed_at < NOW()
                                        - make_interval(secs => $1)
                                )
                            )
                      )
                    ORDER BY op.last_claimed_at NULLS FIRST, op.created_at,
                             op.operation_id
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                    """,
                max(int(stale_after_seconds), 1),
            )
            if operation is None:
                return None

            item = await conn.fetchrow(
                """
                    SELECT document_id
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1
                      AND (
                          status = 'pending'
                          OR (
                              status = 'claiming'
                              AND claimed_at < NOW()
                                  - make_interval(secs => $2)
                          )
                      )
                    ORDER BY ordinal
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                    """,
                operation["operation_id"],
                max(int(stale_after_seconds), 1),
            )
            if item is None:
                return None

            await conn.execute(
                """
                    UPDATE knowledge.kb_document_batch_items
                    SET status = 'claiming', claimed_by = $3,
                        claimed_at = NOW(), error_code = NULL, error = NULL
                    WHERE operation_id = $1 AND document_id = $2
                    """,
                operation["operation_id"],
                item["document_id"],
                worker_id,
            )
            await conn.execute(
                """
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = 'running', last_claimed_at = NOW(),
                        updated_at = NOW()
                    WHERE operation_id = $1
                    """,
                operation["operation_id"],
            )

        raw_roles = operation["actor_roles"]
        if isinstance(raw_roles, str):
            raw_roles = json.loads(raw_roles)
        roles = tuple(str(role) for role in (raw_roles or ()) if str(role).strip())
        return ClaimedDocumentBatchItem(
            operation_id=str(operation["operation_id"]),
            tenant_id=str(operation["tenant_id"]),
            dataset_id=str(operation["dataset_id"]),
            operation=str(operation["operation"]),
            document_id=str(item["document_id"]),
            created_by=str(operation["created_by"]),
            actor_roles=roles,
        )

    async def complete_item(
        self,
        *,
        operation_id: str,
        document_id: str,
        status: str,
        error_code: str | None = None,
        error: str | None = None,
    ) -> None:
        if status not in _TERMINAL_ITEM_STATES:
            raise ValueError("invalid document batch item terminal status")
        safe_error = str(error or "").strip()[:2000] or None
        async with self._pool.acquire() as conn, conn.transaction():
            changed = await conn.fetchval(
                """
                    UPDATE knowledge.kb_document_batch_items
                    SET status = $3, error_code = $4, error = $5,
                        completed_at = NOW()
                    WHERE operation_id = $1::uuid
                      AND document_id = $2
                      AND status = 'claiming'
                    RETURNING 1
                    """,
                operation_id,
                document_id,
                status,
                str(error_code or "").strip()[:64] or None,
                safe_error,
            )
            if changed is None:
                return
            counts = await conn.fetchrow(
                """
                    SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                           COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                           COUNT(*) FILTER (WHERE status = 'failed') AS failed,
                           COUNT(*) FILTER (
                               WHERE status IN ('pending', 'claiming')
                           ) AS remaining
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                    """,
                operation_id,
            )
            queued = int(counts["queued"] or 0)
            skipped = int(counts["skipped"] or 0)
            failed = int(counts["failed"] or 0)
            remaining = int(counts["remaining"] or 0)
            if remaining:
                operation_status = "running"
            elif queued and not skipped and not failed:
                operation_status = "completed"
            elif queued or skipped:
                operation_status = "partial"
            else:
                operation_status = "failed"
            await conn.execute(
                """
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = CASE
                            WHEN error = 'cancelled by operator' THEN 'failed'
                            ELSE $2
                        END,
                        queued_count = $3, skipped_count = $4,
                        failed_count = $5, updated_at = NOW(),
                        completed_at = CASE
                            WHEN error = 'cancelled by operator'
                                THEN COALESCE(completed_at, NOW())
                            WHEN $6 = 0 THEN NOW()
                            ELSE NULL
                        END
                    WHERE operation_id = $1::uuid
                    """,
                operation_id,
                operation_status,
                queued,
                skipped,
                failed,
                remaining,
            )

    async def release_item(
        self,
        *,
        operation_id: str,
        document_id: str,
    ) -> None:
        """Return a transiently blocked claim to the durable fair queue."""

        async with self._pool.acquire() as conn, conn.transaction():
            await conn.execute(
                """
                UPDATE knowledge.kb_document_batch_items AS item
                SET status = CASE
                        WHEN operation.status = 'failed' THEN 'failed'
                        ELSE 'pending'
                    END,
                    claimed_by = NULL, claimed_at = NULL,
                    error_code = CASE
                        WHEN operation.status = 'failed' THEN 'cancelled'
                        ELSE NULL
                    END,
                    error = CASE
                        WHEN operation.status = 'failed' THEN operation.error
                        ELSE NULL
                    END,
                    completed_at = CASE
                        WHEN operation.status = 'failed' THEN NOW()
                        ELSE NULL
                    END
                FROM knowledge.kb_document_batch_operations AS operation
                WHERE item.operation_id = $1::uuid
                  AND item.document_id = $2
                  AND item.status = 'claiming'
                  AND operation.operation_id = item.operation_id
                """,
                operation_id,
                document_id,
            )
            await conn.execute(
                """
                UPDATE knowledge.kb_document_batch_operations AS operation
                SET failed_count = counts.failed, updated_at = NOW()
                FROM (
                    SELECT COUNT(*) FILTER (WHERE status = 'failed') AS failed
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                ) AS counts
                WHERE operation.operation_id = $1::uuid
                  AND operation.status = 'failed'
                """,
                operation_id,
            )
