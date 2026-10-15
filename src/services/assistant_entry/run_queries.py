"""Run/approval queries and the Runtime control-plane accessor for entry routes.

Moved from ``src/api/v1/assistant.py`` by ARC-01.  The ``assistant_runs`` and
``assistant_tool_approvals`` tables remain the run-state authority (ADR-004);
these helpers only read them so route modules do not carry raw SQL or import
each other.
"""

from __future__ import annotations

import logging
import uuid
from typing import Any

from fastapi import HTTPException, Request

logger = logging.getLogger(__name__)


def agent_runtime_control(request: Request) -> Any:
    """Return the Agent Runtime control-plane facade or fail closed."""
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(status_code=503, detail="Agent Runtime is unavailable")
    return control


async def fetch_approval_run_owner(
    database: Any,
    approval_id: str,
    tenant_id: str,
    user_id: str,
) -> Any | None:
    """Resolve the run that owns a pending approval; 503 on storage failure."""
    try:
        return await database.fetchrow(
            """
            SELECT r.engine, r.session_id, r.harness_thread_id
              FROM assistant_tool_approvals AS a
              JOIN assistant_runs AS r ON r.run_id = a.run_id
             WHERE a.approval_id = $1::uuid
               AND a.tenant_id = $2
               AND a.user_id = $3
            """,
            approval_id,
            tenant_id,
            user_id,
        )
    except Exception as exc:
        logger.warning(
            "Failed to resolve approval runtime owner",
            extra={"approval_id": approval_id},
            exc_info=exc,
        )
        raise HTTPException(
            status_code=503, detail="Approval runtime ownership unavailable"
        ) from exc


async def fetch_agent_runtime_run(
    database: Any,
    run_id: str,
    tenant_id: str,
    user_id: str,
) -> Any | None:
    """Fetch one Agent Runtime run row scoped to the calling user."""
    try:
        parsed_run_id = uuid.UUID(run_id)
    except ValueError:
        return None
    return await database.fetchrow(
        """
        SELECT run_id, tenant_id, user_id, session_id, status, engine,
               usage, error, started_at, finished_at, updated_at,
               harness_thread_id, harness_turn_id, kernel_revision,
               capability_revision
          FROM assistant_runs
         WHERE run_id = $1 AND tenant_id = $2 AND user_id = $3
           AND engine = 'agent_runtime'
        """,
        parsed_run_id,
        tenant_id,
        user_id,
    )


async def fetch_cancellable_run(
    database: Any,
    tenant_id: str,
    user_id: str,
    task_id: str,
) -> Any | None:
    """Map the public V1 task identifier to the owning Runtime run/turn."""
    try:
        run_uuid = uuid.UUID(task_id)
    except ValueError:
        run_uuid = None
    return await database.fetchrow(
        """
        SELECT run_id, session_id, harness_thread_id, harness_turn_id, status
          FROM assistant_runs
         WHERE tenant_id = $1 AND user_id = $2 AND engine = 'agent_runtime'
           AND (
                 ($3::uuid IS NOT NULL AND run_id = $3::uuid)
                 OR harness_turn_id = $4
               )
         ORDER BY started_at DESC
         LIMIT 1
        """,
        tenant_id,
        user_id,
        run_uuid,
        task_id,
    )
