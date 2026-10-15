"""Query telemetry wiring (PRD C1 / Phase 0 quick win).

retrieve() must append one dataset_queries row per request via a
fire-and-forget task: independent transaction, never blocking and never
failing the retrieval itself.
"""

from __future__ import annotations

import asyncio
import hashlib
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.persistence.database import DatabaseStorage
from knowledge_service.persistence.datasets import DatasetPersistenceMixin
from knowledge_service.services.knowledge.retrieval_service import RetrievalService

USER = UserContext(user_id="user-a", tenant_id="tenant-a")
DATASET = {
    "dataset_id": "dataset-a",
    "tenant_id": "tenant-a",
    "index_config": {},
    "content_revision": 3,
}


def test_query_stage_timings_json_is_decoded_for_the_read_api() -> None:
    database = DatabaseStorage.__new__(DatabaseStorage)

    row = database._row_to_dict(
        {"stage_timings": '{"dense_search_ms": 2.5, "total_ms": 3.0}'}
    )

    assert row["stage_timings"] == {"dense_search_ms": 2.5, "total_ms": 3.0}


class TelemetryDatabase:
    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail
        self.records: list[dict[str, Any]] = []

    async def record_dataset_query(self, **kwargs: Any) -> bool:
        if self.fail:
            raise RuntimeError("postgres unavailable")
        self.records.append(kwargs)
        return True


class TelemetryKnowledge:
    async def require_dataset_access(self, _user, dataset_id, *, required):
        assert (dataset_id, required) == ("dataset-a", "viewer")
        return dict(DATASET)


def _make_service(monkeypatch, *, fail_insert: bool = False, with_record: bool = True):
    database = TelemetryDatabase(fail=fail_insert)
    if not with_record:
        database = SimpleNamespace()  # type: ignore[assignment]
    service = RetrievalService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = TelemetryKnowledge()  # type: ignore[assignment]

    async def _fake_retrieve_queries(**_kwargs: Any):
        return (
            [SimpleNamespace(segment_id="segment-a")],
            {"timings_ms": {"total_ms": 12.5}},
        )

    monkeypatch.setattr(service, "_retrieve_queries", _fake_retrieve_queries)
    return service, database


async def _drain_pending_tasks() -> None:
    for _ in range(5):
        await asyncio.sleep(0)


async def test_retrieve_records_query_telemetry_fire_and_forget(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database = _make_service(monkeypatch)

    results, meta = await service.retrieve(
        USER,
        "dataset-a",
        "  what   is retrieval? ",
        top_k=3,
        mode="hybrid",
    )

    await _drain_pending_tasks()

    assert [str(result.segment_id) for result in results] == ["segment-a"]
    assert meta["retrieval_cache_hit"] is False
    assert len(database.records) == 1
    record = database.records[0]
    assert record["dataset_id"] == "dataset-a"
    assert record["content"] == "what is retrieval?"
    assert record["source"] == "api"
    assert record["created_by"] == "user-a"
    assert record["created_by_role"] == "normal"
    metadata = record["metadata"]
    assert metadata["mode"] == "hybrid"
    assert metadata["top_k"] == 3
    assert metadata["hit_count"] == 1
    assert metadata["cache_hit"] is False
    assert metadata["stage_timings"] == {"total_ms": 12.5}
    # No cache fingerprint on this path: fall back to the normalized-query hash.
    assert metadata["query_fingerprint"] == hashlib.sha256(
        b"what is retrieval?"
    ).hexdigest()
    assert record["trace_id"] == meta["trace_id"]
    assert record["query_fingerprint"] == meta["query_fingerprint"]
    assert record["segment_ids"] == ["segment-a"]


async def test_each_request_gets_a_new_trace_but_same_query_fingerprint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database = _make_service(monkeypatch)
    _, first = await service.retrieve(USER, "dataset-a", " ＡＢＣ   Policy ")
    _, second = await service.retrieve(USER, "dataset-a", "abc policy")
    await _drain_pending_tasks()

    assert first["trace_id"] != second["trace_id"]
    assert first["query_fingerprint"] == second["query_fingerprint"]
    assert len(database.records) == 2


async def test_retrieve_survives_telemetry_insert_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database = _make_service(monkeypatch, fail_insert=True)

    results, meta = await service.retrieve(USER, "dataset-a", "query", top_k=5)

    await _drain_pending_tasks()

    assert len(results) == 1
    assert meta["retrieval_cache_hit"] is False
    assert database.records == []


async def test_retrieve_is_a_noop_for_telemetry_when_db_lacks_support(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database = _make_service(monkeypatch, with_record=False)

    results, _meta = await service.retrieve(USER, "dataset-a", "query", top_k=5)

    await _drain_pending_tasks()
    assert len(results) == 1
    assert not getattr(database, "records", None)


async def _wire_cache(
    monkeypatch: pytest.MonkeyPatch,
    service: RetrievalService,
    *,
    cache_get,
    cache_set,
) -> None:
    """Enable the standard retrieve() cache path on a stubbed service."""
    ks = service._ks  # noqa: SLF001 - parent-service back reference
    ks._get_cached_retrieval = cache_get  # type: ignore[attr-defined]
    ks._set_cached_retrieval = cache_set  # type: ignore[attr-defined]
    ks._compute_retrieval_query_fingerprint = (  # type: ignore[attr-defined]
        lambda _payload: "fp-cache"
    )

    async def _no_collection(_dataset, _dataset_id):
        return None

    async def _no_generation(*_args, **_kwargs):
        return None

    async def _all_active(**_kwargs):
        return set()

    monkeypatch.setattr(service, "_require_collection_readable", _no_collection)
    monkeypatch.setattr(
        service, "_require_unchanged_retrieval_generation", _no_generation
    )
    monkeypatch.setattr(service, "_active_segment_ids", _all_active)


async def test_degraded_rerank_result_is_never_cached(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A rerank degrade is transient state: report it live, never cache it.

    Caching a degraded result kept replaying ``rerank_degraded`` on every
    cache hit for the whole TTL — phantom telemetry long after the provider
    recovered (review-metrics-v1 follow-up).
    """
    service, database = _make_service(monkeypatch)
    cache_sets: list[dict[str, Any]] = []

    async def _cache_get(_key):
        return None

    async def _cache_set(_key, _results, meta):
        cache_sets.append(meta)

    await _wire_cache(monkeypatch, service, cache_get=_cache_get, cache_set=_cache_set)

    async def _degraded_queries(**_kwargs):
        return (
            [SimpleNamespace(segment_id="segment-a")],
            {"timings_ms": {}, "rerank_degraded": "timeout", "rerank_error": "boom"},
        )

    monkeypatch.setattr(service, "_retrieve_queries", _degraded_queries)

    user = UserContext(user_id="user-a", tenant_id="tenant-a")
    results, meta = await service.retrieve(
        user, "dataset-a", "query", top_k=3, mode="hybrid"
    )
    await _drain_pending_tasks()

    # The live degrade is still reported to the caller and telemetry once…
    assert meta["rerank_degraded"] == "timeout"
    assert database.records[0]["metadata"]["rerank_degraded"] == "timeout"
    # …but the degraded result never enters the cache.
    assert cache_sets == []


async def test_cache_hit_does_not_replay_stale_rerank_degrade(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A cache hit performed no rerank, so it must not report a degrade.

    Also covers entries written before the no-degraded-cache rule existed:
    the hit path strips the stale flag before telemetry and before return.
    """
    service, database = _make_service(monkeypatch)

    cached_meta: dict[str, Any] = {
        "pipeline_stages": [],
        "rerank_degraded": "timeout",
        "rerank_error": "boom",
    }

    async def _cache_get(_key):
        return [], cached_meta

    async def _cache_set(_key, _results, _meta):
        raise AssertionError("cache_set must not run on a cache hit")

    await _wire_cache(monkeypatch, service, cache_get=_cache_get, cache_set=_cache_set)

    user = UserContext(user_id="user-a", tenant_id="tenant-a")
    _results, meta = await service.retrieve(
        user, "dataset-a", "query", top_k=3, mode="hybrid"
    )
    await _drain_pending_tasks()

    assert meta["retrieval_cache_hit"] is True
    assert "rerank_degraded" not in meta
    assert "rerank_error" not in meta
    record = database.records[0]
    assert record["metadata"]["cache_hit"] is True
    assert "rerank_degraded" not in record["metadata"]


async def test_telemetry_metadata_prefers_pipeline_fingerprint_and_degrade_flag() -> None:
    service = RetrievalService(SimpleNamespace(), SimpleNamespace())  # type: ignore[arg-type]
    captured: dict[str, Any] = {}

    class _RecordingDatabase:
        async def record_dataset_query(self, **kwargs: Any) -> bool:
            captured.update(kwargs)
            return True

    service.db = _RecordingDatabase()  # type: ignore[assignment]

    service._record_retrieval_telemetry(
        user=USER,
        dataset_id="dataset-a",
        query="q",
        mode="dense",
        top_k=7,
        results=[SimpleNamespace(), SimpleNamespace()],
        meta={
            "trace_id": "6db4a38a-69b7-4eaf-a311-5955f54d06dd",
            "query_fingerprint": "a" * 64,
            "retrieval_cache_hit": True,
            "rerank_degraded": "timeout",
            "timings_ms": {"total_ms": 1.0},
        },
    )
    await _drain_pending_tasks()

    assert captured["metadata"]["query_fingerprint"] == "a" * 64
    assert captured["metadata"]["cache_hit"] is True
    assert captured["metadata"]["hit_count"] == 2
    assert captured["metadata"]["rerank_degraded"] == "timeout"


def test_telemetry_without_running_loop_is_dropped_silently() -> None:
    service = RetrievalService(SimpleNamespace(), SimpleNamespace())  # type: ignore[arg-type]
    calls: list[dict[str, Any]] = []

    class _Database:
        async def record_dataset_query(self, **kwargs: Any) -> bool:
            calls.append(kwargs)
            return True

    service.db = _Database()  # type: ignore[assignment]

    # Sync context: no running event loop — the record is dropped, not raised.
    service._record_retrieval_telemetry(
        user=USER,
        dataset_id="dataset-a",
        query="q",
        mode="dense",
        top_k=1,
        results=[],
        meta={},
    )
    assert calls == []


def test_telemetry_noop_when_service_has_no_db_attribute() -> None:
    # Partial fakes construct RetrievalService without full __init__.
    service = object.__new__(RetrievalService)

    service._record_retrieval_telemetry(
        user=USER,
        dataset_id="dataset-a",
        query="q",
        mode="dense",
        top_k=1,
        results=[],
        meta={},
    )  # must not raise


# ---------------------------------------------------------------------------
# Persistence helper: never raises, independent of caller state.
# ---------------------------------------------------------------------------


class _HelperHost(DatasetPersistenceMixin):
    def __init__(self, pool: Any) -> None:
        self._pool = pool


class _FakeConnection:
    def __init__(self, *, fail: bool) -> None:
        self.fail = fail
        self.executed: list[tuple[Any, ...]] = []

    async def execute(self, query: str, *params: Any) -> None:
        if self.fail:
            raise RuntimeError("insert failed")
        self.executed.append((query, *params))


class _FakeAcquire:
    def __init__(self, connection: _FakeConnection) -> None:
        self.connection = connection

    async def __aenter__(self) -> _FakeConnection:
        return self.connection

    async def __aexit__(self, *_exc: Any) -> bool:
        return False


class _FakePool:
    def __init__(self, connection: _FakeConnection) -> None:
        self.connection = connection

    def acquire(self) -> _FakeAcquire:
        return _FakeAcquire(self.connection)


class _Transaction:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_exc: Any) -> bool:
        return False


class _AtomicConnection(_FakeConnection):
    def __init__(self) -> None:
        super().__init__(fail=False)
        self.inserted = True
        self.fetches: list[tuple[Any, ...]] = []

    def transaction(self) -> _Transaction:
        return _Transaction()

    async def fetchval(self, query: str, *params: Any) -> int | None:
        self.fetches.append((query, *params))
        if self.inserted:
            self.inserted = False
            return 1
        return None


async def test_record_dataset_query_without_pool_is_a_silent_noop() -> None:
    host = _HelperHost(pool=None)
    assert await host.record_dataset_query(dataset_id="dataset-a", content="q") is False


async def test_record_dataset_query_swallows_insert_errors() -> None:
    host = _HelperHost(pool=_FakePool(_FakeConnection(fail=True)))
    assert await host.record_dataset_query(dataset_id="dataset-a", content="q") is False


async def test_record_dataset_query_writes_metadata_as_jsonb() -> None:
    connection = _FakeConnection(fail=False)
    host = _HelperHost(pool=_FakePool(connection))

    result = await host.record_dataset_query(
        dataset_id="dataset-a",
        content="查询",
        metadata={"mode": "hybrid", "hit_count": 0},
    )

    assert result is True
    query, *params = connection.executed[0]
    assert "INSERT INTO dataset_queries" in query
    assert "$7::jsonb" in query
    assert params[0] == "dataset-a"
    assert params[1] == "查询"
    assert '"hit_count": 0' in params[6]
    assert "\\u67e5" not in params[6]  # ensure_ascii=False keeps CJK readable


async def test_structured_observation_increments_hits_only_on_first_trace_insert() -> None:
    connection = _AtomicConnection()
    host = _HelperHost(pool=_FakePool(connection))
    kwargs = {
        "dataset_id": "dataset-a",
        "content": "query",
        "trace_id": "d04d53c8-acde-49d0-b3eb-49890dbd5673",
        "query_fingerprint": "a" * 64,
        "mode": "hybrid",
        "top_k": 5,
        "hit_count": 1,
        "stage_timings": {"total_ms": 1},
        "segment_ids": ["segment-a", "segment-a"],
    }

    assert await host.record_dataset_query(**kwargs) is True
    assert await host.record_dataset_query(**kwargs) is True

    updates = [item for item in connection.executed if "UPDATE segments" in item[0]]
    assert len(updates) == 1
    assert updates[0][2] == ["segment-a"]
