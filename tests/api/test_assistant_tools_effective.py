"""Tenant catalog is distinct from the session's resolved Runtime tools."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.v1._assistant_routes import tools as routes
from src.core.auth.user_resolver import UserContext


@pytest.mark.asyncio
async def test_session_tools_separate_catalog_and_current_effective(monkeypatch) -> None:
    records = (
        {"name": "todo_write", "description": "Plan", "input_schema": {}, "effect": "write"},
        {"name": "local_node_catalog", "description": "Device tools", "input_schema": {"required": ["device_id"]}, "effect": "read"},
    )
    monkeypatch.setattr(routes, "load_assistant_capability_catalog", lambda: (1, records))
    monkeypatch.setattr(routes, "load_gateway_assistant_policies", AsyncMock(return_value={}))
    monkeypatch.setattr(routes, "project_assistant_tools", lambda *_args, **_kwargs: [
        {"name": "todo_write"}, {"name": "local_node_catalog"},
    ])
    session = SimpleNamespace(user_id="user-a", tenant_id="tenant-a", config={"selected_model": "qwen"})
    monkeypatch.setattr(routes, "get_session_manager", lambda _request: SimpleNamespace(get=AsyncMock(return_value=session)))

    async def resolve(readonly, **_scope):
        readonly["tools"] = [{"name": "local_node_catalog", "effect": "read", "approval": "never"}]

    control = SimpleNamespace(
        model_service=SimpleNamespace(get_model=AsyncMock(return_value={"capability_revision": 2})),
        _fetch_capability_catalog=resolve,
    )
    database = SimpleNamespace(fetchrow=AsyncMock(return_value=None))
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        database=database, agent_runtime_control=control,
    )))
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    result = await routes.get_session_tools("session-a", request, user)

    assert [item["name"] for item in result["platform_catalog"]] == ["todo_write", "local_node_catalog"]
    assert [item["name"] for item in result["next_turn_estimate"]["tools"]] == ["local_node_catalog"]
    assert result["next_turn_estimate"]["tools"][0]["device_required"] is True
    assert result["last_run_pinned"] is None


@pytest.mark.asyncio
async def test_other_users_session_has_no_tool_oracle(monkeypatch) -> None:
    session = SimpleNamespace(user_id="someone-else", tenant_id="tenant-a")
    monkeypatch.setattr(routes, "get_session_manager", lambda _request: SimpleNamespace(get=AsyncMock(return_value=session)))
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    with pytest.raises(HTTPException) as exc:
        await routes.get_session_tools(
            "session-a", request,
            UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True),
        )
    assert exc.value.status_code == 404
