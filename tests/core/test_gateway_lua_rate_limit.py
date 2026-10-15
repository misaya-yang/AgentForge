"""SPO-02 / GW2 gate tests: atomic Lua rate limiting and admission.

All tests drive the shipped middleware / limiter / admission code against a
fake Redis that simulates the two Lua script contracts atomically (the same
key/args plumbing as real ``EVAL``) while counting round trips.

Gates:
- warm path rate limiting is ONE round trip for all middleware dimensions;
- the user dimension is counted exactly once across middleware + route level;
- admission acquire + release is 2 round trips (chat path stays ≤ 4 total
  including the unchanged jti revocation check);
- concurrent admission cannot over-sell (Lua TOCTOU).
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest
from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from src.core.gateway.admission import CapacityAdmissionController, CapacityRejected
from src.core.gateway.capacity import CapacityBudget
from src.core.gateway.lua_scripts import SLIDING_WINDOW_CHECK_LUA, eval_script
from src.core.gateway.multi_dimension_rate_limiter import (
    MultiDimensionRateLimitConfig,
    MultiDimensionRateLimiter,
    RateLimitContext,
)
from src.core.middleware.streaming import (
    StreamingAnonymousConfig,
    StreamingAnonymousMiddleware,
    StreamingAuthConfig,
    StreamingAuthMiddleware,
    StreamingRateLimitConfig,
    StreamingRateLimitMiddleware,
)


class _LuaFakeRedis:
    """Fake redis that simulates both Lua script contracts and counts RTTs."""

    def __init__(self) -> None:
        self.zsets: dict[str, dict[str, float]] = {}
        self.round_trips = 0
        self.eval_calls: list[tuple[str, list[str], list[str]]] = []

    async def eval(self, script: str, numkeys: int, *keys_and_args: object) -> list[object]:
        self.round_trips += 1
        keys = [str(key) for key in keys_and_args[:numkeys]]
        args = [str(arg) for arg in keys_and_args[numkeys:]]
        self.eval_calls.append((script, keys, args))
        if "PEXPIRE" in script:
            # CAPACITY_ACQUIRE_PAIR_LUA: KEYS[1..2];
            # ARGV = now_ms, expires_at, member, ttl, shared limit, tenant limit.
            # Shared-before-tenant ordering: tenant untouched when shared rejects.
            now_ms = float(args[0])
            expires_at = float(args[1])
            member = args[2]

            def _admit(key: str, limit: int) -> int:
                zset = self.zsets.setdefault(key, {})
                for existing, score in list(zset.items()):
                    if score <= now_ms:
                        zset.pop(existing, None)
                count = len(zset)
                if count < limit:
                    zset[member] = expires_at
                return count

            shared_count = _admit(keys[0], int(args[4]))
            tenant_count = -1
            if shared_count < int(args[4]):
                tenant_count = _admit(keys[1], int(args[5]))
                if tenant_count >= int(args[5]):
                    self.zsets.setdefault(keys[0], {}).pop(member, None)
            return [shared_count, tenant_count]
        if "redis.call('ZREM'" in script:
            # CAPACITY_RELEASE_LUA: KEYS[1..N], ARGV[1] = member
            for key in keys:
                self.zsets.setdefault(key, {}).pop(args[0], None)
            return [len(keys)]
        # SLIDING_WINDOW_CHECK_LUA: KEYS[1..N]; ARGV = now, window_start, expire, member, limits
        now = float(args[0])
        window_start = float(args[1])
        member = args[3]
        for index, key in enumerate(keys):
            limit = int(args[4 + index])
            zset = self.zsets.setdefault(key, {})
            for existing, score in list(zset.items()):
                if score <= window_start:
                    zset.pop(existing, None)
            count = len(zset)
            if count >= limit:
                earliest = min(zset.values(), default=window_start)
                return [index, earliest, 0]
            zset[member] = now
        remaining = min(
            int(args[4 + index]) - len(self.zsets[key])
            for index, key in enumerate(keys)
        )
        return [-1, 0, remaining]


@pytest.mark.asyncio
async def test_allowed_redis_limit_reports_real_remaining_capacity() -> None:
    redis = _LuaFakeRedis()
    limiter = MultiDimensionRateLimiter(
        MultiDimensionRateLimitConfig(
            global_enabled=False,
            ip_enabled=False,
            tenant_enabled=False,
            assistant_enabled=False,
            operation_limits={
                "assistant_chat": SimpleNamespace(requests=3, window=60, burst=0)
            },
        ),
        redis_client=redis,
    )
    context = RateLimitContext(
        ip="127.0.0.1",
        user_id="user-1",
        operation="assistant_chat",
    )

    first = await limiter.check(context)
    second = await limiter.check(context)

    assert first.allowed and first.remaining == 2
    assert second.allowed and second.remaining == 1


@pytest.mark.asyncio
async def test_same_window_custom_policies_use_one_redis_round_trip() -> None:
    redis = _LuaFakeRedis()
    limiter = MultiDimensionRateLimiter(
        MultiDimensionRateLimitConfig(),
        redis_client=redis,
    )
    policies = [
        SimpleNamespace(key="rate:a", requests=10, window=60, dimension="a"),
        SimpleNamespace(key="rate:b", requests=5, window=60, dimension="b"),
        SimpleNamespace(key="rate:c", requests=3, window=60, dimension="c"),
    ]

    results = await limiter.check_custom_limits(policies=policies)

    assert redis.round_trips == 1
    assert len(redis.eval_calls) == 1
    assert redis.eval_calls[0][1] == ["rate:a", "rate:b", "rate:c"]
    assert [result.dimension for result in results] == ["a", "b", "c"]
    assert all(result.allowed for result in results)


@pytest.mark.asyncio
async def test_registered_lua_script_is_reused_on_warm_path() -> None:
    class RegisteredRedis:
        def __init__(self) -> None:
            self.register_calls = 0
            self.evalsha_calls = 0

        def register_script(self, _script: str):
            self.register_calls += 1

            async def execute(*, keys, args):
                self.evalsha_calls += 1
                return [-1, 0, int(args[4]) - 1, int(args[4]) - 1]

            return execute

        async def eval(self, *_args):
            raise AssertionError("registered clients must not send raw EVAL")

    redis = RegisteredRedis()
    kwargs = {
        "keys": ["rate:a"],
        "args": [1, 0, 61, "member", 10],
    }
    await eval_script(redis, SLIDING_WINDOW_CHECK_LUA, **kwargs)
    await eval_script(redis, SLIDING_WINDOW_CHECK_LUA, **kwargs)

    assert redis.register_calls == 1
    assert redis.evalsha_calls == 2

    async def zrem(self, key: str, member: str) -> int:
        self.round_trips += 1
        existed = member in self.zsets.setdefault(key, {})
        self.zsets[key].pop(member, None)
        return 1 if existed else 0


def _shared_budget() -> CapacityBudget:
    return CapacityBudget(
        key="upstream.langgraph_agent",
        limit=2,
        queue_max=0,
        queue_timeout_ms=100,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    )


@pytest.mark.asyncio
async def test_middleware_checks_all_dimensions_in_one_round_trip() -> None:
    redis = _LuaFakeRedis()
    config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1000,
        user_limit=100,
        guest_limit=100,
        ip_limit=100,
        tenant_limit=500,
        whitelist_paths=[],
    )
    app = FastAPI()
    app.state.redis = SimpleNamespace(get_native_client=lambda: redis)
    app.add_middleware(StreamingAnonymousMiddleware, config=StreamingAnonymousConfig())
    app.add_middleware(StreamingRateLimitMiddleware, config=config)
    app.add_middleware(StreamingAuthMiddleware, config=StreamingAuthConfig())

    @app.get("/ping")
    async def ping() -> dict[str, str]:
        return {"ok": "true"}

    client = TestClient(app)
    response = client.get("/ping", headers={"X-AG-Anonymous-Id": "anon-1"})

    assert response.status_code == 200
    # global + guest + ip = 3 dimensions (no user info → no tenant) in ONE EVAL.
    assert redis.round_trips == 1
    assert len(redis.eval_calls) == 1
    assert len(redis.eval_calls[0][1]) == 3


@pytest.mark.asyncio
async def test_user_dimension_counted_exactly_once_across_middleware_and_route() -> None:
    import jwt as pyjwt

    secret = "sota-spo02-test-secret-32-chars-min"
    token = pyjwt.encode(
        {"sub": "user-1", "tenant_id": "tenant-1", "roles": ["user"]},
        secret,
        algorithm="HS256",
    )
    redis = _LuaFakeRedis()
    config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1000,
        user_limit=100,
        guest_limit=100,
        ip_limit=100,
        tenant_limit=500,
        whitelist_paths=[],
    )
    app = FastAPI()
    app.state.redis = SimpleNamespace(get_native_client=lambda: redis)
    app.add_middleware(StreamingAnonymousMiddleware, config=StreamingAnonymousConfig())
    app.add_middleware(StreamingRateLimitMiddleware, config=config)
    app.add_middleware(
        StreamingAuthMiddleware,
        config=StreamingAuthConfig(jwt_enabled=True, jwt_secret=secret),
    )

    counted_dimensions: list[set[str]] = []

    @app.get("/ping")
    async def ping(request: Request) -> dict[str, str]:
        counted_dimensions.append(request.state.rate_limit_counted_dimensions)
        return {"ok": "true"}

    client = TestClient(app)
    response = client.get("/ping", headers={"Authorization": f"Bearer {token}"})
    assert response.status_code == 200

    # The middleware counted global + user + tenant + ip in ONE EVAL and
    # recorded exactly which dimensions it counted.
    assert counted_dimensions == [{"global", "user", "tenant", "ip"}]
    route_limiter = MultiDimensionRateLimiter(
        MultiDimensionRateLimitConfig(
            ip_enabled=False,
            tenant_enabled=False,
            assistant_enabled=False,
            operation_limits={"assistant_chat": SimpleNamespace(requests=60, window=60, burst=0)},
        ),
        redis_client=redis,
    )
    result = await route_limiter.check(
        RateLimitContext(
            ip="127.0.0.1",
            user_id="user-1",
            user_tier="normal",
            tenant_id="tenant-1",
            operation="assistant_chat",
        ),
        skip_dimensions=counted_dimensions[0],
    )
    assert result.allowed

    # One EVAL from the middleware + one for the operation dimension.
    assert redis.round_trips == 2
    # Every dimension counted exactly once — the user key appears in exactly
    # one EVAL (the middleware's), and the operation key in exactly one.
    user_count = sum(
        1
        for _script, keys, _args in redis.eval_calls
        for key in keys
        if key.startswith("ratelimit:user:")
    )
    operation_count = sum(
        1
        for _script, keys, _args in redis.eval_calls
        for key in keys
        if key.startswith("ratelimit:op:")
    )
    assert user_count == 1
    assert operation_count == 1


@pytest.mark.asyncio
async def test_admission_acquire_and_release_are_two_round_trips() -> None:
    redis = _LuaFakeRedis()
    controller = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat",
        per_tenant_default_share=0.0,
    )
    budget = _shared_budget()

    lease = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-a",
        user_id="user-a",
        service_id="svc",
        request_class="sync",
        request_id="req-1",
    )
    assert redis.round_trips == 1  # one atomic EVAL for the shared budget

    await lease.release()
    assert redis.round_trips == 2  # one ZREM for the release


@pytest.mark.asyncio
async def test_concurrent_admission_cannot_oversell_lua_atomicity() -> None:
    redis = _LuaFakeRedis()
    controller = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat",
        per_tenant_default_share=1.0,
    )
    budget = _shared_budget()  # limit=2

    async def _acquire(request_id: str) -> bool:
        try:
            lease = await controller.acquire(
                budgets=[budget],
                tenant_id="tenant-a",
                user_id="user-a",
                service_id="svc",
                request_class="sync",
                request_id=request_id,
            )
        except CapacityRejected:
            return False
        await asyncio.sleep(0.05)  # hold the lease across the race window
        await lease.release()
        return True

    results = await asyncio.gather(
        *(_acquire(f"req-{index}") for index in range(10))
    )
    assert sum(results) == 2  # exactly the budget limit, no over-sell


@pytest.mark.asyncio
async def test_rejected_dimension_reports_index_for_retry_header() -> None:
    redis = _LuaFakeRedis()
    config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1,
        user_limit=100,
        guest_limit=100,
        ip_limit=100,
        whitelist_paths=[],
    )
    app = FastAPI()
    app.state.redis = SimpleNamespace(get_native_client=lambda: redis)
    app.add_middleware(StreamingRateLimitMiddleware, config=config)

    @app.get("/ping")
    async def ping() -> dict[str, str]:
        return {"ok": "true"}

    client = TestClient(app)
    assert client.get("/ping", headers={"X-AG-Anonymous-Id": "anon-1"}).status_code == 200
    response = client.get("/ping", headers={"X-AG-Anonymous-Id": "anon-1"})
    assert response.status_code == 429
    assert response.json()["error"]["dimension"] == "global"
    assert response.headers["Retry-After"]


@pytest.mark.asyncio
async def test_tenant_share_reject_does_not_leak_shared_slot() -> None:
    """Shared-limit 4, tenant share 1: extra acquires must not occupy shared."""
    from src.core.gateway.lua_scripts import CAPACITY_ACQUIRE_PAIR_LUA

    assert "ZREM" in CAPACITY_ACQUIRE_PAIR_LUA
    redis = _LuaFakeRedis()
    controller = CapacityAdmissionController(
        redis_client=redis,
        gateway_instance_id="gw-a",
        cluster_epoch="uat",
        per_tenant_default_share=0.25,
    )
    budget = CapacityBudget(
        key="upstream.langgraph_agent",
        limit=4,
        queue_max=0,
        queue_timeout_ms=100,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    )

    lease = await controller.acquire(
        budgets=[budget],
        tenant_id="tenant-busy",
        user_id="user-a",
        service_id="svc",
        request_class="sync",
        request_id="req-keep",
    )
    rejected = 0
    for index in range(2):
        try:
            await controller.acquire(
                budgets=[budget],
                tenant_id="tenant-busy",
                user_id="user-a",
                service_id="svc",
                request_class="sync",
                request_id=f"req-reject-{index}",
            )
        except CapacityRejected:
            rejected += 1
    assert rejected == 2

    shared_keys = [key for key in redis.zsets if key.startswith("gateway:capacity:")]
    assert len(shared_keys) == 1
    assert len(redis.zsets[shared_keys[0]]) == 1
    await lease.release()


def test_streaming_policy_mirrors_env_user_and_ip_limits() -> None:
    from src.core.gateway.multi_dimension_rate_limiter import create_rate_limit_config
    from src.core.middleware._streaming.rate_limit import (
        streaming_rate_limit_config_from_policy,
    )

    config = streaming_rate_limit_config_from_policy(
        create_rate_limit_config(),
        whitelist_paths=[],
    )
    assert config.ip_limit == 30
    assert config.user_limit == 60
    assert config.user_tier_limits["normal"] == 60
    assert config.user_tier_limits["enterprise"] == 1000
    assert config.user_tier_limits["admin"] == 10000


@pytest.mark.asyncio
async def test_normal_tier_is_refused_at_policy_limit_not_legacy_300() -> None:
    """Middleware must use RATE_LIMIT_NORMAL_LIMIT, not the old 300/min cap."""
    import jwt as pyjwt

    from src.core.gateway.multi_dimension_rate_limiter import (
        TierLimit,
        create_rate_limit_config,
    )
    from src.core.middleware._streaming.rate_limit import (
        streaming_rate_limit_config_from_policy,
    )

    secret = "sota-spo02-test-secret-32-chars-min"
    token = pyjwt.encode(
        {"sub": "user-1", "tenant_id": "tenant-1", "roles": ["user"], "tier": "normal"},
        secret,
        algorithm="HS256",
    )
    policy = create_rate_limit_config()
    policy.user_tier_limits["normal"] = TierLimit(requests=2, window=60, burst=0)
    policy.ip_limit = 30
    config = streaming_rate_limit_config_from_policy(
        policy,
        whitelist_paths=[],
        global_limit=1000,
        guest_limit=100,
    )
    assert config.user_limit == 2
    assert config.ip_limit == 30
    assert config.user_tier_limits["normal"] == 2

    redis = _LuaFakeRedis()
    app = FastAPI()
    app.state.redis = SimpleNamespace(get_native_client=lambda: redis)
    app.add_middleware(StreamingRateLimitMiddleware, config=config)
    app.add_middleware(
        StreamingAuthMiddleware,
        config=StreamingAuthConfig(jwt_enabled=True, jwt_secret=secret),
    )

    @app.get("/ping")
    async def ping() -> dict[str, str]:
        return {"ok": "true"}

    client = TestClient(app)
    headers = {"Authorization": f"Bearer {token}"}
    assert client.get("/ping", headers=headers).status_code == 200
    assert client.get("/ping", headers=headers).status_code == 200
    denied = client.get("/ping", headers=headers)
    assert denied.status_code == 429
    assert denied.json()["error"]["dimension"] == "user"
