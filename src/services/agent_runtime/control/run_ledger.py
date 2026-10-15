"""Terminal run ledger writes: lease revocation and assistant_runs status.

ARC-02 split of ``control_plane.py``.
"""

from __future__ import annotations

import json
import uuid
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane


# Recorded model-call totals are not proof of provider-reported usage: existing
# chat adapters persist estimates in the same fields. Keep this projection under
# metadata; never settle missing usage or overwrite terminal execution facts.
TERMINAL_USAGE_PROJECTION_SQL = r"""
WITH target AS MATERIALIZED (
    SELECT r.run_id,r.tenant_id,r.user_id,r.session_id,r.harness_thread_id,
           r.runtime_snapshot_id,r.status,r.usage
    FROM assistant_runs r
    JOIN assistant_runtime_snapshots s ON s.snapshot_id=r.runtime_snapshot_id
      AND s.run_id=r.run_id AND s.runtime_thread_id=r.harness_thread_id
      AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id
    WHERE r.run_id=$1::uuid AND r.engine='agent_runtime'
      AND r.status IN ('succeeded','failed','cancelled')
      AND jsonb_typeof(r.usage)='object'
      AND (NOT (r.usage ? 'metadata') OR jsonb_typeof(r.usage->'metadata')='object')
      AND EXISTS (
          SELECT 1 FROM assistant_runtime_items i
          WHERE i.runtime_thread_id=r.harness_thread_id
            AND i.kernel_thread_id=r.harness_thread_id
            AND i.tenant_id=r.tenant_id AND i.user_id=r.user_id AND i.session_id=r.session_id
            AND i.turn_id=r.run_id::text AND i.status=r.status
            AND i.payload->>'schema_version'='assistant-turn-contract/v1'
            AND i.payload->'data'->>'run_id'=r.run_id::text
            AND i.payload->'data'->>'thread_id'=r.harness_thread_id::text
            AND i.payload->'data'->>'session_id'=r.session_id
            AND i.payload->'data'->>'status'=r.status
            AND i.event_type='compat/v1/'||(i.payload->>'event_type')
            AND ((r.status='succeeded' AND i.event_type='compat/v1/run_finished')
              OR (r.status='failed' AND i.event_type='compat/v1/run_error')
              OR (r.status='cancelled' AND i.event_type IN ('compat/v1/run_error','compat/v1/cancelled')))
      )
), observed AS (
    SELECT t.run_id,COUNT(c.call_id)::bigint AS total_calls,
           COUNT(c.call_id) FILTER (WHERE c.status='completed')::bigint AS completed_calls,
           COUNT(c.call_id) FILTER (WHERE c.status IN ('reserved','dispatched'))::bigint AS pending_calls,
           COUNT(c.call_id) FILTER (WHERE c.status='unknown')::bigint AS unknown_calls,
           COUNT(c.call_id) FILTER (WHERE c.dispatched_at IS NOT NULL)::bigint AS dispatched_calls,
           COUNT(c.call_id) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS recorded_calls,
           SUM(c.input_tokens) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS input_tokens,
           SUM(c.output_tokens) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS output_tokens,
           SUM(c.cost_microusd) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS cost_microusd
    FROM target t LEFT JOIN assistant_runtime_model_calls c
      ON c.run_id=t.run_id AND c.snapshot_id=t.runtime_snapshot_id
     AND c.tenant_id=t.tenant_id AND c.user_id=t.user_id AND c.session_id=t.session_id
    GROUP BY t.run_id
), projection AS (
    SELECT t.*,t.usage || jsonb_build_object(
        'metadata',COALESCE(t.usage->'metadata','{}'::jsonb) || jsonb_build_object(
            'runtime_model_usage',jsonb_build_object(
                'source','persisted_model_call_ledger',
                'provider_usage_verified',false,
                'settlement','not_established',
                'reason','provider_reported_and_estimated_usage_not_distinguished',
                'total_calls',o.total_calls,'completed_calls',o.completed_calls,
                'recorded_calls',o.recorded_calls,'pending_calls',o.pending_calls,
                'unknown_calls',o.unknown_calls,'dispatched_calls',o.dispatched_calls,
                'recorded_totals_complete',o.recorded_calls>0
                    AND o.recorded_calls=o.completed_calls AND o.pending_calls=0
                    AND o.unknown_calls=0 AND o.dispatched_calls=o.recorded_calls,
                'recorded_totals',CASE WHEN o.recorded_calls>0 THEN jsonb_build_object(
                    'input_tokens',o.input_tokens,'output_tokens',o.output_tokens,
                    'cost_microusd',o.cost_microusd
                ) ELSE NULL END
            )
        )
    ) AS next_usage
    FROM target t JOIN observed o USING(run_id)
)
UPDATE assistant_runs r SET usage=p.next_usage,updated_at=NOW()
FROM projection p
WHERE r.run_id=p.run_id AND r.engine='agent_runtime' AND r.status=p.status
  AND r.tenant_id=p.tenant_id AND r.user_id=p.user_id AND r.session_id=p.session_id
  AND r.harness_thread_id=p.harness_thread_id AND r.runtime_snapshot_id=p.runtime_snapshot_id
  AND r.usage IS NOT DISTINCT FROM p.usage AND r.usage IS DISTINCT FROM p.next_usage
"""

async def complete_run(
    plane: AgentRuntimeControlPlane, run_id: uuid.UUID, terminal_status: str
) -> None:
    status = (
        "succeeded"
        if terminal_status == "succeeded"
        else "cancelled"
        if terminal_status == "cancelled"
        else "failed"
    )
    usage = await plane.database.fetchrow(
        """
        SELECT COALESCE(SUM(input_tokens), 0)::bigint AS input_tokens,
               COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens,
               COALESCE(SUM(cost_microusd), 0)::bigint AS cost_microusd
          FROM assistant_runtime_model_calls
         WHERE run_id = $1 AND status = 'completed'
        """,
        run_id,
    )
    snapshot = await plane.database.fetchrow(
        "SELECT snapshot AS payload FROM assistant_runtime_snapshots WHERE run_id=$1", run_id,
    )
    payload = (snapshot or {}).get("payload") or {}
    if isinstance(payload, str):
        payload = json.loads(payload)
    usage_record = dict(usage or {})
    pricing_snapshot = payload.get("pricing", {}).get("snapshot") if isinstance(payload, dict) else None
    if pricing_snapshot:
        usage_record["metadata"] = {"pricing_snapshot": pricing_snapshot}
    await plane.database.execute(
        """
        UPDATE assistant_runtime_model_leases
           SET status = 'revoked', revoked_at = NOW(),
               revoked_reason = $2, updated_at = NOW()
         WHERE run_id = $1 AND status = 'active'
        """,
        run_id,
        f"turn_{status}",
    )
    await plane.database.execute(
        """
        UPDATE assistant_runs
           SET status = $2, usage = $3::jsonb, finished_at = NOW(), updated_at = NOW()
         WHERE run_id = $1 AND engine = 'agent_runtime' AND status = 'running'
        """,
        run_id,
        status,
        json.dumps(usage_record, separators=(",", ":")),
    )

    await plane.database.execute(TERMINAL_USAGE_PROJECTION_SQL, run_id)


async def fail_run(
    plane: AgentRuntimeControlPlane,
    run_id: uuid.UUID,
    snapshot_id: uuid.UUID,
    reason: str,
) -> None:
    await plane.database.execute(
        """
        UPDATE assistant_runtime_model_leases
           SET status = 'revoked', revoked_at = NOW(), revoked_reason = $3, updated_at = NOW()
         WHERE run_id = $1 AND snapshot_id = $2 AND status = 'active'
        """,
        run_id,
        snapshot_id,
        reason,
    )
    await plane.database.execute(
        """
        UPDATE assistant_runs
           SET status = 'failed', error = $2, finished_at = NOW(), updated_at = NOW()
         WHERE run_id = $1 AND engine = 'agent_runtime' AND status = 'running'
        """,
        run_id,
        reason,
    )


__all__ = ["complete_run", "fail_run"]
