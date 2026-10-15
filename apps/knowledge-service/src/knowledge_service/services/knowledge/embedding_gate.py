"""PRD T3: the real evaluation gate for a blue-green embedding migration.

``run_gate`` (embedding_migration.py) only enforces *that* a passing verdict
exists; this module supplies the missing evaluator: a shadow-vs-serving
self-retrieval measurement over a random sample of the dataset's authoritative
enabled PostgreSQL chunks.

Method (per sampled chunk, treated as its own one-document query):

1. embed the stored text with the *target* (candidate) generation embedder and
   dense-query the shadow collection; record whether the chunk's own
   ``segment_id`` is retrieved in top-k  -> shadow self-retrieval hit-rate;
2. repeat with the *source* (serving) generation embedder against the serving
   collection -> serving baseline hit-rate;
3. ``passed`` iff the shadow rate is above the absolute ``floor`` AND within
   ``tolerance`` of the serving rate. A candidate that cannot even retrieve
   its own source text, or that measurably regresses against the generation
   in production, must not cut over.

The gate fails closed: no segments to probe, an unqueryable collection, or a
crashed measurement all yield ``passed=False`` with an explanatory ``reason``
— an unmeasurable migration is never treated as a measured-and-good one.

Callables are injectable (``embedder_factory``, ``search_fn``,
``segment_sampler``) so the evaluator is unit-testable offline; the
production defaults use ``svc.db`` / ``svc.vector_store`` / the server-owned
``svc._resolve_embedding_config`` + ``create_embedding`` path.

Injected-callable contract:

* ``embedder_factory(identity: dict, tenant_id: str) -> embedder | awaitable``
  where the embedder exposes ``async embed_documents(texts) -> list[list[float]]``
  and ``async close()``;
* ``search_fn(collection_name=..., query_vector=..., tenant_id=..., dataset_id=...,
  top_k=...) -> awaitable[list[hit]]`` where each hit is either a
  ``VectorSearchHit`` or a mapping carrying ``segment_id`` (directly or under
  ``payload``);
* ``segment_sampler(dataset_id, tenant_id, limit) -> awaitable[list[dict]]``
  with ``segment_id`` and ``text`` keys per row.
"""

from __future__ import annotations

import contextlib
import logging
from collections.abc import Mapping
from typing import Any

from .common import maybe_await
from .embedding_migration import GateEvaluator, embedding_identity

logger = logging.getLogger("knowledge_service.services.knowledge.embedding_gate")

DEFAULT_SAMPLE_SIZE = 16
DEFAULT_TOP_K = 5
DEFAULT_TOLERANCE = 0.10
DEFAULT_FLOOR = 0.80

# Same authority as EmbeddingVersionStore's pending/backfill enumeration:
# enabled, non-archived, text segments only. ``random()`` sampling keeps the
# probe honest across the whole corpus instead of always re-testing the head.
SAMPLE_SEGMENTS_SQL = """
                SELECT s.segment_id,
                       COALESCE(NULLIF(s.vector_id, ''), s.segment_id) AS vector_id,
                       s.text
                FROM segments s
                JOIN documents d ON d.document_id = s.document_id
                WHERE s.dataset_id = $1
                  AND s.enabled = TRUE
                  AND d.enabled = TRUE
                  AND COALESCE(d.archived, FALSE) = FALSE
                  AND COALESCE(s.content_type, 'text') = 'text'
                  AND COALESCE(s.text, '') <> ''
                ORDER BY random()
                LIMIT $2
"""


def _hit_segment_id(hit: Any) -> str:
    """Best-effort segment identity of one search hit (VectorSearchHit or dict)."""
    if isinstance(hit, Mapping):
        payload = hit.get("payload")
        payload = payload if isinstance(payload, Mapping) else {}
        return str(
            payload.get("segment_id") or hit.get("segment_id") or hit.get("point_id") or ""
        ).strip()
    payload = getattr(hit, "payload", None)
    payload = payload if isinstance(payload, Mapping) else {}
    if payload.get("segment_id"):
        return str(payload["segment_id"]).strip()
    return str(getattr(hit, "point_id", "") or "").strip()


async def _close_quietly(embedder: Any) -> None:
    close = getattr(embedder, "close", None)
    if not callable(close):
        return
    with contextlib.suppress(Exception):
        await maybe_await(close())


def _default_verdict(sample_size: int, top_k: int, tolerance: float, floor: float) -> dict[str, Any]:
    return {
        "evaluator": "shadow_serving_self_retrieval",
        "passed": False,
        "samples": 0,
        "top_k": int(top_k),
        "tolerance": float(tolerance),
        "floor": float(floor),
        "shadow_hit_rate": None,
        "serving_hit_rate": None,
    }


async def shadow_serving_gate_evaluator(
    svc: Any,
    dataset: Mapping[str, Any],
    *,
    sample_size: int = DEFAULT_SAMPLE_SIZE,
    top_k: int = DEFAULT_TOP_K,
    tolerance: float = DEFAULT_TOLERANCE,
    floor: float = DEFAULT_FLOOR,
    embedder_factory: Any | None = None,
    search_fn: Any | None = None,
    segment_sampler: Any | None = None,
) -> GateEvaluator:
    """Build the ``run_gate`` evaluator for one dataset (see module docstring)."""

    dataset = dict(dataset or {})

    async def _sample_segments(dataset_id: str, tenant_id: str, limit: int) -> list[dict[str, Any]]:
        if segment_sampler is not None:
            return list(await maybe_await(segment_sampler(dataset_id, tenant_id, limit)) or [])
        fetch = getattr(getattr(svc, "db", None), "fetch", None)
        if not callable(fetch):
            raise RuntimeError("knowledge service database authority is unavailable for sampling")
        rows = await maybe_await(fetch(SAMPLE_SEGMENTS_SQL, dataset_id, int(limit)))
        return [dict(row) for row in rows or []]

    async def _build_embedder(identity: dict[str, Any], tenant_id: str) -> Any:
        if embedder_factory is not None:
            return await maybe_await(embedder_factory(identity, tenant_id))
        from .embedding import create_embedding

        resolve = getattr(svc, "_resolve_embedding_config", None)
        if not callable(resolve):
            raise RuntimeError("knowledge service cannot resolve server-owned embedding config")
        config = await maybe_await(
            resolve(
                provider=str(identity.get("embedding_provider") or ""),
                model=str(identity.get("embedding_model") or ""),
                embedding_config={},
                tenant_id=tenant_id,
            )
        )
        dimension = int(identity.get("embedding_dimension") or 0) or None
        return create_embedding(config, dimension=dimension)

    async def _dense_search(
        *, collection_name: str, query_vector: list[float], tenant_id: str, dataset_id: str,
    ) -> list[Any]:
        kwargs = {
            "collection_name": collection_name,
            "query_vector": list(query_vector),
            "tenant_id": tenant_id or None,
            "dataset_id": dataset_id or None,
            "top_k": int(top_k),
        }
        if search_fn is not None:
            return list(await maybe_await(search_fn(**kwargs)) or [])
        search = getattr(getattr(svc, "vector_store", None), "hybrid_search_native", None)
        if not callable(search):
            raise RuntimeError("vector store search authority is unavailable")
        # Pure dense probe: sparse channels deliberately empty (vector_store.py
        # contract), so the measurement isolates the embedding generation.
        return list(
            await maybe_await(
                search(
                    collection_name,
                    list(query_vector),
                    [],
                    [],
                    top_k=int(top_k),
                    tenant_id=tenant_id or None,
                    dataset_id=dataset_id or None,
                    with_payload=True,
                )
            )
            or []
        )

    async def _self_retrieval_rate(
        *,
        collection_name: str,
        vectors: list[list[float]],
        segments: list[dict[str, Any]],
        tenant_id: str,
        dataset_id: str,
    ) -> float:
        found = 0
        for vector, segment in zip(vectors, segments, strict=True):
            hits = await _dense_search(
                collection_name=collection_name,
                query_vector=list(vector),
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
            segment_id = str(segment.get("segment_id") or "").strip()
            if any(_hit_segment_id(hit) == segment_id for hit in hits):
                found += 1
        return found / len(segments)

    async def evaluate(context: dict[str, Any]) -> dict[str, Any]:
        verdict = _default_verdict(sample_size, top_k, tolerance, floor)
        try:
            context = dict(context or {})
            dataset_id = str(
                context.get("dataset_id") or dataset.get("dataset_id") or ""
            ).strip()
            target_binding = context.get("target_binding") or {}
            source_binding = context.get("source_binding") or {}
            tenant_id = str(
                target_binding.get("tenant_id")
                or source_binding.get("tenant_id")
                or dataset.get("tenant_id")
                or ""
            ).strip()
            shadow_collection = str(target_binding.get("collection_name") or "").strip()
            serving_collection = str(source_binding.get("collection_name") or "").strip()
            if not dataset_id or not shadow_collection or not serving_collection:
                verdict["reason"] = (
                    "gate context is missing dataset/serving/target collection bindings; "
                    "cannot measure the candidate generation"
                )
                return verdict

            segments = [
                dict(row)
                for row in await _sample_segments(dataset_id, tenant_id, sample_size)
                if str((row or {}).get("segment_id") or "").strip()
                and str((row or {}).get("text") or "").strip()
            ]
            verdict["samples"] = len(segments)
            if not segments:
                verdict["reason"] = (
                    "no enabled text segments available to probe; a migration that "
                    "cannot be measured must not cut over"
                )
                return verdict

            texts = [str(row.get("text")) for row in segments]
            try:
                target_embedder = await _build_embedder(
                    embedding_identity(target_binding), tenant_id
                )
            except Exception as exc:
                verdict["reason"] = (
                    f"target generation cannot embed the probe corpus: "
                    f"{type(exc).__name__}: {exc}"
                )
                return verdict
            try:
                try:
                    target_vectors = await maybe_await(target_embedder.embed_documents(texts))
                except Exception as exc:
                    verdict["reason"] = (
                        f"target generation cannot embed the probe corpus: "
                        f"{type(exc).__name__}: {exc}"
                    )
                    return verdict
                if len(target_vectors) != len(segments) or any(
                    vector is None or len(vector) <= 0 for vector in target_vectors
                ):
                    verdict["reason"] = (
                        f"target embedder returned {len(target_vectors)} vectors "
                        f"for {len(segments)} probes"
                    )
                    return verdict
                verdict["shadow_hit_rate"] = round(
                    await _self_retrieval_rate(
                        collection_name=shadow_collection,
                        vectors=[list(v) for v in target_vectors],
                        segments=segments,
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                    ),
                    6,
                )
            finally:
                await _close_quietly(target_embedder)

            try:
                source_embedder = await _build_embedder(
                    embedding_identity(source_binding), tenant_id
                )
            except Exception as exc:
                verdict["reason"] = (
                    f"serving generation cannot embed the probe corpus: "
                    f"{type(exc).__name__}: {exc}"
                )
                return verdict
            try:
                try:
                    source_vectors = await maybe_await(source_embedder.embed_documents(texts))
                except Exception as exc:
                    verdict["reason"] = (
                        f"serving generation cannot embed the probe corpus: "
                        f"{type(exc).__name__}: {exc}"
                    )
                    return verdict
                if len(source_vectors) != len(segments) or any(
                    vector is None or len(vector) <= 0 for vector in source_vectors
                ):
                    verdict["reason"] = (
                        f"serving embedder returned {len(source_vectors)} vectors "
                        f"for {len(segments)} probes"
                    )
                    return verdict
                verdict["serving_hit_rate"] = round(
                    await _self_retrieval_rate(
                        collection_name=serving_collection,
                        vectors=[list(v) for v in source_vectors],
                        segments=segments,
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                    ),
                    6,
                )
            finally:
                await _close_quietly(source_embedder)

            shadow_rate = float(verdict["shadow_hit_rate"])
            serving_rate = float(verdict["serving_hit_rate"])
            verdict["passed"] = bool(
                shadow_rate >= floor and shadow_rate >= serving_rate - tolerance
            )
            if not verdict["passed"]:
                if shadow_rate < floor:
                    verdict["reason"] = (
                        f"shadow self-retrieval hit-rate {shadow_rate:.3f} is below "
                        f"the required floor {floor:.3f}"
                    )
                else:
                    verdict["reason"] = (
                        f"shadow self-retrieval hit-rate {shadow_rate:.3f} regressed "
                        f"against serving {serving_rate:.3f} beyond tolerance {tolerance:.3f}"
                    )
            return verdict
        except Exception as exc:
            # Fail closed, but never crash the gate: run_gate also records the
            # failure; a clear unmeasured verdict is the better audit artifact.
            logger.warning("embedding gate measurement crashed", exc_info=True)
            verdict["passed"] = False
            verdict["reason"] = f"gate measurement failed: {type(exc).__name__}: {exc}"
            return verdict

    return evaluate
