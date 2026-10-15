"""Bind Runtime Trace dimensions from the original authorized session pin."""
from __future__ import annotations

from typing import Any


async def bind_runtime_trace_dimensions(
    connection: Any, *, trace_id: str, tenant_id: str, user_id: str, session_id: str,
) -> None:
    # Metadata and caller-supplied Agent IDs are never attribution authority.
    await connection.execute(
        """UPDATE agent_traces t
           SET agent_id = a.agent_id, agent_version_id = a.agent_version_id,
               agent_draft_revision = a.agent_draft_revision,
               publication_id = a.publication_id, channel = a.channel,
               runtime_fingerprint = a.runtime_fingerprint,
               agent_spec_hash = a.agent_spec_hash
           FROM assistant_runs r
           JOIN assistant_runtime_snapshots s
             ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
            AND s.user_id = r.user_id AND s.session_id = r.session_id
            AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
           JOIN assistant.sessions a
             ON a.session_id = r.session_id AND a.tenant_id = r.tenant_id
            AND a.user_id = r.user_id
            AND a.agent_id::text = s.snapshot->'agent_spec'->>'agentId'
            AND a.agent_version_id::text IS NOT DISTINCT FROM
                s.snapshot->'agent_spec'->>'agentVersionId'
           WHERE t.trace_id = $1::uuid AND t.tenant_id = $2 AND t.user_id = $3
             AND t.session_id = $4 AND t.trace_family = 'assistant'
             AND t.run_id = r.run_id::text AND t.trace_id = r.run_id
             AND t.tenant_id = r.tenant_id AND t.user_id = r.user_id
             AND t.session_id = r.session_id AND r.engine = 'agent_runtime'
             AND t.agent_id IS NULL AND a.agent_id IS NOT NULL""",
        trace_id, tenant_id, user_id, session_id,
    )
