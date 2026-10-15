"""Tests for MetricsRecorder latency-sample ZSET invariants.

Phase 0 hotfix coverage:
1. ZSET member uniqueness under concurrent identical (timestamp, duration_ms)
   — prevents p99 under-counting from Redis de-duplication.
2. Percentile parser compatibility — member format includes a UUID middle
   segment but keeps latency as the LAST ':'-delimited token, which is what
   ``sample.rsplit(':', 1)`` extracts.
3. Configurable ``latency_sample_cap`` — ``ZREMRANGEBYRANK`` trims to the
   configured window instead of a hard-coded 1000.
"""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from src.services.metrics.metrics_recorder import MetricsRecorder


class FakeZSet:
    """In-memory replacement for the small subset of Redis ZSET ops we use."""

    def __init__(self) -> None:
        # member -> score
        self._members: dict[str, float] = {}

    def zadd(self, mapping: dict[str, float]) -> int:
        added = 0
        for m, s in mapping.items():
            if m not in self._members:
                added += 1
            self._members[m] = s
        return added

    def zremrangebyrank(self, start: int, stop: int) -> int:
        if not self._members:
            return 0
        # sort ascending by score, match Redis semantics
        ordered = sorted(self._members.items(), key=lambda kv: kv[1])
        n = len(ordered)
        # Normalise negative indices
        s = start if start >= 0 else max(0, n + start)
        e = stop if stop >= 0 else max(-1, n + stop)
        if s > e or s >= n:
            return 0
        victims = ordered[s : e + 1]
        for m, _ in victims:
            self._members.pop(m, None)
        return len(victims)

    def zcard(self) -> int:
        return len(self._members)

    def zrange(self) -> list[str]:
        return [m for m, _ in sorted(self._members.items(), key=lambda kv: kv[1])]


class FakePipeline:
    """Buffers ZSET ops against a FakeZSet keyed by key."""

    def __init__(self, zsets: dict[str, FakeZSet]) -> None:
        self._zsets = zsets
        self._ops: list = []

    # --- non-zset ops: accept and ignore, matching real pipeline's fluent style
    def incr(self, *_a, **_kw):
        return self

    def incrby(self, *_a, **_kw):
        return self

    def expire(self, *_a, **_kw):
        return self

    # --- zset ops we care about
    def zadd(self, key: str, mapping: dict[str, float]):
        self._zsets.setdefault(key, FakeZSet()).zadd(mapping)
        return self

    def zremrangebyrank(self, key: str, start: int, stop: int):
        self._zsets.setdefault(key, FakeZSet()).zremrangebyrank(start, stop)
        return self

    async def execute(self):
        return []


def _make_recorder(latency_sample_cap: int = 10000) -> tuple[MetricsRecorder, dict[str, FakeZSet]]:
    zsets: dict[str, FakeZSet] = {}

    client = MagicMock()
    client.pipeline = MagicMock(side_effect=lambda: FakePipeline(zsets))
    # zrange used by get_latency_percentiles
    client.zrange = AsyncMock(
        side_effect=lambda key, _s, _e: zsets.get(key, FakeZSet()).zrange()
    )
    storage = MagicMock()
    storage._client = client

    recorder = MetricsRecorder(redis=storage, latency_sample_cap=latency_sample_cap)
    return recorder, zsets


class TestLatencyZSetCollision:
    @pytest.mark.asyncio
    async def test_no_collision_under_concurrent_identical_samples(self):
        recorder, zsets = _make_recorder(latency_sample_cap=10000)

        # Fire 1000 concurrent record_request with identical duration_ms.
        # Under the old (timestamp:duration) format, many members would
        # collide and ZADD would overwrite — ZCARD would be << 1000.
        await asyncio.gather(
            *[
                recorder.record_request(
                    method="GET", path="/x", status_code=200, duration_ms=100
                )
                for _ in range(1000)
            ]
        )

        zset = zsets["metrics:latency:samples"]
        assert zset.zcard() == 1000, (
            f"Expected 1000 unique members after 1000 concurrent identical calls, "
            f"got {zset.zcard()} — collision bug regressed"
        )

    @pytest.mark.asyncio
    async def test_percentile_parser_extracts_latency_from_new_format(self):
        """Sanity-check the parser invariant: latency stays as the last field."""
        recorder, _ = _make_recorder(latency_sample_cap=10000)

        # Seed 100 samples with distinct known latencies
        await asyncio.gather(
            *[
                recorder.record_request(
                    method="GET", path="/x", status_code=200, duration_ms=i + 1
                )
                for i in range(100)
            ]
        )

        percentiles = await recorder.get_latency_percentiles()
        # Non-zero p50/p95/p99 means the parser still recovers latency
        # (i.e. rsplit(':', 1)[1] produced a valid float).
        assert percentiles["p50"] > 0
        assert percentiles["p95"] > 0
        assert percentiles["p99"] > 0
        # And the p99 of {1..100} should be near the top of the range
        assert percentiles["p99"] >= 90

    @pytest.mark.asyncio
    async def test_latency_sample_cap_honours_setting(self):
        """Cap=50 means ZCARD never exceeds 50 regardless of writes."""
        recorder, zsets = _make_recorder(latency_sample_cap=50)

        await asyncio.gather(
            *[
                recorder.record_request(
                    method="GET", path="/x", status_code=200, duration_ms=i
                )
                for i in range(120)
            ]
        )

        zset = zsets["metrics:latency:samples"]
        assert zset.zcard() == 50, (
            f"Expected ZCARD == 50 (cap), got {zset.zcard()}"
        )
