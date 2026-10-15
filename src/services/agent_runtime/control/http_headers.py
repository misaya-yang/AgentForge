"""Internal Runtime headers with bounded trace correlation."""

from __future__ import annotations

from typing import TYPE_CHECKING

from ai_gateway_core.tracing import internal_http_headers

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane


def runtime_headers(
    plane: AgentRuntimeControlPlane,
    *,
    tenant_id: str = "",
    user_id: str = "",
    session_id: str = "",
    run_id: object = None,
    turn_id: object = None,
    execution_id: object = None,
) -> dict[str, str]:
    headers = {"x-ai-platform-internal-token": plane.runtime_internal_token}
    for name, value in (
        ("x-ai-tenant-id", tenant_id),
        ("x-ai-user-id", user_id),
        ("x-ai-session-id", session_id),
    ):
        if value:
            headers[name] = value
    return internal_http_headers(
        headers,
        run_id=run_id,
        turn_id=turn_id,
        execution_id=execution_id,
    )


__all__ = ["runtime_headers"]
