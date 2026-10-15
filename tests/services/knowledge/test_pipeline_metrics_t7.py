"""PRD T7-1: durable ingestion pipeline metrics (claims, verbs, per-mode
duration, stuck recovery, embedding calls) and their export via /metrics.

The Prometheus registry is process-global, so every assertion measures a
delta instead of assuming a clean registry (same discipline as
test_metrics_v1.py).
"""

from __future__ import annotations

import pytest
from knowledge_service.core.observability import pipeline_metrics
from prometheus_client import REGISTRY


@pytest.fixture(autouse=True)
def _use_explicit_test_idempotency(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ENVIRONMENT", "test")
    monkeypatch.setenv("INTERNAL_IDEMPOTENCY_BACKEND", "memory")


def _sample(name: str, **labels: str) -> float:
    return float(REGISTRY.get_sample_value(name, labels) or 0.0)


# ---------------------------------------------------------------------------
# record_ingest_task: verb counter + per-mode duration
# ---------------------------------------------------------------------------


def test_record_ingest_task_counts_verb_and_observes_duration() -> None:
    actions_before = _sample("kb_ingest_actions_total", action="reembed", outcome="completed")
    count_before = _sample("kb_ingest_duration_seconds_count", mode="scanned", outcome="completed")
    sum_before = _sample("kb_ingest_duration_seconds_sum", mode="scanned", outcome="completed")

    pipeline_metrics.record_ingest_task(
        action="reembed",
        mode="scanned",
        outcome="completed",
        duration_seconds=12.5,
    )

    assert (
        _sample("kb_ingest_actions_total", action="reembed", outcome="completed") - actions_before
        == 1.0
    )
    assert (
        _sample("kb_ingest_duration_seconds_count", mode="scanned", outcome="completed")
        - count_before
        == 1.0
    )
    observed = (
        _sample("kb_ingest_duration_seconds_sum", mode="scanned", outcome="completed") - sum_before
    )
    assert observed == pytest.approx(12.5)


@pytest.mark.parametrize(
    ("action", "expected"),
    [
        ("REEMBED", "reembed"),
        (" retry ", "retry"),
        ("ingest", "ingest"),
        ("bulk-backfill", "other"),  # unknown verbs never reach the label space
        (None, "other"),
    ],
)
def test_record_ingest_task_bounds_action_label(action: object, expected: str) -> None:
    before = _sample("kb_ingest_actions_total", action=expected, outcome="error")
    pipeline_metrics.record_ingest_task(
        action=action, mode="auto", outcome="error", duration_seconds=1.0
    )
    assert _sample("kb_ingest_actions_total", action=expected, outcome="error") - before == 1.0


def test_record_ingest_task_bounds_mode_and_outcome() -> None:
    before = _sample("kb_ingest_duration_seconds_count", mode="unknown", outcome="error")
    pipeline_metrics.record_ingest_task(
        action="retry", mode="text", outcome="partial", duration_seconds=2.0
    )
    # "text" is not in the processing-mode vocabulary -> unknown;
    # "partial" is not an outcome -> error (fail closed visibly).
    assert (
        _sample("kb_ingest_duration_seconds_count", mode="unknown", outcome="error") - before == 1.0
    )


def test_record_ingest_task_skips_bad_duration_without_losing_count() -> None:
    actions_before = _sample("kb_ingest_actions_total", action="recover", outcome="completed")
    duration_before = _sample("kb_ingest_duration_seconds_count", mode="auto", outcome="completed")
    pipeline_metrics.record_ingest_task(
        action="recover", mode="auto", outcome="completed", duration_seconds=-1
    )
    pipeline_metrics.record_ingest_task(
        action="recover", mode="auto", outcome="completed", duration_seconds=None
    )
    pipeline_metrics.record_ingest_task(
        action="recover", mode="auto", outcome="completed", duration_seconds=float("nan")
    )
    assert (
        _sample("kb_ingest_actions_total", action="recover", outcome="completed") - actions_before
        == 3.0
    )
    assert (
        _sample("kb_ingest_duration_seconds_count", mode="auto", outcome="completed")
        == duration_before
    )


def test_record_ingest_task_never_raises_on_garbage() -> None:
    pipeline_metrics.record_ingest_task(  # type: ignore[arg-type]
        action=object(), mode=None, outcome=object(), duration_seconds=object()
    )


# ---------------------------------------------------------------------------
# record_worker_claim: claim / contention
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("outcome", ["claimed", "contended", "deferred"])
def test_record_worker_claim_counts_each_outcome(outcome: str) -> None:
    before = _sample("kb_worker_claims_total", outcome=outcome)
    pipeline_metrics.record_worker_claim(outcome)
    assert _sample("kb_worker_claims_total", outcome=outcome) - before == 1.0


def test_record_worker_claim_bounds_unknown_outcomes_to_deferred() -> None:
    before = _sample("kb_worker_claims_total", outcome="deferred")
    pipeline_metrics.record_worker_claim("connection reset by peer")
    pipeline_metrics.record_worker_claim(None)
    assert _sample("kb_worker_claims_total", outcome="deferred") - before == 2.0


# ---------------------------------------------------------------------------
# record_stuck_recovery
# ---------------------------------------------------------------------------


def test_record_stuck_recovery_adds_recovered_and_requeued() -> None:
    recovered_before = _sample("kb_stuck_documents_recovered_total")
    requeued_before = _sample("kb_stuck_documents_requeued_total")
    pipeline_metrics.record_stuck_recovery(recovered=3, requeued=2)
    assert _sample("kb_stuck_documents_recovered_total") - recovered_before == 3.0
    assert _sample("kb_stuck_documents_requeued_total") - requeued_before == 2.0


@pytest.mark.parametrize("bad", ["junk", -5, None, float("nan")])
def test_record_stuck_recovery_ignores_invalid_counts(bad: object) -> None:
    recovered_before = _sample("kb_stuck_documents_recovered_total")
    requeued_before = _sample("kb_stuck_documents_requeued_total")
    pipeline_metrics.record_stuck_recovery(recovered=bad, requeued=bad)
    assert _sample("kb_stuck_documents_recovered_total") == recovered_before
    assert _sample("kb_stuck_documents_requeued_total") == requeued_before


# ---------------------------------------------------------------------------
# record_embedding_call
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("outcome", ["ok", "error", "timeout"])
def test_record_embedding_call_counts_outcome(outcome: str) -> None:
    before = _sample("kb_embedding_calls_total", outcome=outcome)
    pipeline_metrics.record_embedding_call(outcome, duration_seconds=0.3)
    assert _sample("kb_embedding_calls_total", outcome=outcome) - before == 1.0


def test_record_embedding_call_folds_unknown_into_error() -> None:
    before = _sample("kb_embedding_calls_total", outcome="error")
    pipeline_metrics.record_embedding_call("rate limit from provider")
    pipeline_metrics.record_embedding_call(None)
    assert _sample("kb_embedding_calls_total", outcome="error") - before == 2.0


def test_record_embedding_call_observes_duration_only_when_valid() -> None:
    count_before = _sample("kb_embedding_duration_seconds_count", outcome="ok")
    pipeline_metrics.record_embedding_call("ok")  # no duration -> count only
    assert _sample("kb_embedding_duration_seconds_count", outcome="ok") == count_before
    pipeline_metrics.record_embedding_call("ok", duration_seconds=0.05)
    assert _sample("kb_embedding_duration_seconds_count", outcome="ok") - count_before == 1.0


def test_record_embedding_call_never_raises_on_garbage() -> None:
    pipeline_metrics.record_embedding_call(object(), duration_seconds=object())  # type: ignore[arg-type]


# ---------------------------------------------------------------------------
# /metrics route exposes the T7 families (registry integration contract)
# ---------------------------------------------------------------------------


def _build_app(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main

    return main.create_app(main.Settings())


def test_metrics_route_exports_pipeline_families(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fastapi.testclient import TestClient

    # Guarantee at least one observation/child for every labeled family.
    pipeline_metrics.record_ingest_task(
        action="ingest", mode="text_only", outcome="completed", duration_seconds=3
    )
    pipeline_metrics.record_worker_claim("contended")
    pipeline_metrics.record_stuck_recovery(recovered=1)
    pipeline_metrics.record_embedding_call("ok", duration_seconds=0.1)

    app = _build_app(monkeypatch)
    response = TestClient(app).get("/metrics")

    assert response.status_code == 200
    body = response.text
    for name in (
        "kb_ingest_duration_seconds",
        "kb_ingest_actions_total",
        "kb_worker_claims_total",
        "kb_stuck_documents_recovered_total",
        "kb_embedding_calls_total",
        "kb_embedding_duration_seconds",
    ):
        assert name in body
    # The T7 additions must not shadow or clash with the metrics_v1 surface.
    for name in (
        "kb_retrieve_latency_seconds",
        "kb_retrievals_total",
        "kb_ingestion_queue_depth",
    ):
        assert name in body
