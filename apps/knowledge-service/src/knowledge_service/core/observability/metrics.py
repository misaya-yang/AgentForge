"""Prometheus metrics for the knowledge service (PRD §7 Phase 0 / T7-1).

First version: the retrieval hot path and the durable ingestion queue depth.
Embedding-call errors and worker claim/contention gauges land with T7 once
their host modules are instrumented.

Cardinality discipline: every label comes from a fixed vocabulary below.
Metric construction happens at import time against the default registry so
``/metrics`` scrapes need no synchronization and workers/API processes export
the same names.
"""

from __future__ import annotations

from prometheus_client import Counter, Gauge, Histogram

RETRIEVE_MODES = frozenset({"bm25", "dense", "hybrid", "keyword"})
# "vector" is the legacy alias the retrieve contract normalizes to dense.
_MODE_ALIASES = {"vector": "dense"}
CACHE_OUTCOMES = ("hit", "miss")
RERANK_DEGRADE_REASONS = frozenset({"budget_exhausted", "error", "timeout"})
BM25_V2_READINESS_OUTCOMES = frozenset({"hit", "miss", "failure"})

retrieve_latency_seconds = Histogram(
    "kb_retrieve_latency_seconds",
    "Wall-clock latency of the /retrieve entrypoint, including cache lookup.",
    ("mode",),
    buckets=(0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0),
)

retrievals_total = Counter(
    "kb_retrievals",
    "Retrieval requests served, by mode and cache outcome.",
    ("mode", "cache"),
)

rerank_degraded_total = Counter(
    "kb_rerank_degraded",
    "Rerank stages served degraded (fusion order kept), by reason.",
    ("reason",),
)

ingestion_queue_depth = Gauge(
    "kb_ingestion_queue_depth",
    "Documents dispatchable from the durable ingestion queue.",
)

bm25_v2_readiness_total = Counter(
    "kb_bm25_v2_readiness",
    "BM25 v2 readiness checks by cache and verification outcome.",
    ("outcome",),
)


def _bounded_mode(mode: object) -> str:
    value = str(mode or "").strip().lower()
    value = _MODE_ALIASES.get(value, value)
    return value if value in RETRIEVE_MODES else "other"


def record_retrieval(mode: object, *, cache_hit: bool, duration_seconds: float) -> None:
    """Count one served retrieval and observe its latency.

    Never raises into the request path: metrics failures must not break
    retrieval (same containment contract as query telemetry, PRD C1).
    """
    try:
        bounded_mode = _bounded_mode(mode)
        retrievals_total.labels(
            mode=bounded_mode, cache="hit" if cache_hit else "miss"
        ).inc()
        retrieve_latency_seconds.labels(mode=bounded_mode).observe(
            max(0.0, float(duration_seconds))
        )
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass


def record_rerank_degraded(reason: object) -> None:
    try:
        value = str(reason or "")
        bounded_reason = value if value in RERANK_DEGRADE_REASONS else "error"
        rerank_degraded_total.labels(reason=bounded_reason).inc()
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass


def set_ingestion_queue_depth(depth: object) -> None:
    try:
        value = float(depth or 0)
        if value >= 0 and value == value and value != float("inf"):
            ingestion_queue_depth.set(value)
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass


def record_bm25_v2_readiness(outcome: object) -> None:
    try:
        value = str(outcome or "failure")
        bounded = value if value in BM25_V2_READINESS_OUTCOMES else "failure"
        bm25_v2_readiness_total.labels(outcome=bounded).inc()
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass
