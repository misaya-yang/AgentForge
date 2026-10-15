"""Knowledge-base RAGAS orchestration for gateway eval APIs."""

from __future__ import annotations

from typing import Any

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from .kb_ragas_client import KbRagasClient, build_kb_ragas_complete

_DEFAULT_BATCH_LIMIT = 50


async def get_kb_ragas_knowledge_summary(
    repository: AgentTraceRepository,
    *,
    tenant_id: str,
    days: int = 7,
    dataset_id: str | None = None,
) -> dict[str, Any]:
    return await repository.get_kb_ragas_summary(
        tenant_id=tenant_id,
        days=days,
        dataset_id=dataset_id,
    )


async def batch_score_kb_ragas_traces(
    repository: AgentTraceRepository,
    *,
    tenant_id: str,
    dataset_id: str,
    evaluator_id: str,
    created_by: str,
    limit: int = _DEFAULT_BATCH_LIMIT,
    only_unscored: bool = True,
) -> dict[str, Any]:
    evaluator = await repository.get_evaluator(tenant_id=tenant_id, evaluator_id=evaluator_id)
    if not evaluator:
        raise ValueError("Evaluator not found")
    if str(evaluator.get("evaluator_type") or "") != "ragas":
        raise ValueError("Evaluator must be of type ragas")

    page_size = max(1, min(limit, 200))
    offset = 0
    jobs: list[dict[str, Any]] = []
    queued = 0
    skipped = 0

    while queued < limit:
        traces, total = await repository.list_traces(
            tenant_id=tenant_id,
            trace_family="rag",
            metadata_dataset_id=dataset_id,
            limit=page_size,
            offset=offset,
        )
        if not traces:
            break
        for trace in traces:
            if queued >= limit:
                break
            trace_id = str(trace.get("trace_id") or "")
            if not trace_id:
                skipped += 1
                continue
            if only_unscored and await repository.trace_has_kb_ragas_score(
                tenant_id=tenant_id,
                trace_id=trace_id,
                evaluator_id=evaluator_id,
            ):
                skipped += 1
                continue
            if await repository.has_active_evaluator_run_for_trace(
                tenant_id=tenant_id,
                evaluator_id=evaluator_id,
                trace_id=trace_id,
            ):
                skipped += 1
                continue
            job = await repository.enqueue_evaluator_run(
                tenant_id=tenant_id,
                evaluator_id=evaluator_id,
                created_by=created_by,
                payload={
                    "trace_id": trace_id,
                    "target_snapshot": {
                        "trace_id": trace_id,
                        "trace_family": "rag",
                        "source": "kb_ragas_batch",
                        "dataset_id": dataset_id,
                    },
                    "metadata": {"kb_ragas_batch": True, "dataset_id": dataset_id},
                },
            )
            jobs.append(job)
            queued += 1
        offset += page_size
        if offset >= total:
            break

    return {"queued": queued, "skipped": skipped, "jobs": jobs}


async def score_retrieval_with_kb_ragas(
    *,
    query: str,
    contexts: list[str],
    answer: str | None = None,
    metrics: list[str] | None = None,
    ground_truth: str | None = None,
    llm_config: dict[str, Any] | None = None,
    client: KbRagasClient | None = None,
) -> dict[str, Any]:
    kb_client = client or build_kb_ragas_complete()
    results = await kb_client.evaluate_retrieval(
        query=query,
        contexts=contexts,
        answer=answer,
        metrics=metrics,
        ground_truth=ground_truth,
        llm_config=llm_config,
    )
    judge_model = next((item.judge_model for item in results if item.judge_model), None)
    return {
        "judge_model": judge_model or "",
        "results": [
            {
                "metric": item.metric,
                "score": item.score,
                "explanation": item.explanation,
                "label": item.label,
            }
            for item in results
        ],
    }
