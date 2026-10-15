from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    _retry_source_cases,
)


def _case(case_id: str, *, status: str = "failed", dispatch: str = "accepted") -> dict[str, Any]:
    return {
        "run_case_id": f"source-{case_id}",
        "case_id": case_id,
        "example_id": None,
        "trial_index": 3,
        "status": status,
        "dispatch_state": dispatch,
        "candidate_trace_id": "11111111-1111-4111-8111-111111111111",
        "input": {"message": f"frozen {case_id}"},
        "expected_output": {"contains": ["answer"]},
        "expected_trajectory": {},
        "assertions": [],
        "metadata": {"case_id": case_id},
        "observed_metrics": {
            "behavior_pass": False,
            "execution_succeeded": True,
            "trace_id": "11111111-1111-4111-8111-111111111111",
            "tool_trajectory": [],
            "exit_reason": "completed",
        },
    }


def test_retry_selects_only_named_failed_cases_and_blocks_uncertain_dispatch() -> None:
    rows = [_case("failed"), _case("passed", status="succeeded")]
    rows[1]["observed_metrics"]["behavior_pass"] = True
    assert [row["case_id"] for row in _retry_source_cases(rows, ["failed"])] == ["failed"]
    with pytest.raises(ValueError, match="eval_retry_case_not_failed"):
        _retry_source_cases(rows, ["passed"])
    with pytest.raises(ValueError, match="eval_retry_case_ids_invalid"):
        _retry_source_cases(rows, ["failed", "failed"])
    uncertain = _case("uncertain", dispatch="reconcile_required")
    with pytest.raises(ValueError, match="eval_retry_side_effect_unconfirmed"):
        _retry_source_cases([uncertain], ["uncertain"])
    tool_effect = _case("tool-effect")
    tool_effect["observed_metrics"]["tool_trajectory"] = [{"name": "write", "status": "succeeded"}]
    with pytest.raises(ValueError, match="eval_retry_side_effect_unconfirmed"):
        _retry_source_cases([tool_effect], ["tool-effect"])
    unknown_effect = _case("unknown-effect")
    unknown_effect["observed_metrics"].pop("tool_trajectory")
    with pytest.raises(ValueError, match="eval_retry_side_effect_unconfirmed"):
        _retry_source_cases([unknown_effect], ["unknown-effect"])
    kb_case = _case("kb-case")
    kb_case["metadata"]["source_kind"] = "kb_failure"
    with pytest.raises(ValueError, match="eval_retry_kb_source_requires_fresh_authorization"):
        _retry_source_cases([kb_case], ["kb-case"])
    no_dispatch = _case("no-dispatch", dispatch="not_started")
    no_dispatch["candidate_trace_id"] = None
    no_dispatch["observed_metrics"] = {"execution_succeeded": False}
    assert _retry_source_cases([no_dispatch], ["no-dispatch"]) == [no_dispatch]


class _Transaction:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _Connection:
    def __init__(self) -> None:
        self.source = {
            "run_mode": "live_candidate", "status": "failed",
            "experiment_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "evaluator_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "dataset_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "baseline_run_id": None, "evaluator_suite_hash": "frozen-suite",
            "candidate_fingerprint": {"model_ref": {"model_id": "frozen-model"}},
            "execution_config": {"model_id": "frozen-model", "evaluators": [
                {"evaluator_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"},
            ]},
            "target_snapshot": {
                "candidate_label": "original", "dataset_manifest_hash": "original-hash",
                "dataset_kb_linked": False,
            },
        }
        self.dataset_metadata: dict[str, Any] = {}
        self.run_insert_args: tuple[Any, ...] | None = None
        self.case_insert_args: list[tuple[Any, ...]] | None = None
        self.existing_retry: dict[str, Any] | None = None
        self.run_insert_count = 0
        self.runtime_tool_receipt = False

    async def __aenter__(self) -> _Connection:
        return self

    async def __aexit__(self, *_args: Any) -> None:
        return None

    def transaction(self) -> _Transaction:
        return _Transaction()

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "SELECT metadata FROM eval_datasets" in query:
            return {"metadata": self.dataset_metadata}
        if "SELECT run_id, status, target_snapshot FROM eval_experiment_runs" in query:
            return self.existing_retry
        if "SELECT * FROM eval_experiment_runs" in query:
            return self.source
        if "INSERT INTO eval_experiment_runs" in query:
            self.run_insert_count += 1
            self.run_insert_args = args
            return {"run_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd"}
        if "INSERT INTO agent_trace_outbox" in query:
            return {"job_id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"}
        raise AssertionError(query)

    async def execute(self, query: str, *_args: Any) -> str:
        assert "UPDATE eval_experiment_runs" in query
        assert self.run_insert_args is not None
        snapshot = json.loads(self.run_insert_args[9])
        snapshot["retry_job_id"] = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        self.existing_retry = {
            "run_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            "status": "queued", "target_snapshot": snapshot,
        }
        return "UPDATE 1"

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        assert "FROM eval_experiment_run_cases" in query
        return [_case("failed")]

    async def executemany(self, query: str, args: list[tuple[Any, ...]]) -> None:
        assert "INSERT INTO eval_experiment_run_cases" in query
        self.case_insert_args = args

    async def fetchval(self, query: str, *args: Any) -> bool:
        assert "assistant_capability_executions" in query and "assistant_runtime_items" in query
        assert args[0] == "tenant-a"
        return self.runtime_tool_receipt


@pytest.mark.asyncio
async def test_retry_rechecks_original_tool_ledger_when_old_trace_omits_tools() -> None:
    conn = _Connection()
    conn.runtime_tool_receipt = True
    repo = AgentTraceRepository(SimpleNamespace(_pool=SimpleNamespace(acquire=lambda: conn), enabled=True))
    with pytest.raises(ValueError, match="eval_retry_side_effect_unconfirmed"):
        await repo.retry_failed_experiment_cases(
            tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
            case_ids=["failed"], created_by="operator", idempotency_key="request-123",
        )
    assert conn.run_insert_count == 0


@pytest.mark.asyncio
async def test_retry_creates_one_independent_attempt_from_frozen_inputs() -> None:
    conn = _Connection()
    repo = AgentTraceRepository(SimpleNamespace(_pool=SimpleNamespace(acquire=lambda: conn), enabled=True))

    result = await repo.retry_failed_experiment_cases(
        tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
        case_ids=["failed"], created_by="operator", idempotency_key="request-123",
    )

    assert result == {
        "job_id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
        "run_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "status": "queued",
    }
    assert conn.run_insert_args is not None
    assert json.loads(conn.run_insert_args[8])["model_id"] == "frozen-model"
    snapshot = json.loads(conn.run_insert_args[9])
    assert snapshot["retry_of_run_id"] == "ffffffff-ffff-4fff-8fff-ffffffffffff"
    assert snapshot["retry_source_run_case_ids"] == ["source-failed"]
    assert snapshot["dataset_manifest_hash"] != "original-hash"
    assert conn.case_insert_args is not None
    assert len(conn.case_insert_args) == 1
    assert json.loads(conn.case_insert_args[0][4]) == {"message": "frozen failed"}

    duplicate = await repo.retry_failed_experiment_cases(
        tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
        case_ids=["failed"], created_by="operator", idempotency_key="request-123",
    )
    assert duplicate == result
    assert conn.run_insert_count == 1
    with pytest.raises(ValueError, match="eval_retry_idempotency_conflict"):
        await repo.retry_failed_experiment_cases(
            tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
            case_ids=["different"], created_by="operator", idempotency_key="request-123",
        )


@pytest.mark.asyncio
async def test_retry_fails_closed_when_kb_dataset_link_or_provenance_is_uncertain() -> None:
    conn = _Connection()
    repo = AgentTraceRepository(SimpleNamespace(_pool=SimpleNamespace(acquire=lambda: conn), enabled=True))
    conn.dataset_metadata = {"kb_dataset_id": "revoked-kb"}
    with pytest.raises(ValueError, match="eval_retry_kb_source_requires_fresh_authorization"):
        await repo.retry_failed_experiment_cases(
            tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
            case_ids=["failed"], created_by="operator", idempotency_key="request-kb",
        )
    assert conn.run_insert_count == 0
    conn.dataset_metadata = {}
    conn.source["target_snapshot"].pop("dataset_kb_linked")
    with pytest.raises(ValueError, match="eval_retry_dataset_provenance_unverified"):
        await repo.retry_failed_experiment_cases(
            tenant_id="tenant-a", run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
            case_ids=["failed"], created_by="operator", idempotency_key="request-legacy",
        )
    assert conn.run_insert_count == 0


@pytest.mark.asyncio
async def test_retry_idempotent_replay_survives_later_dataset_revocation() -> None:
    conn = _Connection()
    repo = AgentTraceRepository(SimpleNamespace(_pool=SimpleNamespace(acquire=lambda: conn), enabled=True))
    request = {
        "tenant_id": "tenant-a", "run_id": "ffffffff-ffff-4fff-8fff-ffffffffffff",
        "case_ids": ["failed"], "created_by": "operator", "idempotency_key": "request-123",
    }
    accepted = await repo.retry_failed_experiment_cases(**request)
    conn.dataset_metadata = {"kb_dataset_id": "revoked-after-acceptance"}

    replay = await repo.retry_failed_experiment_cases(**request)

    assert replay == accepted
    assert conn.run_insert_count == 1


@pytest.mark.asyncio
async def test_llm_judge_snapshot_freezes_server_model_ref() -> None:
    repo = AgentTraceRepository(SimpleNamespace(_pool=None, enabled=False))

    async def model_ref(**_kwargs: Any) -> dict[str, Any]:
        return {"model_id": "judge-model", "provider_id": "judge-provider", "price_version": "v1"}

    repo.freeze_eval_model_ref = model_ref  # type: ignore[method-assign]
    frozen = await repo.freeze_eval_judge(
        tenant_id="tenant-a",
        evaluator={
            "evaluator_id": "judge-a", "evaluator_type": "llm_judge",
            "metadata": {"judge_model_id": "judge-model"},
        },
    )

    assert frozen["metadata"]["judge_model_ref"]["provider_id"] == "judge-provider"
