"""PostgreSQL authority for Gateway-owned Local Node executions.

There is deliberately no in-process repository implementation. Devices,
grants, approvals, dispatch fences, and receipt cursors are all Postgres data.
"""

from __future__ import annotations

import json
import uuid
from collections.abc import Mapping
from typing import Any, Protocol

from ai_gateway_core.local_node import LocalNodeAction, LocalNodeReceipt, LocalNodeReceiptStatus


class LocalNodeRepositoryError(RuntimeError):
    """A durable scope, approval, or event transition was rejected."""


class LocalNodeExecutionRepository(Protocol):
    async def reserve_execution(self, action: LocalNodeAction, *, resource_binding: Mapping[str, Any], approval_status: str = "not_required") -> None: ...
    async def claim_dispatch(self, action: LocalNodeAction) -> bool: ...
    async def dispatch_fence_for(self, action: LocalNodeAction) -> str: ...
    async def append_receipt(self, receipt: LocalNodeReceipt) -> int: ...
    async def recover_receipts(self, action: LocalNodeAction, *, after_sequence: int = 0) -> list[LocalNodeReceipt]: ...
    async def mark_side_effect_unknown(self, action: LocalNodeAction) -> None: ...
    async def execution_result(self, action: LocalNodeAction) -> Mapping[str, Any] | None: ...


def _uuid(value: str, label: str) -> uuid.UUID:
    try:
        return uuid.UUID(value)
    except (ValueError, AttributeError, TypeError) as exc:
        raise LocalNodeRepositoryError(f"{label} identity is invalid") from exc


def _binding_matches(binding: Any, action: LocalNodeAction) -> bool:
    if not isinstance(binding, Mapping):
        return False
    return (
        binding.get("device_id") == action.scope.device_id
        and binding.get("channel_id") == action.scope.channel_id
        and (action.grant_id is None or binding.get("grant_id") == action.grant_id)
        and (action.grant_revision is None or int(binding.get("grant_revision", 0)) == action.grant_revision)
    )


class PostgresLocalNodeExecutionRepository:
    """Durable Local Node execution fence and append-only receipt store."""

    def __init__(self, pool: Any) -> None:
        self._pool = pool

    async def reserve_execution(self, action: LocalNodeAction, *, resource_binding: Mapping[str, Any], approval_status: str = "not_required") -> None:
        """Publish an execution before Worker dispatch; this is the Gateway fence input."""
        action.validate()
        if not isinstance(resource_binding, Mapping):
            raise LocalNodeRepositoryError("resource binding must be an object")
        if action.effect in {"write", "unknown"} and (not action.approval_id or approval_status not in {"pending", "approved"}):
            raise LocalNodeRepositoryError("write and unknown actions require approval")
        if action.effect == "read" and (action.approval_id is not None or approval_status != "not_required"):
            raise LocalNodeRepositoryError("read actions cannot carry approval")
        async with self._pool.acquire() as connection:
            try:
                await connection.execute(
                    """
                    INSERT INTO local_node_executions(
                      execution_id,lease_id,tenant_id,user_id,session_id,run_id,tool_call_id,
                      attempt_id,device_id,channel_id,grant_id,grant_revision,capability_id,
                      capability_revision,operation,arguments,arguments_sha256,idempotency_key,
                      effect,approval_id,approval_status,resource_binding,status
                    ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16::jsonb,$17,$18,
                             $19,$20,$21,$22::jsonb,CASE WHEN $21='pending' THEN 'awaiting_approval' ELSE 'published' END)
                    """,
                    _uuid(action.execution_id, "execution"), _uuid(action.lease_id, "lease"),
                    action.scope.tenant_id, action.scope.user_id, action.scope.session_id,
                    _uuid(action.run_id, "run"), action.tool_call_id, action.attempt_id,
                    action.scope.device_id, action.scope.channel_id,
                    _uuid(action.grant_id, "grant") if action.grant_id else None,
                    action.grant_revision, action.capability_id, action.capability_revision,
                    action.operation, json.dumps(dict(action.arguments)),
                    action.arguments_sha256.removeprefix("sha256:"), action.idempotency_key,
                    action.effect, _uuid(action.approval_id, "approval") if action.approval_id else None,
                    approval_status, json.dumps(dict(resource_binding)),
                )
            except Exception as exc:
                raise LocalNodeRepositoryError("execution reservation rejected") from exc

    async def claim_dispatch(self, action: LocalNodeAction) -> bool:
        action.validate()
        execution_id = _uuid(action.execution_id, "execution")
        run_id = _uuid(action.run_id, "run")
        lease_id = _uuid(action.lease_id, "lease")
        async with self._pool.acquire() as connection, connection.transaction():
            row = await connection.fetchrow(
                """
                SELECT * FROM local_node_executions
                 WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3
                   AND session_id=$4 AND run_id=$5 FOR UPDATE
                """, execution_id, action.scope.tenant_id, action.scope.user_id,
                action.scope.session_id, run_id,
            )
            if row is None:
                raise LocalNodeRepositoryError("execution scope mismatch")
            if (
                row["lease_id"] != lease_id
                or row["tool_call_id"] != action.tool_call_id
                or row["attempt_id"] != action.attempt_id
                or str(row["arguments_sha256"]) != action.arguments_sha256.removeprefix("sha256:")
                or row["capability_revision"] != action.capability_revision
                or row["capability_id"] != action.capability_id
                or row["effect"] != action.effect
                or row["idempotency_key"] != action.idempotency_key
                or not _binding_matches(row["resource_binding"], action)
                or row["status"] in {"succeeded", "failed", "cancelled", "timeout", "side_effect_unknown"}
            ):
                raise LocalNodeRepositoryError("execution binding rejected")
            if row["dispatch_fence"] is not None:
                return False
            if action.effect in {"write", "unknown"} and row["approval_status"] != "approved":
                raise LocalNodeRepositoryError("approval is required")
            fence = uuid.uuid4()
            claimed = await connection.fetchval(
                "SELECT claim_local_node_dispatch($1,$2,$3,$4,$5,$6)",
                execution_id, action.scope.tenant_id, action.scope.user_id,
                action.scope.session_id, fence, action.effect,
            )
            if not claimed:
                raise LocalNodeRepositoryError("dispatch fence rejected")
        return True

    async def dispatch_fence_for(self, action: LocalNodeAction) -> str:
        execution_id = _uuid(action.execution_id, "execution")
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                """
                SELECT dispatch_fence FROM local_node_executions
                 WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4
                """, execution_id, action.scope.tenant_id, action.scope.user_id,
                action.scope.session_id,
            )
        if row is None or row["dispatch_fence"] is None:
            raise LocalNodeRepositoryError("dispatch fence unavailable")
        return str(row["dispatch_fence"])

    async def append_receipt(self, receipt: LocalNodeReceipt) -> int:
        receipt.validate()
        execution_id = _uuid(receipt.execution_id, "receipt execution")
        fence = _uuid(receipt.dispatch_fence, "dispatch fence")
        event_id = uuid.uuid5(uuid.NAMESPACE_URL, f"local-node:{receipt.execution_id}:{receipt.sequence}:{receipt.event}")
        async with self._pool.acquire() as connection, connection.transaction():
            row = await connection.fetchrow(
                """
                SELECT dispatch_fence, device_id, channel_id, receipt_cursor
                  FROM local_node_executions
                 WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4
                 FOR UPDATE
                """, execution_id, receipt.tenant_id, receipt.user_id, receipt.session_id,
            )
            if row is None or row["dispatch_fence"] != fence or row["device_id"] != receipt.device_id:
                raise LocalNodeRepositoryError("receipt dispatch fence or owner mismatch")
            if receipt.channel_id is not None and row["channel_id"] != receipt.channel_id:
                raise LocalNodeRepositoryError("receipt channel mismatch")
            cursor = int(row["receipt_cursor"])
            if receipt.sequence <= cursor:
                duplicate = await connection.fetchval(
                    "SELECT 1 FROM local_node_receipts WHERE execution_id=$1 AND sequence=$2",
                    execution_id, receipt.sequence,
                )
                if duplicate:
                    return cursor
                raise LocalNodeRepositoryError("receipt cursor moved")
            if receipt.sequence != cursor + 1:
                raise LocalNodeRepositoryError("receipt sequence gap")
            await connection.execute(
                """
                INSERT INTO local_node_receipts(
                    execution_id,event_id,tenant_id,user_id,session_id,device_id,
                    channel_id,dispatch_fence,sequence,event,status,payload
                ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12::jsonb)
                """, execution_id, event_id, receipt.tenant_id, receipt.user_id,
                receipt.session_id, receipt.device_id, row["channel_id"], fence,
                receipt.sequence, receipt.event, receipt.status.value, json.dumps(dict(receipt.payload)),
            )
            await connection.execute(
                """
                INSERT INTO local_node_events(
                    event_id,execution_id,tenant_id,user_id,session_id,device_id,
                    channel_id,sequence,event,status,payload
                ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::jsonb)
                """, event_id, execution_id, receipt.tenant_id, receipt.user_id,
                receipt.session_id, receipt.device_id, row["channel_id"], receipt.sequence,
                receipt.event, receipt.status.value, json.dumps(dict(receipt.payload)),
            )
            await connection.execute(
                """
                UPDATE local_node_executions SET receipt_cursor=$2,status=$3,
                  result_summary=CASE WHEN $3 IN ('succeeded','failed') THEN $4::jsonb ELSE result_summary END,
                  terminal_at=CASE WHEN $3 IN ('succeeded','failed','cancelled','timeout','side_effect_unknown') THEN now() ELSE terminal_at END,
                  updated_at=now() WHERE execution_id=$1
                """, execution_id, receipt.sequence, receipt.status.value, json.dumps(dict(receipt.payload)),
            )
            await connection.execute(
                """
                UPDATE local_node_channels SET receipt_cursor=GREATEST(receipt_cursor,$2),last_seen_at=now()
                 WHERE channel_id=$1 AND tenant_id=$3 AND user_id=$4 AND device_id=$5
                """, row["channel_id"], receipt.sequence, receipt.tenant_id, receipt.user_id, receipt.device_id,
            )
        return receipt.sequence

    async def recover_receipts(self, action: LocalNodeAction, *, after_sequence: int = 0) -> list[LocalNodeReceipt]:
        execution_id = _uuid(action.execution_id, "execution")
        async with self._pool.acquire() as connection:
            rows = await connection.fetch(
                """
                SELECT execution_id,tenant_id,user_id,session_id,device_id,channel_id,
                       dispatch_fence,sequence,event,status,payload
                  FROM local_node_receipts
                 WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4
                   AND sequence>$5 ORDER BY sequence
                """, execution_id, action.scope.tenant_id, action.scope.user_id,
                action.scope.session_id, after_sequence,
            )
        return [LocalNodeReceipt(
            execution_id=str(row["execution_id"]), tenant_id=row["tenant_id"], user_id=row["user_id"],
            session_id=row["session_id"], device_id=row["device_id"], channel_id=row["channel_id"],
            dispatch_fence=str(row["dispatch_fence"]), sequence=int(row["sequence"]),
            status=LocalNodeReceiptStatus(row["status"]), event=row["event"], payload=row["payload"],
        ) for row in rows]

    async def mark_side_effect_unknown(self, action: LocalNodeAction) -> None:
        execution_id = _uuid(action.execution_id, "execution")
        async with self._pool.acquire() as connection:
            result = await connection.execute(
                """
                UPDATE local_node_executions SET status='side_effect_unknown',terminal_at=now(),updated_at=now()
                 WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4
                   AND dispatch_fence IS NOT NULL
                   AND status NOT IN ('succeeded','failed','cancelled','timeout','side_effect_unknown')
                """, execution_id, action.scope.tenant_id, action.scope.user_id, action.scope.session_id,
            )
        if result.endswith("0"):
            raise LocalNodeRepositoryError("side effect transition rejected")

    async def execution_result(self, action: LocalNodeAction) -> Mapping[str, Any] | None:
        execution_id = _uuid(action.execution_id, "execution")
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                """
                SELECT status,result_summary,receipt_cursor,dispatch_fence,device_id,channel_id,arguments_sha256
                  FROM local_node_executions WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3
                   AND session_id=$4 AND run_id=$5
                """, execution_id, action.scope.tenant_id, action.scope.user_id,
                action.scope.session_id, _uuid(action.run_id, "run"),
            )
        if row is None or row["device_id"] != action.scope.device_id or row["channel_id"] != action.scope.channel_id:
            raise LocalNodeRepositoryError("execution scope mismatch")
        if str(row["arguments_sha256"]) != action.arguments_sha256.removeprefix("sha256:"):
            raise LocalNodeRepositoryError("execution arguments mismatch")
        if row["dispatch_fence"] is None or row["status"] not in {"succeeded","failed","cancelled","timeout","side_effect_unknown"}:
            return None
        result = row["result_summary"] if isinstance(row["result_summary"], Mapping) else {}
        return {**dict(result), "status": row["status"], "last_sequence": int(row["receipt_cursor"])}
