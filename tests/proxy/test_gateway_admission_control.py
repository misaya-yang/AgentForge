from __future__ import annotations

import asyncio

import pytest

from src.core.gateway.admission import CapacityAdmissionController, CapacityRejected
from src.core.gateway.capacity import CapacityBudget


def _budget(
    key: str = "gateway.total_inflight",
    *,
    limit: int = 1,
    queue_max: int = 1,
    queue_timeout_ms: int = 200,
) -> CapacityBudget:
    return CapacityBudget(
        key=key,
        limit=limit,
        queue_max=queue_max,
        queue_timeout_ms=queue_timeout_ms,
        scope="gateway",
        source="test",
        enforced=True,
    )


@pytest.mark.asyncio
async def test_tenant_bulkhead_rejects_one_tenant_without_exhausting_global_capacity():
    controller = CapacityAdmissionController(per_tenant_default_share=0.5)
    budget = _budget(key="upstream.langgraph_agent", limit=4, queue_max=0)

    first = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-a",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-1",
    )
    second = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-b",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-2",
    )

    with pytest.raises(CapacityRejected) as exc_info:
        await controller.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-c",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-3",
        )

    rejection = exc_info.value
    assert rejection.status_code == 429
    assert rejection.code == "GATEWAY_TENANT_CAPACITY_EXHAUSTED"
    assert rejection.headers["X-RateLimit-Remaining"] == "0"
    assert rejection.headers["X-Gateway-Tenant-Id"] == "tenant-a"

    other_tenant = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-b",
        user_id="user-z",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-4",
    )

    await other_tenant.release()
    await second.release()
    await first.release()


@pytest.mark.asyncio
async def test_adaptive_load_shedder_rejects_normal_priority_when_p99_is_high():
    from src.core.gateway.admission import AdaptiveLoadShedder

    shedder = AdaptiveLoadShedder(window_seconds=30, normal_threshold_ms=10_000)
    for _ in range(20):
        shedder.record_latency(12_000)

    rejection = shedder.maybe_reject(
        service_id="agent-runtime",
        request_class="sync",
        priority=2,
    )

    assert rejection is not None
    assert rejection.status_code == 503
    assert rejection.code == "GATEWAY_LOAD_SHED"
    assert rejection.headers["Retry-After"] == "1"


def test_adaptive_load_shedder_never_rejects_critical_priority():
    from src.core.gateway.admission import AdaptiveLoadShedder

    shedder = AdaptiveLoadShedder(window_seconds=30, normal_threshold_ms=10_000)
    for _ in range(20):
        shedder.record_latency(60_000)

    assert (
        shedder.maybe_reject(
            service_id="gateway",
            request_class="health",
            priority=0,
        )
        is None
    )


@pytest.mark.asyncio
async def test_second_request_waits_until_capacity_is_released():
    controller = CapacityAdmissionController()
    budget = _budget(limit=1, queue_max=1, queue_timeout_ms=500)

    first = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-a",
        service_id="svc",
        request_class="sync",
        request_id="req-1",
    )
    blocked = asyncio.create_task(
        controller.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-b",
            service_id="svc",
            request_class="sync",
            request_id="req-2",
        )
    )
    await asyncio.sleep(0.03)
    assert not blocked.done()

    await first.release()
    second = await blocked
    assert second.queue_wait_ms > 0
    await second.release()


@pytest.mark.asyncio
async def test_capacity_rejection_has_stable_503_headers_and_code():
    controller = CapacityAdmissionController()
    budget = _budget(key="upstream.langgraph_agent", limit=1, queue_max=0)

    first = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-a",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-1",
    )

    with pytest.raises(CapacityRejected) as exc_info:
        await controller.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-b",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-2",
        )

    rejection = exc_info.value
    assert rejection.status_code == 503
    assert rejection.code == "GATEWAY_CAPACITY_EXHAUSTED"
    assert rejection.headers["X-Gateway-Capacity-Key"] == "upstream.langgraph_agent"
    assert "Retry-After" in rejection.headers
    assert "X-Gateway-Queue-Wait-Ms" in rejection.headers

    await first.release()


@pytest.mark.asyncio
async def test_capacity_context_releases_on_cancellation_path():
    controller = CapacityAdmissionController()
    budget = _budget(limit=1, queue_max=0)

    with pytest.raises(asyncio.CancelledError):
        async with await controller.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-a",
            service_id="svc",
            request_class="stream",
            request_id="req-stream",
        ):
            assert controller.snapshot()["gateway.total_inflight"]["inflight"] == 1
            raise asyncio.CancelledError()

    assert controller.snapshot()["gateway.total_inflight"]["inflight"] == 0


@pytest.mark.asyncio
async def test_release_many_continues_after_one_release_fails(monkeypatch):
    controller = CapacityAdmissionController()
    calls = []

    async def release_tenant(key):
        calls.append(("tenant", key))
        if key == "tenant-fail":
            raise RuntimeError("simulated tenant release failure")

    async def release_shared_pair(shared_key, tenant_key, member):
        calls.append(("shared-pair", shared_key, tenant_key, member))

    async def release_local(budget):
        calls.append(("local", budget.key))

    monkeypatch.setattr(controller, "_release_tenant_local", release_tenant)
    monkeypatch.setattr(controller, "_release_shared_pair", release_shared_pair)
    monkeypatch.setattr(controller, "_release_local", release_local)

    await controller._release_many(
        [_budget(key="local-good")],
        [("shared-good", "shared-tenant-good", "member")],
        [
            ("local", "tenant-good", ""),
            ("local", "tenant-fail", ""),
        ],
    )

    assert calls == [
        ("tenant", "tenant-fail"),
        ("tenant", "tenant-good"),
        ("shared-pair", "shared-good", "shared-tenant-good", "member"),
        ("local", "local-good"),
    ]
