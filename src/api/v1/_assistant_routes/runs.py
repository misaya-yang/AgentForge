"""Assistant run-surface routes: approvals, run status, resume, cancellation.

ARC-01 split of ``src/api/v1/assistant.py``.  Run state stays in
``assistant_runs`` (ADR-004) and execution authority stays with the Agent
Runtime; these routes only adapt the public V1 surface onto the control plane.
"""

from __future__ import annotations

import contextlib
import json
import logging

from fastapi import APIRouter, Depends, HTTPException, Request

from ....core.auth.user_resolver import UserContext
from ....services.agent_runtime import AgentRuntimeControlError
from ....services.assistant_entry.approval_preview import owner_approval_preview
from ....services.assistant_entry.run_queries import (
    agent_runtime_control,
    fetch_agent_runtime_run,
    fetch_approval_run_owner,
    fetch_cancellable_run,
)
from ....services.assistant_entry.source_access import require_conversation_source_access
from ...deps import get_user_context
from .schemas import (
    ApprovalRequest,
    ApprovalResponse,
    ResumeRequest,
    ResumeResponse,
    RunStatusResponse,
    TaskCancelRequest,
    TaskCancelResponse,
)

router = APIRouter()
logger = logging.getLogger(__name__)


@router.post("/approvals/{approval_id}", response_model=ApprovalResponse)
async def approve_tool_call(
    approval_id: str,
    body: ApprovalRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> ApprovalResponse:
    """Gateway-owned approval decision for a Runtime tool call. Approval state is tenant- and session-scoped."""
    # The handler projects that decision through the Agent Runtime control
    # plane while retaining the public Gateway-owned approval contract.
    database = getattr(request.app.state, "database", None)
    if database is not None:
        run = await fetch_approval_run_owner(
            database,
            approval_id,
            user.tenant_id,
            user.user_id,
        )
        if not run:
            raise HTTPException(status_code=404, detail="Approval not found")
        if run and str(run.get("engine") or "") == "agent_runtime":
            control = agent_runtime_control(request)
            session_id = str(run.get("session_id") or "")

            try:
                approval = await control.get_approval(
                    approval_id=approval_id,
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    session_id=session_id,
                )
            except AgentRuntimeControlError as exc:
                raise HTTPException(status_code=exc.status_code, detail=exc.code) from exc
            if approval is None:
                raise HTTPException(status_code=404, detail="Approval not found")
            payload = body.model_dump()
            if payload["approved"]:
                await require_conversation_source_access(request, user, session_id, for_execution=True)
                preview = await owner_approval_preview(
                    database,
                    approval_id=approval_id,
                    runtime_thread_id=str(run.get("harness_thread_id") or ""),
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    session_id=session_id,
                    runtime_summary=approval,
                )
                if not preview["can_approve"]:
                    raise HTTPException(status_code=409, detail={"code": "APPROVAL_ACTION_UNVERIFIED"})
            try:
                await control.decide_approval(
                    approval_id=approval_id,
                    approved=bool(payload["approved"]),
                    reason=payload.get("reason"),
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    session_id=session_id,
                )
            except AgentRuntimeControlError as exc:
                raise HTTPException(status_code=exc.status_code, detail=exc.code) from exc
            return ApprovalResponse(
                approval={
                    **approval,
                    "status": "approved" if payload["approved"] else "rejected",
                    "approved": payload["approved"],
                    "reason": payload.get("reason"),
                }
            )
    raise HTTPException(
        status_code=409,
        detail={
            "code": "AGENT_RUNTIME_ONLY",
            "message": "Approval state is owned by the Agent Runtime.",
        },
    )


@router.get("/runs/{run_id}", response_model=RunStatusResponse)
async def get_run_status(
    run_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> RunStatusResponse:
    """Thin proxy — run state lives in ``assistant_runs`` (ADR-004)."""
    database = getattr(request.app.state, "database", None)
    if database is not None:
        row = await fetch_agent_runtime_run(
            database,
            run_id,
            user.tenant_id,
            user.user_id,
        )
        if row is not None:
            payload = dict(row)
            # Runtime error text is an internal diagnostic channel. The one
            # restart outcome with a product-specific explanation is exposed
            # as a bounded reason, without forwarding arbitrary error text.
            payload["terminal_reason"] = (
                "runtime_restart_interrupted"
                if payload.get("status") == "cancelled"
                and payload.get("error") == "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"
                else None
            )
            payload["error"] = None
            usage = payload.get("usage")
            if isinstance(usage, str):
                with contextlib.suppress(json.JSONDecodeError):
                    payload["usage"] = json.loads(usage)
            return RunStatusResponse(run=payload)
    raise HTTPException(status_code=404, detail="Run not found")


@router.post("/runs/{run_id}/resume", response_model=ResumeResponse)
async def prepare_run_resume(
    run_id: str,
    request: Request,
    body: ResumeRequest | None = None,
    user: UserContext = Depends(get_user_context),
) -> ResumeResponse:
    """Reject legacy resume instead of dispatching to a second AgentLoop."""
    from ...deps import enforce_rate_limit

    await enforce_rate_limit(request, user, operation="assistant_resume")
    raise HTTPException(
        status_code=409,
        detail={
            "code": "AGENT_RUNTIME_ONLY",
            "message": "Resume is owned by the Agent Runtime; use the V2 turn contract.",
        },
    )


@router.post("/tasks/{task_id}/cancel", response_model=TaskCancelResponse)
async def cancel_task(
    task_id: str,
    request: Request,
    body: TaskCancelRequest | None = None,
    user: UserContext = Depends(get_user_context),
) -> TaskCancelResponse:
    """Map the V1 task identifier to the owning Runtime run/turn interrupt."""

    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(status_code=503, detail="Agent Runtime is unavailable")
    row = await fetch_cancellable_run(database, user.tenant_id, user.user_id, task_id)
    if not row:
        raise HTTPException(status_code=404, detail="Task not found")
    status = str(row.get("status") or "")
    session_id = str(row.get("session_id") or "")
    if status not in {"running", "pending", "queued", "awaiting_approval"}:
        return TaskCancelResponse(
            task_id=task_id,
            session_id=session_id,
            cancelled=status == "cancelled",
            message="Task is already complete",
        )
    thread_id = str(row.get("harness_thread_id") or "")
    turn_id = str(row.get("harness_turn_id") or row.get("run_id") or "")
    if not thread_id or not turn_id or not session_id:
        raise HTTPException(status_code=503, detail="Agent Runtime task mapping is unavailable")
    control = agent_runtime_control(request)
    try:
        await control.interrupt_turn(
            runtime_thread_id=thread_id,
            turn_id=turn_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            reason=(body.reason if body is not None else "client_interrupt"),
        )
    except AgentRuntimeControlError as exc:
        raise HTTPException(status_code=exc.status_code, detail=exc.code) from None
    except Exception:
        raise HTTPException(status_code=503, detail="Agent Runtime interrupt failed") from None
    return TaskCancelResponse(
        task_id=task_id,
        session_id=session_id,
        cancelled=True,
        message="Cancellation requested",
    )
