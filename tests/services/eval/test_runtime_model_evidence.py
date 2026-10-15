from copy import deepcopy
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.api.schemas.eval import AgentTraceScore
from src.services.eval import eval_outbox_worker
from src.services.eval.assistant_trace_capture import runtime_model_evidence
from src.services.eval.eval_candidate_client import EvalCandidateResult


def _evidence():
    started = datetime.now(timezone.utc)
    ref = {"tenant_id": "tenant-a", "provider_id": "dashscope", "model_id": "qwen3.7-plus",
           "capability_revision": 3, "price_version": "price-v1"}
    evidence = {**ref, "session_id": "case-a", "run_id": str(uuid4()),
                "pricing_snapshot": {"version": "price-v1"}, "runtime_revision": "runtime-v1",
                "parameters": {"temperature": 0}, "limits": {"max_output_tokens": 512},
                "calls": [{"call_id": str(uuid4()), "status": "completed", "input_tokens": 13,
                           "output_tokens": 7, "cost_microusd": 0, "dispatched_at": started,
                           "completed_at": started + timedelta(seconds=1)}]}
    return ref, evidence


def _project(ref, evidence):
    return runtime_model_evidence(evidence=evidence, tenant_id="tenant-a", run_case_id="case-a",
                                  run_id=str(evidence.get("run_id")), expected_model_ref=ref)


def test_actual_ledger_supplies_model_span_usage_and_frozen_identity():
    ref, evidence = _evidence()
    fingerprint, usage, spans = _project(ref, evidence)
    assert fingerprint["model_ref"] == ref
    assert fingerprint["model_ref_verified"] is True
    assert usage == {"input_tokens": 13, "output_tokens": 7, "total_tokens": 20}
    assert spans[0]["span_kind"] == "model_invocation"
    assert spans[0]["status"] == "succeeded"
    assert spans[0]["duration_ms"] == 1000
    assert spans[0]["attributes"]["call_id"] == evidence["calls"][0]["call_id"]
    assert _project(ref, evidence) == (fingerprint, usage, spans)


def test_native_execution_receipt_proves_tool_effect_without_exposing_arguments():
    ref, evidence = _evidence()
    execution = {"execution_id": str(uuid4()), "capability_id": "generate_quiz",
                 "status": "succeeded", "effect": "write", "approval_status": "approved",
                 "quiz_id": str(uuid4()), "arguments": {"private": "must-not-project"}}
    evidence["tool_executions"] = [execution]
    spans = _project(ref, evidence)[2]
    assert spans[-1]["span_kind"] == "tool_execution"
    assert spans[-1]["name"] == "generate_quiz" and spans[-1]["status"] == "succeeded"
    assert spans[-1]["attributes"]["quiz_id"] == execution["quiz_id"]
    assert "arguments" not in spans[-1]["attributes"]
    assert "must-not-project" not in repr(spans)


@pytest.mark.parametrize("field,value", [("tenant_id", "foreign"), ("session_id", "another-case"),
                                         ("provider_id", "foreign"), ("model_id", "another-model"),
                                         ("capability_revision", 4), ("pricing_snapshot", {"version": "changed"})])
def test_wrong_scope_or_changed_model_receipt_is_rejected(field, value):
    ref, evidence = _evidence()
    evidence[field] = value
    with pytest.raises(RuntimeError, match="AGENT_EVAL_MODEL_"):
        _project(ref, evidence)


def test_unobserved_calls_and_usage_are_not_invented():
    ref, evidence = _evidence()
    evidence["calls"][0]["input_tokens"] = None
    reserved = deepcopy(evidence["calls"][0])
    reserved.update(status="reserved", dispatched_at=None, input_tokens=9999)
    evidence["calls"].append(reserved)
    _, usage, spans = _project(ref, evidence)
    assert usage == {"output_tokens": 7}
    assert len(spans) == 1
    evidence["calls"] = [reserved]
    _, usage, spans = _project(ref, evidence)
    assert usage == {} and spans == []


def test_runtime_prompt_and_tools_use_persisted_fingerprints_only():
    ref, evidence = _evidence()
    assert "system_prompt_hash" not in _project(ref, evidence)[0]
    evidence["eval_fingerprint"] = {"system_prompt_hash": "a" * 64, "tool_schema_hash": "b" * 64}
    fingerprint = _project(ref, evidence)[0]
    assert fingerprint["system_prompt_hash"] == "a" * 64
    assert fingerprint["tool_schema_hash"] == "b" * 64
    evidence["eval_fingerprint"] = {"system_prompt_hash": "unverified", "tool_schema_hash": ""}
    fingerprint = _project(ref, evidence)[0]
    assert "system_prompt_hash" not in fingerprint and "tool_schema_hash" not in fingerprint


def test_trace_score_serializes_database_evaluator_uuid():
    repository = AgentTraceRepository(SimpleNamespace(enabled=False, _pool=None))
    evaluator_id = uuid4()
    decoded = repository._decode_score_row({"score_id": uuid4(), "trace_id": uuid4(),
                                            "evaluator_id": evaluator_id, "metadata": {},
                                            "score_name": "behavior", "created_by": "eval-worker"})
    assert AgentTraceScore.model_validate(decoded).evaluator_id == str(evaluator_id)
    assert repository._decode_score_row({"evaluator_id": None})["evaluator_id"] is None


@pytest.mark.asyncio
async def test_repository_scopes_snapshot_and_calls_to_same_tenant_case_and_run():
    repository = AgentTraceRepository(SimpleNamespace(enabled=False, _pool=None))
    ref, evidence = _evidence()
    repository.fetchrow = AsyncMock(return_value={**evidence, "pricing_snapshot": '{"version":"price-v1"}'})
    repository.fetch = AsyncMock(return_value=evidence["calls"])
    result = await repository.get_candidate_runtime_evidence(
        tenant_id="tenant-a", run_case_id="case-a", run_id=evidence["run_id"],
    )
    assert result["pricing_snapshot"]["version"] == ref["price_version"]
    for method in (repository.fetchrow, repository.fetch):
        query, *args = method.await_args.args
        assert args == ["tenant-a", "case-a", evidence["run_id"]]
        assert "tenant_id = $1" in query and "session_id = $2" in query and "run_id = $3::uuid" in query
    repository.fetchrow.return_value = None
    repository.fetch.reset_mock()
    assert await repository.get_candidate_runtime_evidence(
        tenant_id="foreign", run_case_id="case-a", run_id=evidence["run_id"],
    ) is None
    repository.fetch.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("mismatched_version", [False, True])
async def test_reconciled_terminal_candidate_uses_original_evidence_without_dispatch(
    monkeypatch, mismatched_version,
):
    ref, evidence = _evidence()
    pin = {"agent_id": str(uuid4()), "agent_version_id": str(uuid4()),
           "agent_spec_hash": "a" * 64, "agent_runtime_snapshot_hash": "sha256:" + "b" * 64}
    evidence.update(pin)
    if mismatched_version:
        evidence["agent_version_id"] = str(uuid4())
    detail = {"trace": {"trace_id": evidence["run_id"], "status": "succeeded",
                        "model_id": ref["model_id"], "output_preview": "EVAL-MARKER",
                        "metadata": {"runtime_trajectory": {"exit_reason": "succeeded"}}},
              "spans": []}
    repository = SimpleNamespace(
        get_trace_detail=AsyncMock(return_value=detail),
        get_candidate_runtime_evidence=AsyncMock(return_value=evidence),
    )
    candidate = SimpleNamespace(run=AsyncMock())
    monkeypatch.setattr(eval_outbox_worker, "_eval_candidate_client", candidate)
    runner = eval_outbox_worker._build_candidate_runner(repository)
    kwargs = {"tenant_id": "tenant-a", "run_case": {
        "run_case_id": "case-a", "candidate_trace_id": evidence["run_id"],
        "case_id": "recovered", "input": {"message": "original"},
        "expected_output": {"contains": "EVAL-MARKER"},
    }, "execution_config": {"candidate_type": "agent_version", "model_ref": ref, **pin}}
    if mismatched_version:
        with pytest.raises(RuntimeError, match="VERSION_PIN_MISMATCH"):
            await runner(**kwargs)
    else:
        result = await runner(**kwargs)
        assert result["fingerprint"]["model_ref_verified"] is True
        assert result["fingerprint"]["agent_version_id"] == pin["agent_version_id"]
        assert result["usage"] == {"input_tokens": 13, "output_tokens": 7, "total_tokens": 20}
        assert result["detail"]["spans"][0]["span_kind"] == "model_invocation"
    candidate.run.assert_not_awaited()
    repository.get_candidate_runtime_evidence.assert_awaited_once_with(
        tenant_id="tenant-a", run_case_id="case-a", run_id=evidence["run_id"],
    )


@pytest.mark.asyncio
async def test_live_runner_enriches_sse_only_trace_before_contract_evaluation(monkeypatch):
    ref, evidence = _evidence()
    trace = {"trace_id": evidence["run_id"], "status": "succeeded", "metadata": {},
             "user_id": "eval-user", "model_id": ref["model_id"], "output_preview": "EVAL-MARKER", "spans": [], "metrics": {}}
    candidate = SimpleNamespace(run=AsyncMock(return_value=EvalCandidateResult(
        trace_id=evidence["run_id"], output="EVAL-MARKER", trace_payload=trace,
        fingerprint={"runtime_revision": "runtime-v1"},
    )))
    detail = None

    async def ingest(**kwargs):
        nonlocal detail
        payload = kwargs["payload"]["trace"]
        detail = {"trace": {**payload, **payload["metrics"]}, "spans": payload["spans"]}

    async def load(**_kwargs):
        return detail

    repository = SimpleNamespace(list_traces=AsyncMock(return_value=([], 0)),
                                 get_trace_detail=load, ingest_trace=ingest,
                                 get_candidate_runtime_evidence=AsyncMock(return_value=evidence))
    monkeypatch.setattr(eval_outbox_worker, "_eval_candidate_client", candidate)
    run_case = {"run_case_id": "case-a", "case_id": "smoke", "input": {"message": "EVAL-MARKER"},
                "expected_output": {"contains": ["EVAL-MARKER"]},
                "expected_trajectory": {"required_span_kinds": ["model_invocation"]}}
    runner = eval_outbox_worker._build_candidate_runner(repository)
    result = await runner(tenant_id="tenant-a", run_case=run_case, execution_config={"model_ref": ref})
    assert candidate.run.await_args.kwargs["message"] == "EVAL-MARKER"
    assert result["contract_result"]["passed"] is True
    assert result["usage"]["total_tokens"] == 20
    assert result["fingerprint"]["model_ref"] == ref
    assert detail["trace"]["metadata"]["candidate_fingerprint"]["model_ref_verified"] is True
    # Recovery uses the persisted proof and never launches a second candidate.
    restored = await runner(tenant_id="tenant-a", run_case={**run_case, "candidate_trace_id": evidence["run_id"]},
                            execution_config={"model_ref": ref})
    assert restored["fingerprint"] == result["fingerprint"]
    assert restored["contract_result"]["passed"] is True
    assert candidate.run.await_count == 1
