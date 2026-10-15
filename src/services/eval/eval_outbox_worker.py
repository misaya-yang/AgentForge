from __future__ import annotations

import asyncio
from dataclasses import replace
from typing import Any

from ai_gateway_core.billing.pricing_catalog import resolve_pricing_with_status
from ai_gateway_core.eval import EvalOutboxWorker, EvaluatorExecutor
from ai_gateway_core.logging import get_logger
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from .assistant_trace_capture import runtime_model_evidence
from .eval_candidate_client import (
    EVAL_CANDIDATE_USER_ID,
    EvalCandidateClient,
    candidate_fingerprint_from_context,
)
from .eval_llm_client import build_eval_llm_complete, load_eval_llm_settings
from .golden import evaluate_case
from .kb_ragas_client import KbRagasClient, build_kb_ragas_complete

logger = get_logger(__name__)

_eval_outbox_worker: EvalOutboxWorker | None = None
_kb_ragas_client: KbRagasClient | None = None
_eval_candidate_client: EvalCandidateClient | None = None


def _persisted_candidate_fingerprint(detail: dict[str, Any]) -> dict[str, Any]:
    metadata = (detail.get("trace") or {}).get("metadata") or {}
    if isinstance(metadata.get("candidate_fingerprint"), dict):
        return dict(metadata["candidate_fingerprint"])
    for span in detail.get("spans") or []:
        if not isinstance(span, dict) or span.get("span_kind") != "context_building":
            continue
        attributes = span.get("attributes")
        if isinstance(attributes, dict):
            return candidate_fingerprint_from_context(attributes)
    return {}


async def _kb_ragas_evaluate(**kwargs: Any) -> list[dict[str, Any]]:
    global _kb_ragas_client
    if _kb_ragas_client is None:
        _kb_ragas_client = build_kb_ragas_complete()
    results = await _kb_ragas_client.evaluate_retrieval(**kwargs)
    return [
        {
            "metric": item.metric,
            "score": item.score,
            "explanation": item.explanation,
            "label": item.label,
            "judge_model": item.judge_model,
            "failure_kind": item.failure_kind,
        }
        for item in results
    ]


def _candidate_cost_cents(
    model_id: str, usage: dict[str, Any], pricing_snapshot: dict[str, Any] | None = None,
) -> float | None:
    if not any(
        key in usage
        for key in ("input_tokens", "output_tokens", "prompt_tokens", "completion_tokens")
    ):
        return None
    input_tokens = int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
    output_tokens = int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)
    if pricing_snapshot is not None:
        pricing = {"input": pricing_snapshot["input_price_per_1k"],
                   "output": pricing_snapshot["output_price_per_1k"]}
        pricing_status = str(pricing_snapshot.get("pricing_status") or "tenant_model")
    else:
        pricing, pricing_status = resolve_pricing_with_status(model_id)
    if pricing_status == "unknown":
        return None
    return round(
        (
            (input_tokens / 1000) * float(pricing.get("input") or 0)
            + (output_tokens / 1000) * float(pricing.get("output") or 0)
        )
        * 100,
        6,
    )


def _build_candidate_runner(repository: AgentTraceRepository):
    async def _run_candidate(
        *,
        tenant_id: str,
        run_case: dict[str, Any],
        execution_config: dict[str, Any],
    ) -> dict[str, Any]:
        global _eval_candidate_client
        run_case_id = str(run_case.get("run_case_id") or "")
        trace_id = str(run_case.get("candidate_trace_id") or "")
        detail = None
        if trace_id:
            detail = await repository.get_trace_detail(
                tenant_id=tenant_id,
                trace_id=trace_id,
                trace_family="assistant",
            )
        if detail is None:
            existing, _ = await repository.list_traces(
                tenant_id=tenant_id,
                user_id=(run_case.get("runtime_handle") or {}).get("user_id") or EVAL_CANDIDATE_USER_ID,
                trace_family="assistant",
                session_id=run_case_id,
                limit=1,
                offset=0,
            )
            if existing:
                trace_id = str(existing[0].get("trace_id") or "")
                detail = await repository.get_trace_detail(
                    tenant_id=tenant_id,
                    trace_id=trace_id,
                    trace_family="assistant",
                )

        result = None
        if detail is not None and (detail.get("trace") or {}).get("status") not in {
            "succeeded", "failed", "cancelled", "timeout",
        }:
            detail = None
        if detail is None:
            if _eval_candidate_client is None:
                # Only this fenced job path may delegate its persisted creator.
                # Current user grants and Runtime tool approvals still apply.
                _eval_candidate_client = EvalCandidateClient(allow_service_identity=True)
            input_payload = run_case.get("input") if isinstance(run_case.get("input"), dict) else {}
            message = str(input_payload.get("message") or "").strip()

            handle = run_case.get("runtime_handle")
            handle = dict(handle) if isinstance(handle, dict) else {}
            dispatch_state = str(run_case.get("dispatch_state") or "not_started")
            if dispatch_state != "not_started" and not handle.get("turn_id"):
                reconcile = getattr(repository, "reconcile_candidate_handle", None)
                recovered = await reconcile(
                    tenant_id=tenant_id, run_case_id=run_case_id, handle=handle,
                ) if callable(reconcile) and handle.get("thread_id") else None
                if not recovered:
                    raise RuntimeError("AGENT_EVAL_DISPATCH_RECONCILIATION_REQUIRED")
                handle = recovered

            async def save_dispatch(value: dict[str, Any]) -> None:
                await repository.update_experiment_run_case(
                    tenant_id=tenant_id, run_case_id=run_case_id, status="running",
                    runtime_handle=value, dispatch_state="dispatching",
                )

            async def save_handle(value: dict[str, Any]) -> None:
                await repository.update_experiment_run_case(
                    tenant_id=tenant_id, run_case_id=run_case_id, status="running",
                    runtime_handle=value, dispatch_state="accepted",
                )

            delegation = None
            if execution_config.get("candidate_type") == "agent_version":
                # Studio ACLs are evaluated for the persisted run creator,
                # never for the worker's privileged gateway credential.
                delegation = await repository.resolve_eval_job_actor(tenant_id=tenant_id)
            elif (getattr(_eval_candidate_client, "allow_service_identity", False)
                    and not _eval_candidate_client.token and not _eval_candidate_client.api_key):
                delegation = await repository.resolve_eval_job_actor(tenant_id=tenant_id)
            result = await _eval_candidate_client.run(
                tenant_id=tenant_id, run_case_id=run_case_id, message=message,
                config=execution_config, resume_handle=handle or None, delegation=delegation,
                on_dispatch_started=save_dispatch, on_turn_created=save_handle,
                on_cursor=save_handle,
            )
            trace_id = result.trace_id
            expected_ref = execution_config.get("model_ref")
            if isinstance(expected_ref, dict) and expected_ref:
                evidence = await repository.get_candidate_runtime_evidence(
                    tenant_id=tenant_id, run_case_id=run_case_id, run_id=trace_id,
                )
                runtime_fingerprint, usage, model_spans = runtime_model_evidence(
                    evidence=evidence, tenant_id=tenant_id, run_case_id=run_case_id,
                    run_id=trace_id, expected_model_ref=expected_ref,
                )
                fingerprint = {**result.fingerprint, **runtime_fingerprint}
                trace_payload = dict(result.trace_payload or {})
                if not trace_payload:
                    raise RuntimeError("AGENT_EVAL_TRACE_CAPTURE_UNAVAILABLE")
                trace_payload["provider"] = fingerprint["provider"]
                trace_payload["model_id"] = fingerprint["model_id"]
                trace_payload["metrics"] = {**trace_payload.get("metrics", {}),
                    **{key: usage.get(key) for key in ("input_tokens", "output_tokens", "total_tokens")}}
                trace_payload["metadata"] = {**trace_payload.get("metadata", {}),
                                              "candidate_fingerprint": fingerprint}
                trace_payload["spans"] = [*trace_payload.get("spans", []), *model_spans]
                result = replace(result, fingerprint=fingerprint, usage=usage, trace_payload=trace_payload)
            if result.trace_payload is not None:
                subject = result.trace_payload.get("user_id")
                if not isinstance(subject, str) or not subject.strip():
                    raise RuntimeError("AGENT_EVAL_TRACE_IDENTITY_UNAVAILABLE")
                await repository.ingest_trace(
                    tenant_id=tenant_id,
                    created_by=subject,
                    payload={"trace": result.trace_payload, "enqueue": False},
                    enqueue=False,
                )
            for delay in (0.05, 0.1, 0.2, 0.4, 0.8, 1.6, 3.2, 3.2):
                detail = await repository.get_trace_detail(
                    tenant_id=tenant_id,
                    trace_id=trace_id,
                    trace_family="assistant",
                )
                if detail and (detail.get("trace") or {}).get("status") in {
                    "succeeded",
                    "failed",
                    "cancelled",
                    "timeout",
                }:
                    break
                await asyncio.sleep(delay)
        if not detail:
            raise RuntimeError("Candidate trace persistence timed out")
        if result is not None and result.error:
            raise RuntimeError(result.error)

        trace = detail.get("trace") or {}
        fingerprint = (dict(result.fingerprint) if result is not None
                       else _persisted_candidate_fingerprint(detail))
        expected_ref = execution_config.get("model_ref")
        recovered_usage = None
        if result is None and isinstance(expected_ref, dict) and expected_ref:
            # A cold-start reconciler can have persisted the terminal Trace first.
            # Recover observations from that original run, never dispatch again.
            evidence = await repository.get_candidate_runtime_evidence(
                tenant_id=tenant_id, run_case_id=run_case_id, run_id=trace_id,
            )
            recovered_fingerprint, recovered_usage, model_spans = runtime_model_evidence(
                evidence=evidence, tenant_id=tenant_id, run_case_id=run_case_id,
                run_id=trace_id, expected_model_ref=expected_ref,
            )
            fingerprint.update(recovered_fingerprint)
            if execution_config.get("candidate_type") == "agent_version" and any(
                fingerprint.get(key) != execution_config.get(key)
                for key in ("agent_id", "agent_version_id", "agent_spec_hash", "agent_runtime_snapshot_hash")
            ):
                raise RuntimeError("AGENT_EVAL_VERSION_PIN_MISMATCH")
            existing_spans = detail.get("spans") or []
            span_ids = {str(span.get("span_id")) for span in existing_spans}
            detail = {**detail, "spans": [*existing_spans, *(
                span for span in model_spans if str(span["span_id"]) not in span_ids
            )]}
        if isinstance(expected_ref, dict) and expected_ref:
            actual_ref = fingerprint.get("model_ref") or {}
            if fingerprint.get("model_ref_verified") is not True or any(
                actual_ref.get(key) != expected_ref.get(key)
                for key in ("tenant_id", "provider_id", "model_id", "capability_revision", "price_version")
            ):
                raise RuntimeError("AGENT_EVAL_MODEL_REF_MISMATCH")
        metadata = trace.get("metadata") if isinstance(trace.get("metadata"), dict) else {}
        runtime = (
            metadata.get("runtime_trajectory")
            if isinstance(metadata.get("runtime_trajectory"), dict)
            else {}
        )
        usage = (
            dict(result.usage)
            if result is not None
            else {
                key: trace.get(key)
                for key in ("input_tokens", "output_tokens", "total_tokens")
                if isinstance(trace.get(key), int | float)
            }
        )
        if recovered_usage is not None:
            usage = recovered_usage
        output = (
            result.output
            if result is not None and result.output
            else trace.get("output_preview") or ""
        )
        spans = detail.get("spans") if isinstance(detail.get("spans"), list) else []
        observation = {
            "status": trace.get("status"),
            "output_preview": output,
            "span_kinds": [
                str(span.get("span_kind"))
                for span in spans
                if isinstance(span, dict) and span.get("span_kind")
            ],
            "spans": spans,
            "total_latency_ms": trace.get("total_latency_ms"),
            "total_tokens": (
                usage.get("total_tokens")
                if "total_tokens" in usage
                else (
                    int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
                    + int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)
                    if usage
                    else None
                )
            ),
            "total_cost_cents": _candidate_cost_cents(
                str(trace.get("model_id") or ""), usage,
                pricing_snapshot=(execution_config.get("model_ref") or {}).get("pricing_snapshot"),
            ),
            **runtime,
        }
        contract_case = {
            "case_id": run_case.get("case_id"),
            "input": run_case.get("input") or {},
            "expected_output": run_case.get("expected_output") or {},
            "expected_trajectory": run_case.get("expected_trajectory") or {},
            "assertions": run_case.get("assertions") or [],
            "metadata": run_case.get("metadata") or {},
        }
        return {
            "trace_id": trace_id,
            "detail": detail,
            "usage": usage,
            "fingerprint": fingerprint,
            "contract_result": evaluate_case(contract_case, observation),
        }

    return _run_candidate


def init_eval_outbox_worker(
    database: Any,
    *,
    enabled: bool = True,
    concurrency: int = 2,
    poll_interval_s: float = 2.0,
    admission_controller: Any | None = None,
    capacity_resolver: Any | None = None,
) -> EvalOutboxWorker | None:
    global _eval_outbox_worker
    if not enabled or not getattr(database, "enabled", False):
        _eval_outbox_worker = None
        return None
    repository = AgentTraceRepository(database)
    llm_settings = load_eval_llm_settings()
    llm_complete = build_eval_llm_complete(llm_settings)
    executor = EvaluatorExecutor(
        repository,
        llm_complete=llm_complete,
        kb_ragas_evaluate=_kb_ragas_evaluate,
        candidate_run=_build_candidate_runner(repository),
        created_by="eval-worker",
    )
    if llm_complete is not None:
        logger.info(
            "Eval LLM judge enabled model=%s gateway=%s",
            llm_settings.default_judge_model_id,
            llm_settings.gateway_base_url,
        )
    else:
        logger.warning("Eval LLM judge disabled; llm evaluators will require manual review")
    async def admit_job(job: dict[str, Any]):
        tenant_id = str(job["tenant_id"])
        budgets = await capacity_resolver.resolve(
            tenant_id=tenant_id, service_id="eval-worker", request_class="job",
            upstream_group=None, provider_id=None, resource_kind="job",
        )
        return await admission_controller.acquire(
            budgets=budgets, tenant_id=tenant_id, user_id="eval-worker", service_id="eval-worker",
            request_class="job", request_id=str(job["job_id"]),
        )

    _eval_outbox_worker = EvalOutboxWorker(
        repository,
        executor,
        poll_interval_s=poll_interval_s,
        admission_hook=admit_job if admission_controller is not None and capacity_resolver is not None else None,
    )
    return _eval_outbox_worker


def get_eval_outbox_worker() -> EvalOutboxWorker | None:
    return _eval_outbox_worker
