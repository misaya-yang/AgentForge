"""Approval lookup/decision forwarding to the Agent Runtime.

ARC-02 split of ``control_plane.py``.  Gateway only authenticates the scope
and forwards; the Runtime owns approval state.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .http_headers import runtime_headers
from .types import AgentRuntimeControlError

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane


async def get_approval(
    plane: AgentRuntimeControlPlane,
    *,
    approval_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
) -> dict[str, Any] | None:
    response = await plane.http_client.get(
        f"{plane.runtime_url}/internal/v1/approvals/{approval_id}",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        ),
    )
    if response.status_code == 404:
        return None
    if response.status_code >= 400:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_LOOKUP_FAILED", status_code=503
        )
    payload = response.json()
    return payload if isinstance(payload, dict) else None


async def decide_approval(
    plane: AgentRuntimeControlPlane,
    *,
    approval_id: str,
    approved: bool,
    reason: str | None = None,
    tenant_id: str,
    user_id: str,
    session_id: str,
) -> dict[str, Any]:
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/approvals/{approval_id}/decision",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        ),
        json={
            "decision": "approve" if approved else "reject",
            "reason": reason,
        },
    )
    if response.status_code >= 400:
        status = 409 if response.status_code in {400, 404, 409} else 503
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_DECISION_FAILED", status_code=status
        )
    payload = response.json()
    return (
        payload
        if isinstance(payload, dict)
        else {"approval_id": approval_id, "status": "consumed"}
    )


__all__ = ["decide_approval", "get_approval"]
