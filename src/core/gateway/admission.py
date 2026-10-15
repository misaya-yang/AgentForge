from __future__ import annotations

import asyncio
import json
import math
import os
import time
import uuid
from collections import deque
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any

from ai_gateway_core.logging import get_logger

from .capacity import CapacityBudget
from .lua_scripts import (
    CAPACITY_ACQUIRE_PAIR_LUA,
    CAPACITY_RELEASE_LUA,
    CAPACITY_RENEW_LUA,
    eval_script,
)

logger = get_logger(__name__)


class CapacityRejected(RuntimeError):
    def __init__(
        self,
        *,
        budget_key: str,
        code: str = "GATEWAY_CAPACITY_EXHAUSTED",
        message: str | None = None,
        queue_wait_ms: float = 0.0,
        retry_after: int = 1,
        status_code: int = 503,
        headers: dict[str, str] | None = None,
    ) -> None:
        self.budget_key = budget_key
        self.code = code
        self.status_code = status_code
        self.queue_wait_ms = max(float(queue_wait_ms), 0.0)
        self.retry_after = max(int(retry_after), 1)
        self.message = message or f"Gateway capacity exhausted for {budget_key}"
        self.headers = {
            "X-Gateway-Capacity-Key": budget_key,
            "X-Gateway-Queue-Wait-Ms": f"{self.queue_wait_ms:.1f}",
            "Retry-After": str(self.retry_after),
        }
        if headers:
            self.headers.update(headers)
        super().__init__(self.message)


@dataclass
class _LocalBudgetState:
    limit: int
    queue_max: int
    queue_timeout_ms: int
    inflight: int = 0
    queue_depth: int = 0

    def update(self, budget: CapacityBudget) -> None:
        self.limit = max(int(budget.limit), 1)
        self.queue_max = max(int(budget.queue_max), 0)
        self.queue_timeout_ms = max(int(budget.queue_timeout_ms), 1)


async def _finish_cleanup(task: asyncio.Task[Any]) -> Any:
    """Cancellation propagates only after owned cleanup has completed."""
    cancelled = False
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            cancelled = True
    result = task.result()
    if cancelled:
        raise asyncio.CancelledError
    return result


class CapacityLease:
    def __init__(
        self,
        *,
        budget_keys: list[str],
        queue_wait_ms: float,
        release: Callable[[], Awaitable[None]],
    ) -> None:
        self.budget_keys = budget_keys
        self.queue_wait_ms = max(float(queue_wait_ms), 0.0)
        self._release = release
        self._released = False
        self._release_task: asyncio.Task[None] | None = None
        self._renew_task: asyncio.Task[None] | None = None
        self._owner = asyncio.current_task()
        self.lost = False

    @property
    def headers(self) -> dict[str, str]:
        return {
            "X-Gateway-Capacity-Key": ",".join(self.budget_keys),
            "X-Gateway-Queue-Wait-Ms": f"{self.queue_wait_ms:.1f}",
        }

    async def release(self) -> None:
        if self._released:
            return
        if self._release_task is None:
            async def finish() -> None:
                if self._renew_task is not None:
                    self._renew_task.cancel()
                    await asyncio.gather(self._renew_task, return_exceptions=True)
                await self._release()
                self._released = True
            self._release_task = asyncio.create_task(finish())
        await _finish_cleanup(self._release_task)

    def bind_owner(self) -> None:
        self._owner = asyncio.current_task()
        if self.lost:
            raise CapacityRejected(budget_key=",".join(self.budget_keys), code="GATEWAY_CAPACITY_LEASE_LOST")

    async def __aenter__(self) -> CapacityLease:
        self.bind_owner()
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        await self.release()


class AdaptiveLoadShedder:
    """Latency-window based load shedder for gateway admission."""

    def __init__(
        self,
        *,
        window_seconds: float = 30.0,
        low_threshold_ms: float = 5_000,
        normal_threshold_ms: float = 10_000,
        high_threshold_ms: float = 20_000,
        retry_after: int = 1,
    ) -> None:
        self.window_seconds = max(float(window_seconds), 1.0)
        self.low_threshold_ms = max(float(low_threshold_ms), 1.0)
        self.normal_threshold_ms = max(float(normal_threshold_ms), 1.0)
        self.high_threshold_ms = max(float(high_threshold_ms), 1.0)
        self.retry_after = max(int(retry_after), 1)
        self._samples: deque[tuple[float, float]] = deque()

    def record_latency(self, latency_ms: float) -> None:
        now = time.monotonic()
        self._samples.append((now, max(float(latency_ms), 0.0)))
        self._evict(now)

    def maybe_reject(
        self,
        *,
        service_id: str,
        request_class: str,
        priority: int,
    ) -> CapacityRejected | None:
        if int(priority) <= 0:
            return None
        now = time.monotonic()
        self._evict(now)
        p99 = self.p99_latency_ms()
        if p99 is None:
            return None

        threshold = self._threshold_for_priority(priority)
        if p99 <= threshold:
            return None
        return CapacityRejected(
            budget_key=f"load_shed.{service_id}.{request_class}",
            code="GATEWAY_LOAD_SHED",
            message=f"Gateway load shedding active for {service_id}",
            status_code=503,
            retry_after=self.retry_after,
            headers={
                "X-Gateway-Load-Shed": "true",
                "X-Gateway-Request-Priority": str(priority),
            },
        )

    def p99_latency_ms(self) -> float | None:
        if not self._samples:
            return None
        values = sorted(value for _ts, value in self._samples)
        index = max(math.ceil(len(values) * 0.99) - 1, 0)
        return values[min(index, len(values) - 1)]

    def _threshold_for_priority(self, priority: int) -> float:
        if priority <= 1:
            return self.high_threshold_ms
        if priority == 2:
            return self.normal_threshold_ms
        return self.low_threshold_ms

    def _evict(self, now: float) -> None:
        cutoff = now - self.window_seconds
        while self._samples and self._samples[0][0] < cutoff:
            self._samples.popleft()


class CapacityAdmissionController:
    """Local and Redis-backed admission controller for Gateway UAT budgets."""

    def __init__(
        self,
        *,
        redis_client: Any | None = None,
        gateway_instance_id: str = "gateway-1",
        cluster_epoch: str = "uat-2026-05",
        lease_ttl_ms: int = 30000,
        per_tenant_default_share: float | None = None,
        per_tenant_limits: dict[str, int] | None = None,
        load_shedder: AdaptiveLoadShedder | None = None,
    ) -> None:
        self.redis = self._resolve_redis_client(redis_client)
        self.gateway_instance_id = gateway_instance_id
        self.cluster_epoch = cluster_epoch
        self.lease_ttl_ms = max(int(lease_ttl_ms), 1000)
        self.per_tenant_default_share = (
            per_tenant_default_share
            if per_tenant_default_share is not None
            else _get_float_env("ADMISSION_TENANT_SHARE_RATIO", 0.2)
        )
        self.per_tenant_default_share = min(max(float(self.per_tenant_default_share), 0.0), 1.0)
        self.per_tenant_limits = dict(per_tenant_limits or {})
        self.load_shedder = load_shedder
        self._states: dict[str, _LocalBudgetState] = {}
        self._tenant_states: dict[str, _LocalBudgetState] = {}
        self._conditions: dict[str, asyncio.Condition] = {}

    async def acquire(
        self,
        *,
        budgets: list[CapacityBudget],
        tenant_id: str,
        user_id: str,
        service_id: str,
        request_class: str,
        request_id: str,
        allow_degraded_open: bool = False,
        priority: int = 2,
    ) -> CapacityLease:
        enforced = [budget for budget in budgets if budget.enforced]
        acquired_local: list[CapacityBudget] = []
        acquired_tenant: list[tuple[str, str, str]] = []
        acquired_shared: list[tuple[str, str, str]] = []
        started = time.perf_counter()
        # Run/SSE/job leases span whole workflows, including user think time.
        # Their duration is not HTTP request latency. Feeding them into the
        # legacy latency window rejects healthy follow-up turns and callbacks.
        lifecycle_budget = any(
            budget.scope in {"run", "provider", "capability", "job", "sse"}
            and budget.key == f"gateway.{budget.scope}_inflight"
            for budget in enforced
        )
        load_shedder = None if lifecycle_budget else self.load_shedder

        try:
            if load_shedder is not None:
                rejected = load_shedder.maybe_reject(
                    service_id=service_id,
                    request_class=request_class,
                    priority=priority,
                )
                if rejected is not None:
                    _record_admission_rejected(service_id, rejected.code)
                    raise rejected
            for budget in enforced:
                remaining_ms = max(1, min(
                    budget.queue_timeout_ms,
                    min(item.queue_timeout_ms for item in enforced) - int((time.perf_counter() - started) * 1000),
                ))
                wait_ms = await self._acquire_local(budget, queue_timeout_ms=remaining_ms)
                acquired_local.append(budget)
                if self.redis is not None and budget.shared:
                    # One atomic EVAL covers the shared budget AND the tenant
                    # share (SPO-02): a single round trip, no TOCTOU window.
                    tenant_id_resolved = tenant_id or "public"
                    tenant_limit = self._tenant_limit_for(budget, tenant_id_resolved)
                    pair = await self._acquire_shared_and_tenant(
                        budget=budget,
                        tenant_id=tenant_id_resolved,
                        request_class=request_class,
                        request_id=request_id,
                        tenant_limit=tenant_limit,
                        allow_degraded_open=allow_degraded_open,
                    )
                    if pair is not None:
                        (shared_key, member), (tenant_key, _tenant_member) = pair
                        acquired_shared.append((shared_key, tenant_key, member))
                    else:
                        # Degraded open: the shared layer was skipped; the
                        # tenant share falls back to the local budget.
                        key = await self._acquire_tenant_local(
                            budget=budget,
                            tenant_id=tenant_id_resolved,
                            tenant_limit=tenant_limit,
                        )
                        acquired_tenant.append(("local", key, ""))
                else:
                    tenant_lease = await self._acquire_tenant_capacity(
                        budget=budget,
                        tenant_id=tenant_id or "public",
                        request_class=request_class,
                        request_id=request_id,
                        allow_degraded_open=allow_degraded_open,
                    )
                    acquired_tenant.append(tenant_lease)
                _record_admission_state(
                    service_id=service_id,
                    tenant_id=tenant_id or "public",
                    inflight=self._states[budget.key].inflight,
                    queue_depth=self._states[budget.key].queue_depth,
                )
                logger.info(
                    "gateway_capacity_decision result=admitted budget_key=%s service_id=%s "
                    "tenant_id=%s user_id=%s request_class=%s request_id=%s wait_ms=%.1f",
                    budget.key,
                    service_id,
                    tenant_id,
                    user_id,
                    request_class,
                    request_id,
                    wait_ms,
                )
        except BaseException as exc:
            if isinstance(exc, CapacityRejected):
                _record_admission_rejected(service_id, exc.code)
            await _finish_cleanup(asyncio.create_task(
                self._release_many(acquired_local, acquired_shared, acquired_tenant)
            ))
            raise

        queue_wait_ms = (time.perf_counter() - started) * 1000

        async def _release() -> None:
            duration_ms = (time.perf_counter() - started) * 1000
            if load_shedder is not None:
                load_shedder.record_latency(duration_ms)
            await self._release_many(acquired_local, acquired_shared, acquired_tenant)

        lease = CapacityLease(
            budget_keys=[budget.key for budget in enforced],
            queue_wait_ms=queue_wait_ms,
            release=_release,
        )
        if acquired_shared:
            lease._renew_task = asyncio.create_task(self._renew_loop(lease, acquired_shared))
        return lease

    async def _renew_loop(
        self, lease: CapacityLease, shared_leases: list[tuple[str, str, str]],
    ) -> None:
        try:
            while True:
                await asyncio.sleep(self.lease_ttl_ms / 3000)
                for shared_key, tenant_key, member in shared_leases:
                    now_ms = int(time.time() * 1000)
                    renewed = await asyncio.wait_for(eval_script(
                        self.redis, CAPACITY_RENEW_LUA,
                        keys=[shared_key, tenant_key],
                        args=[now_ms, now_ms + self.lease_ttl_ms, member, self.lease_ttl_ms],
                    ), timeout=self.lease_ttl_ms / 3000)
                    if int(renewed) != 1:
                        raise RuntimeError("capacity_lease_lost")
        except asyncio.CancelledError:
            raise
        except Exception:
            lease.lost = True
            logger.error("gateway_capacity_lease_lost budget_keys=%s", lease.budget_keys)
            if lease._owner is not None and not lease._owner.done():
                lease._owner.cancel()

    def snapshot(self) -> dict[str, dict[str, int]]:
        return {
            key: {
                "inflight": int(state.inflight),
                "queue_depth": int(state.queue_depth),
                "limit": int(state.limit),
            }
            for key, state in self._states.items()
        }

    async def _acquire_tenant_capacity(
        self,
        *,
        budget: CapacityBudget,
        tenant_id: str,
        request_class: str,
        request_id: str,
        allow_degraded_open: bool,
    ) -> tuple[str, str, str]:
        """Local tenant share (used when Redis is absent or the budget is
        not shared; the Redis shared path is handled by the combined
        ``_acquire_shared_and_tenant`` EVAL)."""
        del request_class, request_id, allow_degraded_open
        tenant_limit = self._tenant_limit_for(budget, tenant_id)
        key = await self._acquire_tenant_local(
            budget=budget,
            tenant_id=tenant_id,
            tenant_limit=tenant_limit,
        )
        return "local", key, ""

    async def _acquire_tenant_local(
        self,
        *,
        budget: CapacityBudget,
        tenant_id: str,
        tenant_limit: int | None = None,
    ) -> str:
        tenant_limit = tenant_limit if tenant_limit is not None else self._tenant_limit_for(budget, tenant_id)
        key = self._tenant_local_key(tenant_id, budget.key)
        condition = self._conditions.setdefault(key, asyncio.Condition())
        async with condition:
            # Re-check under lock (same TOCTOU fix as _acquire_local).
            state = self._tenant_states.get(key)
            if state is None:
                state = _LocalBudgetState(limit=tenant_limit, queue_max=0, queue_timeout_ms=1)
                self._tenant_states[key] = state
            state.limit = tenant_limit
            if state.inflight >= state.limit:
                raise CapacityRejected(
                    budget_key=budget.key,
                    code="GATEWAY_TENANT_CAPACITY_EXHAUSTED",
                    message=f"Tenant capacity exhausted for {tenant_id}",
                    status_code=429,
                    retry_after=math.ceil(budget.queue_timeout_ms / 1000),
                    headers={
                        "X-RateLimit-Remaining": "0",
                        "X-Gateway-Tenant-Id": tenant_id,
                    },
                )
            state.inflight += 1
        return key

    async def _release_tenant_local(self, key: str) -> None:
        condition = self._conditions.get(key)
        state = self._tenant_states.get(key)
        if condition is None or state is None:
            return
        async with condition:
            state.inflight = max(state.inflight - 1, 0)
            condition.notify(1)

    async def _acquire_shared_and_tenant(
        self,
        *,
        budget: CapacityBudget,
        tenant_id: str,
        request_class: str,
        request_id: str,
        tenant_limit: int,
        allow_degraded_open: bool,
    ) -> tuple[tuple[str, str], tuple[str, str]] | None:
        """Atomically admit the shared budget and the tenant share in one EVAL.

        SPO-02: replaces the former two-step ``zremrangebyscore → zcard →
        zadd`` sequence (six round trips across both keys, TOCTOU between
        workers) with a single script that cleans, counts, and conditionally
        adds for both keys, preserving shared-before-tenant rejection order.
        """
        shared_key = self._redis_key(
            tenant_id=tenant_id or "public",
            budget_key=budget.key,
            request_class=request_class or "sync",
        )
        tenant_key = self._tenant_redis_key(
            tenant_id=tenant_id or "public",
            budget_key=budget.key,
            request_class=request_class or "sync",
        )
        now_ms = int(time.time() * 1000)
        expires_at = now_ms + self.lease_ttl_ms
        member = json.dumps(
            {
                "request_id": request_id,
                "lease_id": str(uuid.uuid4()),
                "gateway_instance_id": self.gateway_instance_id,
                "started_at_ms": now_ms,
                "lease_expires_at_ms": expires_at,
            },
            sort_keys=True,
        )
        try:
            acquire_task = asyncio.create_task(eval_script(
                self.redis,
                CAPACITY_ACQUIRE_PAIR_LUA,
                keys=[shared_key, tenant_key],
                args=[
                    now_ms,
                    expires_at,
                    member,
                    self.lease_ttl_ms,
                    budget.limit,
                    tenant_limit,
                ],
            ))
            try:
                shared_count, tenant_count = await asyncio.shield(acquire_task)
            except asyncio.CancelledError:
                try:
                    await _finish_cleanup(acquire_task)
                finally:
                    await _finish_cleanup(asyncio.create_task(
                        self._release_shared_pair(shared_key, tenant_key, member)
                    ))
                raise
            shared_count = int(shared_count)
            tenant_count = int(tenant_count)
            if shared_count >= budget.limit:
                raise CapacityRejected(
                    budget_key=budget.key,
                    retry_after=math.ceil(budget.queue_timeout_ms / 1000),
                )
            if tenant_count >= tenant_limit:
                # Lua already ZREM'd the shared member; release is idempotent
                # and covers an older script that admitted shared first.
                await self._release_shared_pair(shared_key, tenant_key, member)
                raise CapacityRejected(
                    budget_key=budget.key,
                    code="GATEWAY_TENANT_CAPACITY_EXHAUSTED",
                    message=f"Tenant capacity exhausted for {tenant_id}",
                    status_code=429,
                    retry_after=math.ceil(budget.queue_timeout_ms / 1000),
                    headers={
                        "X-RateLimit-Remaining": "0",
                        "X-Gateway-Tenant-Id": tenant_id,
                    },
                )
        except CapacityRejected:
            raise
        except Exception as exc:
            if allow_degraded_open:
                logger.warning(
                    "gateway_capacity_decision result=degraded_open budget_key=%s error=%s",
                    budget.key,
                    exc,
                )
                return None
            raise CapacityRejected(
                budget_key=budget.key,
                code="GATEWAY_CAPACITY_DEGRADED",
                message=f"Shared capacity unavailable for {budget.key}",
            ) from exc
        return (shared_key, member), (tenant_key, member)

    async def _acquire_local(
        self, budget: CapacityBudget, *, queue_timeout_ms: int | None = None,
    ) -> float:
        condition = self._conditions.setdefault(budget.key, asyncio.Condition())
        started = time.perf_counter()
        async with condition:
            # Re-check state under the lock to prevent a TOCTOU race where two
            # coroutines both see ``state is None`` and overwrite each other's
            # _LocalBudgetState, breaking the capacity limit.
            state = self._states.get(budget.key)
            if state is None:
                state = _LocalBudgetState(
                    limit=budget.limit,
                    queue_max=budget.queue_max,
                    queue_timeout_ms=budget.queue_timeout_ms,
                )
                self._states[budget.key] = state
            state.update(budget)
            if state.inflight < state.limit:
                state.inflight += 1
                return 0.0

            if state.queue_max <= 0 or state.queue_depth >= state.queue_max:
                raise CapacityRejected(
                    budget_key=budget.key,
                    queue_wait_ms=(time.perf_counter() - started) * 1000,
                    retry_after=math.ceil(state.queue_timeout_ms / 1000),
                )

            state.queue_depth += 1
            deadline = time.monotonic() + (queue_timeout_ms or state.queue_timeout_ms) / 1000
            try:
                while state.inflight >= state.limit:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise CapacityRejected(
                            budget_key=budget.key,
                            queue_wait_ms=(time.perf_counter() - started) * 1000,
                            retry_after=math.ceil(state.queue_timeout_ms / 1000),
                        )
                    try:
                        await asyncio.wait_for(condition.wait(), timeout=remaining)
                    except asyncio.TimeoutError as exc:
                        raise CapacityRejected(
                            budget_key=budget.key,
                            queue_wait_ms=(time.perf_counter() - started) * 1000,
                            retry_after=math.ceil(state.queue_timeout_ms / 1000),
                        ) from exc
                state.inflight += 1
                return (time.perf_counter() - started) * 1000
            finally:
                state.queue_depth = max(state.queue_depth - 1, 0)

    async def _release_local(self, budget: CapacityBudget) -> None:
        condition = self._conditions.get(budget.key)
        state = self._states.get(budget.key)
        if condition is None or state is None:
            return
        async with condition:
            state.inflight = max(state.inflight - 1, 0)
            condition.notify(1)

    async def _release_shared_pair(
        self, shared_key: str, tenant_key: str, member: str
    ) -> None:
        if self.redis is None:
            return
        try:
            # One EVAL releases both keys (SPO-02): a single round trip.
            await asyncio.wait_for(eval_script(
                self.redis,
                CAPACITY_RELEASE_LUA,
                keys=[shared_key, tenant_key],
                args=[member],
            ), timeout=min(self.lease_ttl_ms / 1000, 5.0))
        except Exception as exc:
            logger.warning(
                "Failed to release shared capacity lease shared_key=%s error=%s",
                shared_key,
                exc,
            )

    async def _release_many(
        self,
        local_budgets: list[CapacityBudget],
        shared_leases: list[tuple[str, str, str]],
        tenant_leases: list[tuple[str, str, str]] | None = None,
    ) -> None:
        # Each loop is independently guarded so a single failed release
        # does not prevent the remaining budgets from being freed.
        for kind, key, _member in reversed(tenant_leases or []):
            try:
                if kind == "local":
                    await self._release_tenant_local(key)
            except Exception as exc:
                logger.warning(
                    "Failed to release tenant lease kind=%s key=%s: %s", kind, key, exc
                )
        for shared_key, tenant_key, member in reversed(shared_leases):
            try:
                await self._release_shared_pair(shared_key, tenant_key, member)
            except Exception as exc:
                logger.warning(
                    "Failed to release shared lease shared_key=%s: %s", shared_key, exc
                )
        for budget in reversed(local_budgets):
            try:
                await self._release_local(budget)
            except Exception as exc:
                logger.warning(
                    "Failed to release local budget key=%s: %s", budget.key, exc
                )

    def _redis_key(self, *, tenant_id: str, budget_key: str, request_class: str) -> str:
        del tenant_id, request_class
        return f"gateway:capacity:{self.cluster_epoch}:global:{budget_key}"

    def _tenant_redis_key(self, *, tenant_id: str, budget_key: str, request_class: str) -> str:
        del request_class
        return f"gateway:tenant-capacity:{self.cluster_epoch}:{tenant_id}:{budget_key}"

    def _tenant_limit_for(self, budget: CapacityBudget, tenant_id: str) -> int:
        if tenant_id in self.per_tenant_limits:
            return max(int(self.per_tenant_limits[tenant_id]), 1)
        key = self._tenant_local_key(tenant_id, budget.key)
        if key in self.per_tenant_limits:
            return max(int(self.per_tenant_limits[key]), 1)
        return max(int(math.ceil(budget.limit * self.per_tenant_default_share)), 1)

    @staticmethod
    def _tenant_local_key(tenant_id: str, budget_key: str) -> str:
        return f"tenant:{tenant_id}:{budget_key}"

    @staticmethod
    def _resolve_redis_client(redis_client: Any | None) -> Any | None:
        if redis_client is None:
            return None
        getter = getattr(redis_client, "get_native_client", None)
        if callable(getter):
            try:
                return getter()
            except TypeError:
                pass
        client = getattr(redis_client, "_client", None)
        if client is not None:
            return client
        return redis_client


def _get_float_env(key: str, default: float) -> float:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


_ADMISSION_METRICS: dict[str, Any] | None = None


def _admission_metrics() -> dict[str, Any] | None:
    global _ADMISSION_METRICS
    if _ADMISSION_METRICS is not None:
        return _ADMISSION_METRICS
    try:
        from src.core.observability.metrics import Counter, Gauge, get_metrics

        collector = get_metrics()
        _ADMISSION_METRICS = {
            "inflight": collector.register_gauge(
                Gauge(
                    "admission_inflight_requests",
                    "Current admitted requests",
                    labels=["service", "tenant"],
                )
            ),
            "queued": collector.register_gauge(
                Gauge(
                    "admission_queued_requests",
                    "Current queued admission requests",
                    labels=["service"],
                )
            ),
            "rejected": collector.register_counter(
                Counter(
                    "admission_rejected_total",
                    "Total admission rejections",
                    labels=["service", "reason"],
                )
            ),
        }
        return _ADMISSION_METRICS
    except Exception:  # noqa: BLE001
        _ADMISSION_METRICS = {}
        return None


def _record_admission_state(
    *,
    service_id: str,
    tenant_id: str,
    inflight: int,
    queue_depth: int,
) -> None:
    metrics = _admission_metrics()
    if not metrics:
        return
    metrics["inflight"].set(inflight, service=service_id, tenant=tenant_id)
    metrics["queued"].set(queue_depth, service=service_id)


def _record_admission_rejected(service_id: str, reason: str) -> None:
    metrics = _admission_metrics()
    if not metrics:
        return
    metrics["rejected"].inc(service=service_id, reason=reason)
