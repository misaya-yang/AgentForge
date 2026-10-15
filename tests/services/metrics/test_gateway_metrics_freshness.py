from __future__ import annotations

from datetime import datetime
from unittest.mock import MagicMock

import pytest

from src.services.metrics.metrics_recorder import MetricsRecorder


class _FakePipeline:
    def __init__(self, client: _FakeRedisClient):
        self.client = client
        self.ops: list[tuple[str, tuple]] = []

    def incr(self, key: str):
        self.ops.append(("incr", (key,)))
        return self

    def incrby(self, key: str, value: int):
        self.ops.append(("incrby", (key, value)))
        return self

    def expire(self, key: str, ttl: int):
        self.ops.append(("expire", (key, ttl)))
        return self

    def set(self, key: str, value: str, ex: int | None = None):
        self.ops.append(("set", (key, value, ex)))
        return self

    def get(self, key: str):
        self.ops.append(("get", (key,)))
        return self

    def zadd(self, key: str, mapping: dict[str, float]):
        self.ops.append(("zadd", (key, mapping)))
        return self

    def zremrangebyrank(self, key: str, start: int, stop: int):
        self.ops.append(("zremrangebyrank", (key, start, stop)))
        return self

    async def execute(self):
        results = []
        for op, args in self.ops:
            if op == "incr":
                key = args[0]
                self.client.values[key] = str(int(self.client.values.get(key, 0)) + 1)
                results.append(1)
            elif op == "incrby":
                key, value = args
                self.client.values[key] = str(int(self.client.values.get(key, 0)) + int(value))
                results.append(1)
            elif op == "set":
                key, value, _ex = args
                self.client.values[key] = value
                results.append(True)
            elif op == "get":
                results.append(self.client.values.get(args[0]))
            elif op == "zadd":
                key, mapping = args
                self.client.zsets.setdefault(key, {}).update(mapping)
                results.append(len(mapping))
            elif op == "zremrangebyrank":
                results.append(0)
            else:
                results.append(None)
        return results


class _FakeRedisClient:
    def __init__(self):
        self.values: dict[str, str] = {}
        self.zsets: dict[str, dict[str, float]] = {}

    def pipeline(self):
        return _FakePipeline(self)

    async def zrange(self, key: str, _start: int, _end: int):
        return list(self.zsets.get(key, {}))


def _recorder_with_fake_redis() -> MetricsRecorder:
    storage = MagicMock()
    storage._client = _FakeRedisClient()
    return MetricsRecorder(redis=storage)


@pytest.mark.asyncio
async def test_empty_metrics_summary_reports_empty_data_status():
    recorder = MetricsRecorder(redis=None)

    summary = await recorder.get_today_summary()

    assert summary["data_status"] == "empty"
    assert summary["data_source"] == "none"
    assert summary["last_ingested_at"] is None


@pytest.mark.asyncio
async def test_redis_metrics_report_fresh_ingestion_timestamp():
    recorder = _recorder_with_fake_redis()

    await recorder.record_request(
        method="GET",
        path="/api/v1/proxy/local/assistants/search",
        status_code=200,
        duration_ms=42,
        user_id="user-a",
        service_id="local",
    )
    summary = await recorder.get_today_summary()

    assert summary["total_requests"] == 1
    assert summary["data_status"] == "ok"
    assert summary["data_source"] == "redis"
    assert summary["data_freshness_minutes"] <= 1
    assert datetime.fromisoformat(summary["last_ingested_at"]).tzinfo is not None


@pytest.mark.asyncio
async def test_token_metrics_use_tenant_scoped_user_keys():
    recorder = _recorder_with_fake_redis()
    today = recorder._get_date_str()

    await recorder.record_tokens(
        user_id="same-user",
        service_id="assistant",
        input_tokens=7,
        output_tokens=11,
        tenant_id="tenant-a",
    )

    values = recorder.redis._client.values
    assert values[f"metrics:tokens:tenant:tenant-a:user:same-user:input:{today}"] == "7"
    assert values[f"metrics:tokens:tenant:tenant-a:user:same-user:output:{today}"] == "11"
    assert f"metrics:tokens:user:same-user:input:{today}" not in values
    assert f"metrics:tokens:user:same-user:output:{today}" not in values


@pytest.mark.asyncio
async def test_metrics_endpoint_requires_metrics_read(test_app, async_client):
    from src.api import deps

    async def _user_auth_context():
        return deps.AuthContext(
            user_id="user-a",
            tenant_id="tenant-a",
            roles=["user"],
            permissions=[],
            is_authenticated=True,
        )

    test_app.dependency_overrides[deps.get_auth_context] = _user_auth_context

    try:
        response = await async_client.get("/api/v1/metrics/summary")
    finally:
        test_app.dependency_overrides.pop(deps.get_auth_context, None)

    assert response.status_code == 403
    assert "GatewayMetricsRead" in response.text
