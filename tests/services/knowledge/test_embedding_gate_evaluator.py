"""Offline unit tests for the T3 shadow-vs-serving evaluation gate.

The evaluator is exercised through its injectable seams (embedder factory,
search fn) against an in-memory fake DB/vector surface — no PG, no Qdrant, no
network. Pinned behaviour:

* pass only when the shadow self-retrieval rate clears the floor AND stays
  within tolerance of the serving baseline;
* every measurement failure mode (empty probe corpus, unqueryable collection,
  missing bindings, crashed embedder) answers ``passed=False`` with a
  ``reason`` — the gate fails closed, never on a guess;
* the verdict carries the audit metrics run_gate stores with the migration.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.embedding_gate import (
    SAMPLE_SEGMENTS_SQL,
    shadow_serving_gate_evaluator,
)

SHADOW_COLLECTION = "kb_ds-a_1536_vtarg"
SERVING_COLLECTION = "kb_ds-a_1024_vsrv"

DATASET = {"dataset_id": "ds-a", "tenant_id": "tenant-a"}

TARGET_BINDING = {
    "binding_id": "b-target",
    "dataset_id": "ds-a",
    "tenant_id": "tenant-a",
    "collection_name": SHADOW_COLLECTION,
    "embedding_provider": "local",
    "embedding_model": "qwen3-embedding-4b",
    "embedding_model_version": "2026-08",
    "embedding_dimension": 1536,
}
SOURCE_BINDING = {
    "binding_id": "b-source",
    "dataset_id": "ds-a",
    "tenant_id": "tenant-a",
    "collection_name": SERVING_COLLECTION,
    "embedding_provider": "dashscope",
    "embedding_model": "text-embedding-v4",
    "embedding_model_version": "2026-01",
    "embedding_dimension": 1024,
}


def _context(**overrides: Any) -> dict[str, Any]:
    context: dict[str, Any] = {
        "migration": {"migration_id": "m-1", "dataset_id": "ds-a", "state": "gating"},
        "dataset_id": "ds-a",
        "target_binding": dict(TARGET_BINDING),
        "source_binding": dict(SOURCE_BINDING),
    }
    context.update(overrides)
    return context


def _segments(count: int) -> list[dict[str, str]]:
    return [
        {"segment_id": f"seg-{i}", "vector_id": f"vec-{i}", "text": f"chunk text {i}"}
        for i in range(count)
    ]


class FakeDB:
    def __init__(self, rows: list[dict[str, str]]) -> None:
        self.rows = rows
        self.calls: list[tuple[str, tuple[Any, ...]]] = []

    async def fetch(self, sql: str, *args: Any) -> list[dict[str, str]]:
        self.calls.append((sql, args))
        return list(self.rows)


class FakeEmbedder:
    """Index-encodes probes as one-hot vectors so fake search can decode them."""

    def __init__(self, model: str, *, fail: bool = False) -> None:
        self.model = model
        self.fail = fail
        self.closed = False

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        if self.fail:
            raise RuntimeError(f"{self.model} provider credential rejected")
        count = len(texts)
        return [[1.0 if j == i else 0.0 for j in range(count)] for i in range(count)]

    async def close(self) -> None:
        self.closed = True


def _fakes(
    *,
    shadow_hits: set[int],
    serving_hits: set[int] | None = None,
    fail_collections: tuple[str, ...] = (),
):
    """Returns (segment_rows, embedder_factory, search_fn, search_calls, embedders)."""
    if serving_hits is None:
        serving_hits = set()
    search_calls: list[dict[str, Any]] = []
    embedders: list[FakeEmbedder] = []

    async def embedder_factory(identity: dict[str, Any], tenant_id: str) -> FakeEmbedder:
        embedder = FakeEmbedder(str(identity.get("embedding_model") or ""))
        embedders.append(embedder)
        return embedder

    async def search_fn(
        *, collection_name: str, query_vector: list[float], tenant_id: str, dataset_id: str, top_k: int
    ) -> list[Any]:
        search_calls.append(
            {
                "collection_name": collection_name,
                "tenant_id": tenant_id,
                "dataset_id": dataset_id,
                "top_k": top_k,
            }
        )
        if collection_name in fail_collections:
            raise RuntimeError(f"collection '{collection_name}' is not readable")
        index = query_vector.index(max(query_vector))
        segment_id = f"seg-{index}"
        self_hits = shadow_hits if collection_name == SHADOW_COLLECTION else serving_hits
        if index in self_hits:
            return [SimpleNamespace(point_id=f"vec-{index}", score=0.91, payload={"segment_id": segment_id})]
        return [SimpleNamespace(point_id="vec-noise", score=0.42, payload={"segment_id": "seg-noise"})]

    return embedder_factory, search_fn, search_calls, embedders


async def _evaluate(
    segments: list[dict[str, str]],
    *,
    sample_size: int = 16,
    top_k: int = 5,
    tolerance: float = 0.10,
    floor: float = 0.80,
    shadow_hits: set[int],
    serving_hits: set[int] | None = None,
    fail_collections: tuple[str, ...] = (),
    context: dict[str, Any] | None = None,
):
    embedder_factory, search_fn, search_calls, embedders = _fakes(
        shadow_hits=shadow_hits,
        serving_hits=serving_hits,
        fail_collections=fail_collections,
    )
    db = FakeDB(segments)
    svc = SimpleNamespace(db=db, vector_store=None, _resolve_embedding_config=None)
    evaluate = await shadow_serving_gate_evaluator(
        svc,
        DATASET,
        sample_size=sample_size,
        top_k=top_k,
        tolerance=tolerance,
        floor=floor,
        embedder_factory=embedder_factory,
        search_fn=search_fn,
    )
    verdict = await evaluate(_context() if context is None else context)
    return verdict, db, search_calls, embedders


# ---------------------------------------------------------------- happy paths


@pytest.mark.asyncio
async def test_gate_passes_when_shadow_matches_serving() -> None:
    verdict, _db, search_calls, embedders = await _evaluate(
        _segments(4), shadow_hits={0, 1, 2, 3}, serving_hits={0, 1, 2, 3}
    )
    assert verdict["passed"] is True
    assert verdict["shadow_hit_rate"] == 1.0
    assert verdict["serving_hit_rate"] == 1.0
    assert verdict["samples"] == 4
    assert verdict["top_k"] == 5
    assert verdict["tolerance"] == pytest.approx(0.10)
    assert verdict["floor"] == pytest.approx(0.80)
    assert "reason" not in verdict
    # One probe per side per segment, against the right collections, pure-dense
    # top-k, tenant/dataset scoped.
    assert len(search_calls) == 8
    assert {c["collection_name"] for c in search_calls} == {SHADOW_COLLECTION, SERVING_COLLECTION}
    assert all(c["top_k"] == 5 and c["dataset_id"] == "ds-a" and c["tenant_id"] == "tenant-a" for c in search_calls)
    # Both throwaway embedders were closed after measurement.
    assert [e.model for e in embedders] == ["qwen3-embedding-4b", "text-embedding-v4"]
    assert all(e.closed for e in embedders)


@pytest.mark.asyncio
async def test_gate_uses_authoritative_enabled_segment_sample() -> None:
    verdict, db, _calls, _embedders = await _evaluate(
        _segments(2), sample_size=3, shadow_hits={0, 1}, serving_hits={0, 1}
    )
    assert verdict["passed"] is True
    sql, args = db.calls[0]
    assert sql is SAMPLE_SEGMENTS_SQL
    assert "s.enabled = TRUE" in sql and "d.enabled = TRUE" in sql
    assert "COALESCE(s.content_type, 'text') = 'text'" in sql
    assert args == ("ds-a", 3)


@pytest.mark.asyncio
async def test_gate_passes_with_relaxed_tolerance() -> None:
    # 3/4 shadow vs 4/4 serving passes with tolerance=0.5 and floor=0.5 …
    verdict, _db, _calls, _embedders = await _evaluate(
        _segments(4), shadow_hits={0, 1, 2}, serving_hits={0, 1, 2, 3},
        tolerance=0.50, floor=0.50,
    )
    assert verdict["shadow_hit_rate"] == pytest.approx(0.75)
    assert verdict["serving_hit_rate"] == pytest.approx(1.0)
    assert verdict["passed"] is True

    # … and the very same measurement fails at the default 0.10 tolerance
    # (floor relaxed so the regression branch is the one that fires).
    verdict, _db, _calls, _embedders = await _evaluate(
        _segments(4), shadow_hits={0, 1, 2}, serving_hits={0, 1, 2, 3}, floor=0.50
    )
    assert verdict["passed"] is False
    assert "regressed" in verdict["reason"]


# ------------------------------------------------------------- regression fails


@pytest.mark.asyncio
async def test_gate_fails_when_shadow_regresses_against_serving() -> None:
    verdict, _db, _calls, _embedders = await _evaluate(
        _segments(4), shadow_hits={0, 1}, serving_hits={0, 1, 2, 3}, floor=0.40, tolerance=0.10
    )
    assert verdict["passed"] is False
    assert verdict["shadow_hit_rate"] == pytest.approx(0.5)
    assert verdict["serving_hit_rate"] == pytest.approx(1.0)
    assert "regressed" in verdict["reason"]


@pytest.mark.asyncio
async def test_gate_fails_below_absolute_floor_even_when_matching_serving() -> None:
    # Both generations retrieve half the probes: no regression, but an
    # unmeasurably weak candidate still must not cut over.
    verdict, _db, _calls, _embedders = await _evaluate(
        _segments(4), shadow_hits={0, 1}, serving_hits={0, 1}, floor=0.80, tolerance=0.50
    )
    assert verdict["passed"] is False
    assert verdict["shadow_hit_rate"] == pytest.approx(0.5)
    assert verdict["serving_hit_rate"] == pytest.approx(0.5)
    assert "floor" in verdict["reason"]


# -------------------------------------------------------------- fail-closed path


@pytest.mark.asyncio
async def test_gate_fails_closed_with_no_probeable_segments() -> None:
    verdict, _db, search_calls, embedders = await _evaluate([], shadow_hits=set())
    assert verdict["passed"] is False
    assert verdict["samples"] == 0
    assert "no enabled text segments" in verdict["reason"]
    # Nothing measurable: the probe never touches the embedders or collections.
    assert search_calls == []
    assert embedders == []


@pytest.mark.asyncio
async def test_gate_fails_closed_when_shadow_collection_unqueryable() -> None:
    verdict, _db, _calls, embedders = await _evaluate(
        _segments(3), shadow_hits={0, 1, 2}, serving_hits={0, 1, 2},
        fail_collections=(SHADOW_COLLECTION,),
    )
    assert verdict["passed"] is False
    assert "not readable" in verdict["reason"]
    # The crashed measurement must still release the target embedder.
    assert embedders and embedders[0].closed


@pytest.mark.asyncio
async def test_gate_fails_closed_when_embedder_crashes() -> None:
    class ExplodingFactory:
        def __init__(self) -> None:
            self.calls = 0

        async def __call__(self, _identity: dict[str, Any], _tenant_id: str) -> FakeEmbedder:
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("target provider is not configured")
            return FakeEmbedder("source")

    factory = ExplodingFactory()
    _default_factory, search_fn, _calls, _embedders = _fakes(
        shadow_hits={0, 1}, serving_hits={0, 1}
    )
    db = FakeDB(_segments(2))
    svc = SimpleNamespace(db=db, vector_store=None, _resolve_embedding_config=None)
    evaluate = await shadow_serving_gate_evaluator(
        svc, DATASET, sample_size=16, embedder_factory=factory, search_fn=search_fn
    )
    verdict = await evaluate(_context())
    assert verdict["passed"] is False
    assert "target generation cannot embed" in verdict["reason"]


@pytest.mark.asyncio
async def test_gate_fails_closed_without_binding_context() -> None:
    verdict, _db, _calls, _embedders = await _evaluate(
        _segments(2),
        shadow_hits={0, 1},
        serving_hits={0, 1},
        context=_context(target_binding=None),
    )
    assert verdict["passed"] is False
    assert "missing dataset/serving/target collection bindings" in verdict["reason"]
