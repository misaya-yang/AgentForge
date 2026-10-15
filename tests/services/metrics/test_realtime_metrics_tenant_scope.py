from __future__ import annotations

from datetime import datetime
from unittest.mock import MagicMock

import pytest
from ai_gateway_core.metrics.realtime_metrics import RealtimeMetricsService


class _FakePipeline:
    def __init__(self, client: _FakeRedisClient):
        self.client = client
        self.ops: list[tuple[str, tuple]] = []

    def get(self, key: str):
        self.ops.append(("get", (key,)))
        return self

    def incr(self, key: str):
        self.ops.append(("incr", (key,)))
        return self

    def decr(self, key: str):
        self.ops.append(("decr", (key,)))
        return self

    def expire(self, key: str, ttl: int):
        self.ops.append(("expire", (key, ttl)))
        return self

    def zadd(self, key: str, mapping: dict[str, float]):
        self.ops.append(("zadd", (key, mapping)))
        return self

    def zrem(self, key: str, member: str):
        self.ops.append(("zrem", (key, member)))
        return self

    async def execute(self):
        results = []
        for op, args in self.ops:
            if op == "get":
                results.append(self.client.values.get(args[0]))
            elif op == "incr":
                key = args[0]
                self.client.values[key] = str(int(self.client.values.get(key, 0)) + 1)
                results.append(1)
            elif op == "decr":
                key = args[0]
                self.client.values[key] = str(int(self.client.values.get(key, 0)) - 1)
                results.append(1)
            elif op == "zadd":
                key, mapping = args
                self.client.zsets.setdefault(key, {}).update(mapping)
                results.append(len(mapping))
            elif op == "zrem":
                key, member = args
                self.client.zsets.get(key, {}).pop(member, None)
                results.append(1)
            else:
                results.append(True)
        return results


class _FakeRedisClient:
    def __init__(self):
        self.values: dict[str, str] = {}
        self.zsets: dict[str, dict[str, float]] = {}

    def pipeline(self):
        return _FakePipeline(self)


def _service() -> tuple[RealtimeMetricsService, _FakeRedisClient]:
    client = _FakeRedisClient()
    storage = MagicMock()
    storage._client = client
    return RealtimeMetricsService(storage), client


@pytest.mark.asyncio
async def test_realtime_user_metrics_are_tenant_scoped():
    service, client = _service()
    today = datetime.now().strftime("%Y-%m-%d")
    client.values[f"metrics:tokens:tenant:tenant-a:user:same-user:input:{today}"] = "3"
    client.values[f"metrics:tokens:tenant:tenant-a:user:same-user:output:{today}"] = "5"
    client.values[f"metrics:tokens:tenant:tenant-b:user:same-user:input:{today}"] = "101"
    client.values[f"metrics:tokens:tenant:tenant-b:user:same-user:output:{today}"] = "103"
    client.values["metrics:rt:threads:tenant:tenant-a:user:same-user"] = "2"
    client.values["metrics:rt:threads:tenant:tenant-b:user:same-user"] = "9"

    metrics = await service.get_user_metrics("same-user", tenant_id="tenant-a")

    assert metrics["tokens"]["input"] == 3
    assert metrics["tokens"]["output"] == 5
    assert metrics["tokens"]["total"] == 8
    assert metrics["active_threads"] == 2


@pytest.mark.asyncio
async def test_realtime_request_threads_are_recorded_under_tenant_key():
    service, client = _service()

    await service.record_request_start(
        request_id="req-1",
        user_id="same-user",
        tenant_id="tenant-a",
    )

    assert client.values["metrics:rt:threads:tenant:tenant-a:user:same-user"] == "1"
    assert "metrics:rt:threads:same-user" not in client.values
    assert "tenant-a:same-user" in client.zsets["metrics:rt:active_users"]
