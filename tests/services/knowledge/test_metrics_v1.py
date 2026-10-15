"""Phase 0 /metrics v1 (PRD §7): retrieval hot-path counters, latency,
rerank degrade counter, ingestion queue depth gauge, and the /metrics route.

The Prometheus registry is process-global, so every assertion measures a
delta or sets an absolute gauge value instead of assuming a clean registry.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.observability import metrics as kb_metrics
from knowledge_service.services.knowledge import retrieval_service as retrieval_module
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from prometheus_client import REGISTRY


@pytest.fixture(autouse=True)
def _use_explicit_test_idempotency(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ENVIRONMENT", "test")
    monkeypatch.setenv("INTERNAL_IDEMPOTENCY_BACKEND", "memory")


def _sample(name: str, **labels: str) -> float:
    return float(REGISTRY.get_sample_value(name, labels) or 0.0)


# ---------------------------------------------------------------------------
# metrics module: label vocab and containment
# ---------------------------------------------------------------------------


def test_record_retrieval_counts_and_observes_latency() -> None:
    before = _sample("kb_retrievals_total", mode="hybrid", cache="miss")
    count_before = _sample("kb_retrieve_latency_seconds_count", mode="hybrid")
    sum_before = _sample("kb_retrieve_latency_seconds_sum", mode="hybrid")

    kb_metrics.record_retrieval(
        "hybrid", cache_hit=False, duration_seconds=0.125
    )

    assert (
        _sample("kb_retrievals_total", mode="hybrid", cache="miss") - before
        == 1.0
    )
    assert (
        _sample("kb_retrieve_latency_seconds_count", mode="hybrid")
        - count_before
        == 1.0
    )
    observed = (
        _sample("kb_retrieve_latency_seconds_sum", mode="hybrid") - sum_before
    )
    assert observed == pytest.approx(0.125)


def test_record_retrieval_maps_cache_hit_label() -> None:
    before = _sample("kb_retrievals_total", mode="dense", cache="hit")
    kb_metrics.record_retrieval("dense", cache_hit=True, duration_seconds=0.01)
    assert (
        _sample("kb_retrievals_total", mode="dense", cache="hit") - before
        == 1.0
    )


@pytest.mark.parametrize(
    ("mode", "expected"),
    [
        ("vector", "dense"),  # legacy alias, same normalization as retrieve
        ("VECTOR", "dense"),
        (" dense ", "dense"),
        ("", "other"),
        (None, "other"),
        ("semantic", "other"),
    ],
)
def test_record_retrieval_bounds_mode_label(mode: object, expected: str) -> None:
    before = _sample("kb_retrievals_total", mode=expected, cache="miss")
    kb_metrics.record_retrieval(mode, cache_hit=False, duration_seconds=0.0)
    assert (
        _sample("kb_retrievals_total", mode=expected, cache="miss") - before
        == 1.0
    )


def test_record_retrieval_never_raises_on_garbage() -> None:
    kb_metrics.record_retrieval(
        object(), cache_hit=None, duration_seconds=object()  # type: ignore[arg-type]
    )


def test_record_rerank_degraded_counts_known_and_maps_unknown() -> None:
    reasons = {
        "budget_exhausted": _sample(
            "kb_rerank_degraded_total", reason="budget_exhausted"
        ),
        "timeout": _sample("kb_rerank_degraded_total", reason="timeout"),
        "error": _sample("kb_rerank_degraded_total", reason="error"),
    }

    kb_metrics.record_rerank_degraded("budget_exhausted")
    kb_metrics.record_rerank_degraded("timeout")
    kb_metrics.record_rerank_degraded("provider exploded")
    kb_metrics.record_rerank_degraded(None)

    assert (
        _sample("kb_rerank_degraded_total", reason="budget_exhausted")
        - reasons["budget_exhausted"]
        == 1.0
    )
    assert (
        _sample("kb_rerank_degraded_total", reason="timeout")
        - reasons["timeout"]
        == 1.0
    )
    # Unknown and empty reasons fold into the bounded "error" label.
    assert (
        _sample("kb_rerank_degraded_total", reason="error") - reasons["error"]
        == 2.0
    )


def test_set_ingestion_queue_depth_sets_gauge_and_rejects_bad_values() -> None:
    kb_metrics.set_ingestion_queue_depth(7)
    assert _sample("kb_ingestion_queue_depth") == 7.0

    # Invalid values are ignored, never raised, and never clobber the gauge.
    kb_metrics.set_ingestion_queue_depth(-3)
    assert _sample("kb_ingestion_queue_depth") == 7.0
    kb_metrics.set_ingestion_queue_depth(float("nan"))
    assert _sample("kb_ingestion_queue_depth") == 7.0
    kb_metrics.set_ingestion_queue_depth("junk")
    assert _sample("kb_ingestion_queue_depth") == 7.0
    kb_metrics.set_ingestion_queue_depth(None)
    assert _sample("kb_ingestion_queue_depth") == 0.0


# ---------------------------------------------------------------------------
# retrieval_service wiring: every served retrieve is recorded exactly once
# ---------------------------------------------------------------------------


class _RecordingMetrics:
    def __init__(self) -> None:
        self.retrievals: list[tuple[str, bool, float]] = []
        self.rerank_degraded: list[object] = []

    def record_retrieval(
        self, mode: object, *, cache_hit: bool, duration_seconds: float
    ) -> None:
        self.retrievals.append((str(mode), cache_hit, duration_seconds))

    def record_rerank_degraded(self, reason: object) -> None:
        self.rerank_degraded.append(reason)


def _make_service(monkeypatch: pytest.MonkeyPatch) -> tuple[
    RetrievalService,
    dict[str, Any],
    SimpleNamespace,
    SimpleNamespace,
]:
    service = RetrievalService.__new__(RetrievalService)
    service.settings = None
    service.db = None
    service.vector_store = None

    dataset: dict[str, Any] = {
        "dataset_id": "ds-1",
        "tenant_id": "t-1",
        "index_config": {},
        "content_revision": 3,
        "collection_name": "kb-metrics",
    }

    async def _require_dataset_access(_user, _dataset_id, required=None):
        return dataset

    ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    service._ks = ks  # noqa: SLF001 - parent-service back reference

    async def _retrieve_queries(**_kwargs):
        return [], {"pipeline_stages": []}

    monkeypatch.setattr(service, "_retrieve_queries", _retrieve_queries)
    monkeypatch.setattr(
        service, "_record_retrieval_telemetry", lambda **_kwargs: None
    )

    user = SimpleNamespace(user_id="")
    return service, dataset, ks, user


async def test_retrieve_records_cache_miss_metric(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _dataset, _ks, user = _make_service(monkeypatch)
    stub = _RecordingMetrics()
    monkeypatch.setattr(retrieval_module, "_metrics", stub)

    results, meta = await service.retrieve(user, "ds-1", "hello", mode="dense")

    assert results == []
    assert meta["retrieval_cache_hit"] is False
    assert len(stub.retrievals) == 1
    mode, cache_hit, duration = stub.retrievals[0]
    assert mode == "dense"
    assert cache_hit is False
    assert isinstance(duration, float)
    assert duration >= 0.0


async def test_retrieve_records_cache_hit_metric(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _dataset, ks, _user = _make_service(monkeypatch)
    stub = _RecordingMetrics()
    monkeypatch.setattr(retrieval_module, "_metrics", stub)

    user = SimpleNamespace(user_id="u-1")
    cached_meta: dict[str, Any] = {"pipeline_stages": []}

    async def _cache_get(_key):
        return [], cached_meta

    async def _cache_set(_key, _results, _meta):
        raise AssertionError("cache_set must not run on a cache hit")

    ks._get_cached_retrieval = _cache_get  # noqa: SLF001
    ks._set_cached_retrieval = _cache_set  # noqa: SLF001
    ks._compute_retrieval_query_fingerprint = (  # noqa: SLF001
        lambda _payload: "fp-1"
    )

    async def _no_collection_check(_dataset, _dataset_id):
        return None

    async def _no_generation_check(*_args, **_kwargs):
        return None

    async def _no_segments(**_kwargs):
        return set()

    monkeypatch.setattr(service, "_require_collection_readable", _no_collection_check)
    monkeypatch.setattr(
        service, "_require_unchanged_retrieval_generation", _no_generation_check
    )
    monkeypatch.setattr(service, "_active_segment_ids", _no_segments)

    results, meta = await service.retrieve(user, "ds-1", "hello", mode="hybrid")

    assert results == []
    assert meta["retrieval_cache_hit"] is True
    assert len(stub.retrievals) == 1
    mode, cache_hit, duration = stub.retrievals[0]
    assert mode == "hybrid"
    assert cache_hit is True
    assert duration >= 0.0


async def test_retrieve_failure_is_not_recorded_as_served(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _dataset, _ks, user = _make_service(monkeypatch)
    stub = _RecordingMetrics()
    monkeypatch.setattr(retrieval_module, "_metrics", stub)

    async def _explode(**_kwargs):
        raise RuntimeError("vector store down")

    monkeypatch.setattr(service, "_retrieve_queries", _explode)

    with pytest.raises(RuntimeError, match="vector store down"):
        await service.retrieve(user, "ds-1", "hello", mode="dense")

    # A failed retrieval is not a served retrieval: no counter increment.
    assert stub.retrievals == []


# ---------------------------------------------------------------------------
# /metrics route
# ---------------------------------------------------------------------------


def _build_app(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main

    return main.create_app(main.Settings())


def test_metrics_route_serves_prometheus_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fastapi.testclient import TestClient

    app = _build_app(monkeypatch)
    kb_metrics.set_ingestion_queue_depth(0)
    response = TestClient(app).get("/metrics")

    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/plain")
    body = response.text
    for name in (
        "kb_retrieve_latency_seconds",
        "kb_retrievals_total",
        "kb_rerank_degraded_total",
        "kb_ingestion_queue_depth",
    ):
        assert name in body


def test_metrics_route_refreshes_queue_depth_from_db(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fastapi.testclient import TestClient

    app = _build_app(monkeypatch)

    calls: list[int] = []

    class _Db:
        async def count_queued_documents(self) -> int:
            calls.append(1)
            return 12

    app.state.db = _Db()
    response = TestClient(app).get("/metrics")

    assert response.status_code == 200
    assert calls == [1]
    assert "kb_ingestion_queue_depth 12.0" in response.text


def test_metrics_route_survives_db_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fastapi.testclient import TestClient

    app = _build_app(monkeypatch)

    class _BrokenDb:
        async def count_queued_documents(self) -> int:
            raise RuntimeError("pool exhausted")

    kb_metrics.set_ingestion_queue_depth(0)
    app.state.db = _BrokenDb()
    response = TestClient(app).get("/metrics")

    # Scrape must never fail the request path; last good gauge stays.
    assert response.status_code == 200
    assert "kb_ingestion_queue_depth 0.0" in response.text
