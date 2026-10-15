from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from src.api.deps import AuthContext
from src.api.v1.config import get_service_config
from src.config.settings import Settings
from src.models.service import ServiceCapacityConfig, ServiceDefinition


class _Registry:
    def __init__(self, service: ServiceDefinition):
        self.service = service

    async def get(self, service_id: str):
        return self.service if service_id == self.service.service_id else None


@pytest.mark.asyncio
async def test_service_config_marks_priority_not_enforced() -> None:
    service = ServiceDefinition(service_id="agent", name="Agent")
    service.get_service_config().capacity = ServiceCapacityConfig(
        upstream_group="langgraph_agent",
        concurrency_limit=3,
        queue_max=0,
        queue_timeout_ms=100,
    )
    request = SimpleNamespace(
        state=SimpleNamespace(),
        app=SimpleNamespace(
            state=SimpleNamespace(
                settings=Settings(),
                registry=_Registry(service),
            )
        ),
    )
    auth = AuthContext(
        user_id="admin",
        tenant_id="default",
        roles=["developer"],
        permissions=["console:services:edit"],
        is_authenticated=True,
    )

    response = await get_service_config("agent", request=request, auth=auth)

    assert response["config"]["priority"]["enforced"] is False
    assert response["config"]["priority"]["scheduler"] == "not_configured"
    upstream = {
        budget["key"]: budget
        for budget in response["capacity_status"]["budgets"]
    }["upstream.langgraph_agent"]
    assert upstream["limit"] == 3
    assert upstream["queue_max"] == 0
    assert upstream["queue_timeout_ms"] == 100


def test_priority_controls_are_disabled_when_backend_is_not_enforcing() -> None:
    source = Path("web/src/components/ServiceConfigDialog.tsx").read_text()

    assert "priorityEnforced" in source
    assert "disabled={!priorityEnforced" in source
    assert "services.configDialog.priority.notEnforced" in source


def test_agent_capacity_and_load_balancing_are_frontend_configurable() -> None:
    source = Path("web/src/components/ServiceConfigDialog.tsx").read_text()

    assert "upstreamGroupFromBudget" in source
    assert "upstream_urls_text" in source
    assert "load_balance_strategy" in source
    assert "least_connections" in source
