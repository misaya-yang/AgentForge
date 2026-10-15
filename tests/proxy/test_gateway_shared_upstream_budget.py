from __future__ import annotations

import pytest

from src.core.gateway.admission import CapacityAdmissionController, CapacityRejected
from src.core.gateway.capacity import CapacityBudget


class FakeRedisBudget:
    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail
        self.zsets: dict[str, dict[str, float]] = {}
        self.round_trips = 0

    async def _check(self) -> None:
        if self.fail:
            raise ConnectionError("redis unavailable")

    async def eval(self, script: str, numkeys: int, *keys_and_args: object) -> object:
        await self._check()
        self.round_trips += 1
        keys = [str(key) for key in keys_and_args[:numkeys]]
        args = [str(arg) for arg in keys_and_args[numkeys:]]
        if "ZREM" in script and "PEXPIRE" not in script:
            # CAPACITY_RELEASE_LUA: KEYS[1..N], ARGV[1] = member
            for key in keys:
                self.zsets.setdefault(key, {}).pop(args[0], None)
            return len(keys)
        # Simulate the CAPACITY_ACQUIRE_PAIR_LUA contract:
        # KEYS[1..2]; ARGV = now_ms, expires_at_ms, member, ttl_ms,
        # shared limit, tenant limit. Shared-before-tenant ordering.
        now_ms = float(args[0])
        expires_at = float(args[1])
        member = args[2]

        def _admit(key: str, limit: int) -> int:
            zset = self.zsets.setdefault(key, {})
            for existing, score in list(zset.items()):
                if 0 <= score <= now_ms:
                    zset.pop(existing, None)
            count = len(zset)
            if count < limit:
                zset[member] = expires_at
            return count

        shared_count = _admit(keys[0], int(args[4]))
        tenant_count = -1
        if shared_count < int(args[4]):
            tenant_count = _admit(keys[1], int(args[5]))
        return [shared_count, tenant_count]

    async def zremrangebyscore(self, key: str, minimum: float, maximum: float) -> int:
        await self._check()
        self.round_trips += 1
        zset = self.zsets.setdefault(key, {})
        expired = [member for member, score in zset.items() if minimum <= score <= maximum]
        for member in expired:
            zset.pop(member, None)
        return len(expired)

    async def zcard(self, key: str) -> int:
        await self._check()
        self.round_trips += 1
        return len(self.zsets.setdefault(key, {}))

    async def zadd(self, key: str, mapping: dict[str, float]) -> int:
        await self._check()
        self.round_trips += 1
        self.zsets.setdefault(key, {}).update(mapping)
        return len(mapping)

    async def zrem(self, key: str, member: str) -> int:
        await self._check()
        self.round_trips += 1
        existed = member in self.zsets.setdefault(key, {})
        self.zsets[key].pop(member, None)
        return 1 if existed else 0


def _shared_budget() -> CapacityBudget:
    return CapacityBudget(
        key="upstream.langgraph_agent",
        limit=4,
        queue_max=0,
        queue_timeout_ms=100,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    )


@pytest.mark.asyncio
async def test_two_gateway_instances_share_one_upstream_redis_budget():
    redis = FakeRedisBudget()
    first_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=1.0,
    )
    second_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-b",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=1.0,
    )
    budget = _shared_budget()

    leases = []
    for index in range(4):
        controller = first_gateway if index % 2 == 0 else second_gateway
        leases.append(
            await controller.acquire(
                budgets=[budget],
                tenant_id="tenant-a",
                user_id=f"user-{index}",
                service_id="local-2024-agent",
                request_class="sync",
                request_id=f"req-{index}",
            )
        )

    with pytest.raises(CapacityRejected):
        await second_gateway.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-over",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-over",
        )

    await leases[0].release()
    admitted_after_release = await second_gateway.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-next",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-next",
    )
    await admitted_after_release.release()
    for lease in leases[1:]:
        await lease.release()


@pytest.mark.asyncio
async def test_upstream_redis_budget_is_shared_across_tenants():
    redis = FakeRedisBudget()
    first_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=1.0,
    )
    second_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-b",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=1.0,
    )
    budget = _shared_budget()

    leases = []
    for index in range(4):
        controller = first_gateway if index % 2 == 0 else second_gateway
        leases.append(
            await controller.acquire(
                budgets=[budget],
                tenant_id=f"tenant-{index}",
                user_id=f"user-{index}",
                service_id="local-2024-agent",
                request_class="sync",
                request_id=f"req-{index}",
            )
        )

    with pytest.raises(CapacityRejected):
        await second_gateway.acquire(
            budgets=[budget],
            tenant_id="tenant-over",
            user_id="user-over",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-over",
        )

    for lease in leases:
        await lease.release()


@pytest.mark.asyncio
async def test_two_gateway_instances_share_one_tenant_redis_budget():
    redis = FakeRedisBudget()
    first_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=0.5,
    )
    second_gateway = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-b",
        cluster_epoch="uat-2026-05",
        per_tenant_default_share=0.5,
    )
    budget = _shared_budget()

    first = await first_gateway.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-1",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-1",
    )
    second = await second_gateway.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-2",
        service_id="local-2024-agent",
        request_class="sync",
        request_id="req-2",
    )

    with pytest.raises(CapacityRejected) as exc_info:
        await first_gateway.acquire(
            budgets=[budget],
            tenant_id="tenant-a",
            user_id="user-over",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-over",
        )

    assert exc_info.value.status_code == 429
    assert exc_info.value.code == "GATEWAY_TENANT_CAPACITY_EXHAUSTED"

    await second.release()
    await first.release()


@pytest.mark.asyncio
async def test_redis_unavailable_fails_closed_for_shared_sync_budget():
    controller = CapacityAdmissionController(
        redis_client=FakeRedisBudget(fail=True),
        gateway_instance_id="gw-a",
        per_tenant_default_share=1.0,
    )

    with pytest.raises(CapacityRejected) as exc_info:
        await controller.acquire(
            budgets=[_shared_budget()],
            tenant_id="tenant-a",
            user_id="user-a",
            service_id="local-2024-agent",
            request_class="sync",
            request_id="req-fail",
        )

    assert exc_info.value.status_code == 503
    assert exc_info.value.code == "GATEWAY_CAPACITY_DEGRADED"
