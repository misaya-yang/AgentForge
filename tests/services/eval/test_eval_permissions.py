from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.v1 import eval as eval_routes
from src.api.v1.eval import get_eval_summary, list_eval_traces
from src.config.settings import Settings
from src.core.auth.permissions import Capability, accepted_permissions, check_capability
from src.core.auth.rbac import RBAC


def _request() -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    request.app.state.database = SimpleNamespace(enabled=True)
    request.state = SimpleNamespace(request_id="req-eval-perm")
    request.headers = {}
    request.url = SimpleNamespace(path="/api/v1/eval/traces")
    return request


def _auth(*permissions: str) -> AuthContext:
    return AuthContext(
        user_id="user-a",
        tenant_id="tenant-a",
        roles=["user"],
        permissions=list(permissions),
        is_authenticated=True,
    )


class _SummaryRepo:
    async def get_summary(self, **kwargs):
        return {
            "total_traces": 1,
            "failed_traces": 0,
            "succeeded_traces": 1,
            "assistant_traces": 1,
            "langgraph_traces": 0,
            "rag_traces": 0,
            "avg_latency_ms": 10,
            "p95_latency_ms": 10,
            "total_tokens": 0,
            "total_cost_cents": 0,
            "scored_traces": 0,
            "window_days": kwargs.get("days", 7),
        }

    async def list_traces(self, **_kwargs):
        return [], 0


def test_eval_capabilities_do_not_accept_usage_alias() -> None:
    assert "console:usage:view" not in accepted_permissions(Capability.GATEWAY_EVAL_TRACE_READ)
    assert "console:usage:view" not in accepted_permissions(Capability.GATEWAY_EVAL_RUN)


def test_check_capability_rejects_usage_only_for_eval_read() -> None:
    settings = Settings()
    rbac = RBAC(role_permissions=settings.rbac.roles)
    decision = check_capability(
        rbac=rbac,
        roles=["user"],
        permissions=["console:usage:view"],
        capability=Capability.GATEWAY_EVAL_TRACE_READ,
    )
    assert not decision.allowed


@pytest.mark.asyncio
async def test_eval_list_rejects_usage_only_permission(monkeypatch) -> None:
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: _SummaryRepo())

    with pytest.raises(HTTPException) as exc:
        await list_eval_traces(
            request=_request(),
            auth=_auth("console:usage:view"),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == Capability.GATEWAY_EVAL_TRACE_READ.value


@pytest.mark.asyncio
async def test_eval_summary_accepts_eval_view_permission(monkeypatch) -> None:
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: _SummaryRepo())

    result = await get_eval_summary(
        request=_request(),
        auth=_auth("console:eval:view"),
    )

    assert result.total_traces == 1
