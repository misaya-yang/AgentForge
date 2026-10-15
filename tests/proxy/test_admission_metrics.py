from __future__ import annotations

import pytest

from src.core.gateway.admission import CapacityAdmissionController, CapacityRejected
from src.core.gateway.capacity import CapacityBudget
from src.core.observability.metrics import get_metrics


@pytest.mark.asyncio
async def test_admission_rejection_updates_metrics() -> None:
    service_id = "metrics-admission-service"
    controller = CapacityAdmissionController(per_tenant_default_share=1.0)
    budget = CapacityBudget(
        key="gateway.metrics",
        limit=1,
        queue_max=0,
        queue_timeout_ms=100,
        scope="gateway",
        source="test",
        enforced=True,
    )

    first = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-a",
        service_id=service_id,
        request_class="sync",
        request_id="req-1",
    )
    with pytest.raises(CapacityRejected):
        await controller.acquire(
            budgets=[budget],
            tenant_id="tenant-b",
            user_id="user-b",
            service_id=service_id,
            request_class="sync",
            request_id="req-2",
        )
    await first.release()

    metrics = get_metrics()
    assert (
        metrics._counters["admission_rejected_total"].get(
            service=service_id,
            reason="GATEWAY_CAPACITY_EXHAUSTED",
        )
        >= 1
    )
