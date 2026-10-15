from __future__ import annotations

from typing import Any

import pytest
from ai_gateway_core.eval.online_sampling import (
    evaluator_matches_online_trace,
    resolve_online_queue_cap,
    resolve_online_sample_rate,
    schedule_online_eval_for_trace,
    should_sample_trace_id,
)


def test_should_sample_trace_id_is_deterministic() -> None:
    trace_id = "11111111-1111-4111-8111-111111111111"
    first = should_sample_trace_id(trace_id, 0.5)
    second = should_sample_trace_id(trace_id, 0.5)
    assert first == second


def test_should_sample_trace_id_handles_non_uuid_trace_ids() -> None:
    trace_id = "trace-from-external-adapter"
    assert should_sample_trace_id(trace_id, 1.0)
    assert not should_sample_trace_id(trace_id, 0.0)
    assert not should_sample_trace_id(trace_id, "not-a-number")


def test_resolve_online_sample_rate_falls_back_on_invalid_rate() -> None:
    evaluator = {
        "evaluator_id": "eval-bad-rate",
        "sampling_config": {"online": {"enabled": True, "rate": "not-a-number"}},
    }

    assert resolve_online_sample_rate(evaluator, default_rate=0.25) == 0.25


def test_evaluator_matches_online_trace_respects_family_and_failure_only() -> None:
    evaluator = {
        "evaluator_type": "rule",
        "sampling_config": {
            "online": {
                "enabled": True,
                "rate": 1.0,
                "trace_families": ["assistant"],
                "only_failed": True,
            }
        },
    }
    assert evaluator_matches_online_trace(
        evaluator,
        trace_family="assistant",
        status="failed",
    )
    assert not evaluator_matches_online_trace(
        evaluator,
        trace_family="assistant",
        status="succeeded",
    )
    assert not evaluator_matches_online_trace(
        evaluator,
        trace_family="rag",
        status="failed",
    )


class _OnlineRepo:
    def __init__(
        self,
        evaluators: list[dict[str, Any]],
        *,
        active_runs: set[tuple[str, str, str]] | None = None,
        pending_online_runs: int = 0,
    ) -> None:
        self.evaluators = evaluators
        self.enqueued: list[dict[str, Any]] = []
        self.active_runs = active_runs or set()
        self.pending_online_runs = pending_online_runs

    async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any]:
        return {"trace": {"metadata": {}, "user_id": "production-user"}}

    async def list_evaluators(
        self, *, limit: int, offset: int, **_kwargs: Any
    ) -> tuple[list[dict[str, Any]], int]:
        return self.evaluators[offset : offset + limit], len(self.evaluators)

    async def count_pending_online_eval_runs(self, *, tenant_id: str) -> int:  # noqa: ARG002
        return self.pending_online_runs

    async def has_active_evaluator_run_for_trace(
        self,
        *,
        tenant_id: str,
        evaluator_id: str,
        trace_id: str,
    ) -> bool:
        return (tenant_id, evaluator_id, trace_id) in self.active_runs

    async def enqueue_evaluator_run(self, **kwargs: Any) -> dict[str, Any]:
        self.enqueued.append(kwargs)
        return {"job_id": "job-1", "status": "queued", "run_id": "run-1"}


@pytest.mark.asyncio
async def test_schedule_online_eval_enqueues_matching_evaluator(monkeypatch: pytest.MonkeyPatch) -> None:
    trace_id = "22222222-2222-4222-8222-222222222222"
    monkeypatch.setattr(
        "ai_gateway_core.eval.online_sampling.should_sample_trace_id",
        lambda _trace_id, _rate: True,
    )
    repo = _OnlineRepo(
        [
            {
                "evaluator_id": "eval-online",
                "evaluator_type": "rule",
                "sampling_config": {
                    "online": {
                        "enabled": True,
                        "rate": 1.0,
                        "trace_families": ["assistant"],
                    }
                },
            }
        ]
    )

    result = await schedule_online_eval_for_trace(
        repo,
        tenant_id="tenant-a",
        payload={
            "trace_id": trace_id,
            "trace_family": "assistant",
            "status": "succeeded",
            "source_adapter": "agent-runtime-v2",
        },
    )

    assert result["scheduled"] == 1
    assert repo.enqueued[0]["payload"]["trace_id"] == trace_id
    assert repo.enqueued[0]["payload"]["target_snapshot"]["source"] == "online_sampling"
    assert repo.enqueued[0]["payload"]["target_snapshot"]["trace_id"] == trace_id


@pytest.mark.asyncio
async def test_schedule_online_eval_skips_when_active_run_exists(monkeypatch: pytest.MonkeyPatch) -> None:
    trace_id = "33333333-3333-4333-8333-333333333333"
    monkeypatch.setattr(
        "ai_gateway_core.eval.online_sampling.should_sample_trace_id",
        lambda _trace_id, _rate: True,
    )
    repo = _OnlineRepo(
        [
            {
                "evaluator_id": "eval-online",
                "evaluator_type": "rule",
                "sampling_config": {
                    "online": {
                        "enabled": True,
                        "rate": 1.0,
                        "trace_families": ["assistant"],
                    }
                },
            }
        ],
        active_runs={("tenant-a", "eval-online", trace_id)},
    )

    result = await schedule_online_eval_for_trace(
        repo,
        tenant_id="tenant-a",
        payload={
            "trace_id": trace_id,
            "trace_family": "assistant",
            "status": "succeeded",
        },
    )

    assert result["scheduled"] == 0
    assert repo.enqueued == []


def test_resolve_online_queue_cap_falls_back_on_invalid_value() -> None:
    evaluator = {
        "evaluator_id": "eval-cap",
        "sampling_config": {"online": {"enabled": True, "max_pending_runs": "bad"}},
    }
    assert resolve_online_queue_cap(evaluator, default_cap=150) == 150


@pytest.mark.asyncio
async def test_schedule_online_eval_skips_when_queue_is_full() -> None:
    trace_id = "44444444-4444-4444-8444-444444444444"
    repo = _OnlineRepo(
        [
            {
                "evaluator_id": "eval-online",
                "evaluator_type": "rule",
                "sampling_config": {
                    "online": {
                        "enabled": True,
                        "rate": 1.0,
                        "trace_families": ["assistant"],
                    }
                },
            }
        ],
        pending_online_runs=200,
    )

    result = await schedule_online_eval_for_trace(
        repo,
        tenant_id="tenant-a",
        payload={"trace_id": trace_id, "trace_family": "assistant", "status": "succeeded"},
        max_pending_online_runs=200,
    )

    assert result["scheduled"] == 0
    assert result["reason"] == "online_eval_queue_full"
    assert repo.enqueued == []


@pytest.mark.asyncio
async def test_schedule_online_eval_skips_internal_eval_trace() -> None:
    repo = _OnlineRepo([])

    async def get_internal_trace(**_kwargs: Any) -> dict[str, Any]:
        return {"trace": {"metadata": {"eval_origin": "candidate"}}}

    repo.get_trace_detail = get_internal_trace  # type: ignore[method-assign]
    result = await schedule_online_eval_for_trace(
        repo,
        tenant_id="tenant-a",
        payload={
            "trace_id": "55555555-5555-4555-8555-555555555555",
            "trace_family": "assistant",
            "status": "succeeded",
        },
    )

    assert result["scheduled"] == 0
    assert result["reason"] == "internal_eval_trace"


@pytest.mark.asyncio
async def test_schedule_online_eval_pages_through_all_evaluators(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "ai_gateway_core.eval.online_sampling.should_sample_trace_id",
        lambda _trace_id, _rate: True,
    )
    evaluators = [
        {
            "evaluator_id": f"eval-{index}",
            "evaluator_type": "rule",
            "sampling_config": {
                "online": {
                    "enabled": True,
                    "rate": 1.0,
                    "trace_families": ["assistant"],
                }
            },
        }
        for index in range(101)
    ]
    repo = _OnlineRepo(evaluators)

    result = await schedule_online_eval_for_trace(
        repo,
        tenant_id="tenant-a",
        payload={
            "trace_id": "66666666-6666-4666-8666-666666666666",
            "trace_family": "assistant",
            "status": "succeeded",
        },
        evaluator_limit=100,
        max_pending_online_runs=200,
    )

    assert result["matched"] == 101
    assert result["scheduled"] == 101
