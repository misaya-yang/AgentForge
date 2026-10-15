"""Prometheus metrics for the durable ingestion + embedding pipeline (PRD T7-1).

``metrics.py`` owns the retrieval hot path and the queue-depth gauge; this
module owns the worker/embedder side of the same ``/metrics`` surface: claim
contention, per-verb ingest counts, per-mode ingest duration, stuck-recovery
replays, and embedding-call outcomes. The split keeps the two teams' edits in
separate files — neither module imports the other.

Every metric name is frozen once published (grafana queries bind to it);
labels are additive only. Cardinality discipline: every label value is mapped
through a fixed vocabulary first — provider strings, document ids, and error
messages must never reach a label. All record functions swallow their own
failures: metrics must not break the ingest path (same containment contract
as ``metrics.record_retrieval``).
"""

from __future__ import annotations

import contextlib

from prometheus_client import Counter, Histogram

# Fixed vocabularies. INGEST_ACTIONS mirrors persistence.datasets.
# INGEST_ACTION_VOCABULARY; duplicated deliberately so core/ does not import
# the persistence layer, and unknown verbs fold into "other" instead of
# blowing up label cardinality.
INGEST_ACTIONS = frozenset({"ingest", "reprocess", "reembed", "recover", "retry"})
INGEST_MODES = frozenset({"text_only", "scanned", "multimodal", "auto"})
INGEST_OUTCOMES = frozenset({"completed", "error"})
CLAIM_OUTCOMES = frozenset({"claimed", "contended", "deferred"})
EMBEDDING_OUTCOMES = frozenset({"ok", "error", "timeout"})

# Ingest generations are long-running; the retrieval buckets would peg every
# observation in the last bucket. Coverage: 1s .. 1h.
ingest_duration_seconds = Histogram(
    "kb_ingest_duration_seconds",
    "Wall-clock duration of one durable ingest generation, by processing mode.",
    ("mode", "outcome"),
    buckets=(1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0),
)

ingest_actions_total = Counter(
    "kb_ingest_actions_total",
    "Ingest generations processed, by dual-verb action and outcome.",
    ("action", "outcome"),
)

worker_claims_total = Counter(
    "kb_worker_claims_total",
    "Worker claims against the durable queue: claimed won, contended lost the "
    "DB claim race, deferred lost the dataset lifecycle lease.",
    ("outcome",),
)

stuck_documents_recovered_total = Counter(
    "kb_stuck_documents_recovered_total",
    "Stuck document generations replayed by the worker recovery loop.",
)

stuck_documents_requeued_total = Counter(
    "kb_stuck_documents_requeued_total",
    "Recovered generations re-published to the durable queue.",
)

embedding_calls_total = Counter(
    "kb_embedding_calls_total",
    "Embedding batch calls by outcome; error/timeout rates drive the provider-SLA alert.",
    ("outcome",),
)

embedding_duration_seconds = Histogram(
    "kb_embedding_duration_seconds",
    "Wall-clock duration of one embedding batch call, by outcome.",
    ("outcome",),
    buckets=(0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0),
)


def _bounded(value: object, vocabulary: frozenset[str], fallback: str) -> str:
    text = str(value or "").strip().lower()
    return text if text in vocabulary else fallback


def _nonnegative_float(value: object) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    # NaN is x != x; negative durations signal a broken clock on the caller.
    if number != number or number == float("inf") or number < 0:
        return None
    return number


def _nonnegative_int(value: object) -> int | None:
    try:
        number = int(value)
    except (TypeError, ValueError):
        return None
    return number if number >= 0 else None


def record_ingest_task(
    *,
    action: object,
    mode: object,
    outcome: object,
    duration_seconds: object,
) -> None:
    """Record one processed ingest generation: verb count + per-mode duration."""
    try:
        bounded_action = _bounded(action, INGEST_ACTIONS, "other")
        bounded_mode = _bounded(mode, INGEST_MODES, "unknown")
        bounded_outcome = _bounded(outcome, INGEST_OUTCOMES, "error")
        ingest_actions_total.labels(action=bounded_action, outcome=bounded_outcome).inc()
        duration = _nonnegative_float(duration_seconds)
        if duration is not None:
            ingest_duration_seconds.labels(mode=bounded_mode, outcome=bounded_outcome).observe(
                duration
            )
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass


def record_worker_claim(outcome: object) -> None:
    """Record one durable-queue claim attempt (claimed/contended/deferred)."""
    # Metric recording must never propagate into the worker loop.
    with contextlib.suppress(Exception):
        worker_claims_total.labels(outcome=_bounded(outcome, CLAIM_OUTCOMES, "deferred")).inc()


def record_stuck_recovery(*, recovered: object = 0, requeued: object = 0) -> None:
    """Add one recovery pass's counts (from ``recover_stuck_documents``)."""
    try:
        recovered_count = _nonnegative_int(recovered)
        if recovered_count:
            stuck_documents_recovered_total.inc(recovered_count)
        requeued_count = _nonnegative_int(requeued)
        if requeued_count:
            stuck_documents_requeued_total.inc(requeued_count)
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass


def record_embedding_call(outcome: object, duration_seconds: object = None) -> None:
    """Record one embedding batch call outcome (ok/error/timeout)."""
    try:
        bounded_outcome = _bounded(outcome, EMBEDDING_OUTCOMES, "error")
        embedding_calls_total.labels(outcome=bounded_outcome).inc()
        duration = _nonnegative_float(duration_seconds)
        if duration is not None:
            embedding_duration_seconds.labels(outcome=bounded_outcome).observe(duration)
    except Exception:  # noqa: BLE001 - metric recording must never propagate
        pass
