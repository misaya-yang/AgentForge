"""Standalone reproduction: LLM-judge non-canonical labels are dropped from eval aggregation.

Runs against the real EvaluatorExecutor with a fake repository that mimics
create_score() storing payload['label'] verbatim (as the SQL does).
"""
from __future__ import annotations

import asyncio
import json
from typing import Any

from ai_gateway_core.eval.evaluator_executor import EvaluatorExecutor, LlmCompleteContext


class FakeRepo:
    def __init__(self) -> None:
        self.evaluator = {
            "evaluator_id": "eval-llm-1",
            "name": "quality",
            "evaluator_type": "llm",   # LLM judge
            "version": "v1",
            "metadata": {"judge_model_id": "qwen3.7-plus"},
            "filter_config": {},
        }
        self.trace_detail = {
            "trace": {
                "trace_id": "trace-1",
                "input_preview": "What is 2+2?",
                "output_preview": "The answer is 4.",
                "status": "succeeded",
                "total_latency_ms": 120,
                "trace_family": "assistant",
                "metadata": {},
                "metrics": {},
            },
            "spans": [],
            "events": [],
        }
        self.run_updates: list[dict[str, Any]] = []

    async def update_experiment_run(self, **kwargs):
        self.run_updates.append(kwargs)

    async def get_evaluator(self, **kwargs):
        return self.evaluator

    async def get_trace_detail(self, **kwargs):
        return self.trace_detail

    async def create_eval_score(self, *, tenant_id, trace_id, created_by, payload, trace_family="assistant"):
        # Mirrors the real SQL INSERT: label is stored verbatim, no normalization.
        return {
            "score_id": "s1",
            "label": payload.get("label"),          # verbatim
            "numeric_value": payload.get("numeric_value"),
            "score_name": payload.get("score_name"),
        }


async def fake_llm_complete(model_id: str, prompt: str, context: LlmCompleteContext) -> str:
    # A very common, valid judge response — but 'label' is not one of pass/fail/review.
    return json.dumps({
        "numeric_value": 0.95,
        "label": "excellent",
        "explanation": "Correct and well grounded.",
        "confidence": 0.9,
    })


async def main() -> None:
    repo = FakeRepo()
    executor = EvaluatorExecutor(repo, llm_complete=fake_llm_complete)
    result = await executor.run_job(
        tenant_id="t1",
        job_payload={"run_id": "run-1", "evaluator_id": "eval-llm-1", "trace_id": "trace-1"},
    )
    print("run status      :", result.status)
    print("scores_written  :", result.scores_written)
    print("score_summary   :", json.dumps(result.score_summary, ensure_ascii=False))
    s = result.score_summary
    print()
    print("Judge numeric_value returned : 0.95")
    print("average_score reported       :", s.get("average_score"))
    print("scored_count reported        :", s.get("scored_count"))
    if s.get("scored_count") == 0 and s.get("average_score") == 0.0:
        print(">>> BUG CONFIRMED: a valid 0.95 judgment was dropped; run reports avg=0.0, scored=0.")
    else:
        print(">>> no bug")


if __name__ == "__main__":
    asyncio.run(main())
