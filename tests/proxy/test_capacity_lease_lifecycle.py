import asyncio

import pytest

from src.core.gateway.admission import (
    AdaptiveLoadShedder,
    CapacityAdmissionController,
    CapacityLease,
    CapacityRejected,
)
from src.core.gateway.capacity import CapacityBudget, CapacityResolver


def budget(key, limit=1, *, shared=False):
    return CapacityBudget(key, limit, 8, 500, "service", "test", True, shared=shared)


async def acquire(controller, budgets, request_id="req"):
    return await controller.acquire(budgets=budgets, tenant_id="t", user_id="u", service_id="s",
                                    request_class="stream", request_id=request_id)


@pytest.mark.asyncio
async def test_cancel_after_first_budget_releases_partial_acquisition():
    controller = CapacityAdmissionController(per_tenant_default_share=1)
    held = await acquire(controller, [budget("second")], "held")
    waiter = asyncio.create_task(acquire(controller, [budget("first"), budget("second")]))
    for _ in range(20):
        await asyncio.sleep(0)
        if controller.snapshot().get("second", {}).get("queue_depth"):
            break
    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter
    assert controller.snapshot()["first"]["inflight"] == 0
    assert controller.snapshot()["second"]["queue_depth"] == 0
    await held.release()


@pytest.mark.asyncio
async def test_cancelled_release_waits_for_owned_cleanup_and_is_idempotent():
    started, finish = asyncio.Event(), asyncio.Event()
    calls = []

    async def release():
        started.set()
        await finish.wait()
        calls.append("done")

    lease = CapacityLease(budget_keys=["test"], queue_wait_ms=0, release=release)
    task = asyncio.create_task(lease.release())
    await started.wait()
    task.cancel()
    await asyncio.sleep(0)
    assert not task.done()
    finish.set()
    with pytest.raises(asyncio.CancelledError):
        await task
    await lease.release()
    assert calls == ["done"]


@pytest.mark.asyncio
async def test_run_provider_and_sse_use_separate_resources():
    resolver = CapacityResolver()
    resolved = {}
    for kind in ("run", "provider", "sse"):
        rows = await resolver.resolve(tenant_id="t", service_id="agent-runtime", request_class="stream",
                                      upstream_group=None, provider_id="google", resource_kind=kind)
        resolved[kind] = {row.key for row in rows if row.enforced}
    assert not (resolved["run"] & resolved["provider"])
    assert resolved["sse"] == {"gateway.sse_inflight"}


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["run", "provider", "capability", "job", "sse"])
async def test_workflow_lifetime_does_not_poison_http_load_shedding(kind):
    shedder = AdaptiveLoadShedder(normal_threshold_ms=10)
    controller = CapacityAdmissionController(load_shedder=shedder, per_tenant_default_share=1)
    resolver = CapacityResolver()
    budgets = await resolver.resolve(tenant_id="t", service_id=f"agent-runtime-{kind}",
                                      request_class="sync", upstream_group=None, provider_id=None,
                                      resource_kind=kind)
    first = await acquire(controller, budgets, "first-turn")
    await first.release()
    assert shedder.p99_latency_ms() is None
    shedder.record_latency(60_000)
    cancelled = await acquire(controller, budgets, "cancelled-turn")
    await cancelled.release()
    following = await acquire(controller, budgets, "following-turn")
    await following.release()
    assert all(row["inflight"] == 0 for row in controller.snapshot().values())
    # The same controller still protects ordinary HTTP requests.
    ordinary = await resolver.resolve(tenant_id="t", service_id="http-service", request_class="sync",
                                      upstream_group=None, provider_id=None)
    with pytest.raises(CapacityRejected) as failure:
        await acquire(controller, ordinary, "ordinary-http")
    assert failure.value.code == "GATEWAY_LOAD_SHED"


@pytest.mark.asyncio
async def test_unmapped_service_explicit_limit_is_enforced_and_updates():
    resolver = CapacityResolver()
    for limit in (2, 1):
        rows = await resolver.resolve(tenant_id="t", service_id="custom", request_class="sync",
                                      upstream_group=None, provider_id=None,
                                      service_config={"concurrency_limit": limit})
        own = next(row for row in rows if row.key == "service.custom")
        assert own.enforced and own.limit == limit


class RenewableRedis:
    def __init__(self):
        self.values = {}
        self.renewals = 0
        self.lose = False

    async def eval(self, script, count, *values):
        keys, args = values[:count], values[count:]
        if "ZSCORE" in script:
            self.renewals += 1
            if self.lose or any(self.values.get(key, {}).get(args[2], 0) <= args[0] for key in keys):
                return 0
            for key in keys:
                self.values[key][args[2]] = args[1]
            return 1
        if "PEXPIRE" not in script:
            for key in keys:
                self.values.setdefault(key, {}).pop(args[0], None)
            return len(keys)
        counts = []
        for key, limit in zip(keys, args[4:], strict=True):
            active = self.values.setdefault(key, {})
            for member, expiry in list(active.items()):
                if expiry <= args[0]:
                    del active[member]
            counts.append(len(active))
            if len(active) < limit:
                active[args[2]] = args[1]
        return counts


@pytest.mark.asyncio
async def test_shared_lease_renews_and_loss_cancels_owner_before_release():
    redis = RenewableRedis()
    controller = CapacityAdmissionController(redis_client=redis, per_tenant_default_share=1)
    controller.lease_ttl_ms = 90
    ready = asyncio.Event()

    async def run():
        lease = await acquire(controller, [budget("shared", shared=True)])
        async with lease:
            ready.set()
            await asyncio.sleep(10)

    owner = asyncio.create_task(run())
    await ready.wait()
    await asyncio.sleep(0.14)
    assert redis.renewals >= 2 and not owner.done()
    redis.lose = True
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(owner, timeout=0.2)
    assert controller.snapshot()["shared"]["inflight"] == 0
    assert all(not active for active in redis.values.values())
