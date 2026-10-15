from __future__ import annotations

import pytest


@pytest.mark.asyncio
async def test_config_status_reports_capacity_and_rate_limit_enforcement(
    async_client, test_app, test_settings, valid_jwt_admin
):
    from types import SimpleNamespace

    from src.core.auth.rbac import RBAC

    test_app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=test_settings.rbac.roles))
    test_app.state.redis.enabled = False

    response = await async_client.get(
        "/api/v1/config/status",
        headers={"Authorization": f"Bearer {valid_jwt_admin}"},
    )

    assert response.status_code == 200
    data = response.json()
    assert data["rate_limiting"]["configured_rules_count"] >= 0
    assert "runtime_enabled" in data["rate_limiting"]
    assert data["rate_limiting"]["enforcement_source"] in {"settings", "runtime", "disabled"}
    assert data["capacity"]["mode"] == "single-node"
    assert data["capacity"]["gateway_instance_id"]
    assert any(budget["key"] == "upstream.langgraph_agent" for budget in data["capacity"]["budgets"])
    assert all("source_status" in budget for budget in data["capacity"]["budgets"])


@pytest.mark.asyncio
async def test_non_admin_cannot_read_capacity_status(
    async_client, test_app, test_settings, valid_jwt_user_a
):
    from types import SimpleNamespace

    from src.core.auth.rbac import RBAC

    test_app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=test_settings.rbac.roles))
    test_app.state.redis.enabled = False

    response = await async_client.get(
        "/api/v1/config/status",
        headers={"Authorization": f"Bearer {valid_jwt_user_a}"},
    )

    assert response.status_code == 403
