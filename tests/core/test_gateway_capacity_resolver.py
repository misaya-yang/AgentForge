from __future__ import annotations

import pytest

from src.core.gateway.capacity import CapacityResolver


@pytest.mark.asyncio
async def test_default_uat_budgets_include_gateway_stream_upstream_and_provider():
    resolver = CapacityResolver()

    budgets = await resolver.resolve(
        tenant_id="tenant-a",
        service_id="local-2024-agent",
        request_class="stream",
        upstream_group=None,
        provider_id="google",
    )

    by_key = {budget.key: budget for budget in budgets}
    assert by_key["gateway.total_inflight"].limit == 64
    assert by_key["gateway.stream_inflight"].limit == 16
    assert by_key["upstream.langgraph_agent"].limit == 4
    assert by_key["provider.google_gemini"].limit == 4
    assert all(budget.enforced for budget in by_key.values())


@pytest.mark.asyncio
async def test_service_capacity_override_controls_upstream_budget():
    resolver = CapacityResolver()

    budgets = await resolver.resolve(
        tenant_id="tenant-a",
        service_id="local-2024-agent",
        request_class="sync",
        upstream_group=None,
        provider_id=None,
        service_config={
            "capacity": {
                "upstream_group": "langgraph_agent",
                "concurrency_limit": 3,
                "queue_max": 0,
                "queue_timeout_ms": 100,
            }
        },
    )

    upstream = {budget.key: budget for budget in budgets}["upstream.langgraph_agent"]
    assert upstream.limit == 3
    assert upstream.queue_max == 0
    assert upstream.queue_timeout_ms == 100
    assert upstream.source == "service_config"


@pytest.mark.asyncio
async def test_admin_reads_bypass_service_budget_but_not_gateway_process_budget():
    resolver = CapacityResolver()

    budgets = await resolver.resolve(
        tenant_id="tenant-a",
        service_id="local-2024-agent",
        request_class="admin",
        upstream_group=None,
        provider_id=None,
        is_admin_read=True,
    )

    keys = {budget.key for budget in budgets}
    assert "gateway.total_inflight" in keys
    assert "upstream.langgraph_agent" not in keys


@pytest.mark.asyncio
async def test_unknown_service_budget_is_reported_as_missing_not_silently_enforced():
    resolver = CapacityResolver()

    budgets = await resolver.resolve(
        tenant_id="tenant-a",
        service_id="unmapped-service",
        request_class="sync",
        upstream_group=None,
        provider_id=None,
    )

    missing = [budget for budget in budgets if budget.source_status == "missing"]
    assert missing
    assert all(not budget.enforced for budget in missing)
