from __future__ import annotations

import importlib.util
import json
from pathlib import Path
from typing import Any

import pytest

from src.services.eval.agent_observation_adapter import (
    PRODUCER_EVIDENCE_SCHEMA_VERSION,
    adapt_producer_case,
)
from src.services.eval.golden import evaluate_case, load_jsonl

GOLDEN = Path("tests/fixtures/eval/golden/assistant_regression_v1.jsonl")
RUN_ID = "11111111-1111-4111-8111-111111111111"
TENANT_ID = "tenant-a"
SESSION_ID = "session-a"


def _golden(case_id: str) -> dict[str, Any]:
    return next(row for row in load_jsonl(GOLDEN) if row["case_id"] == case_id)


def _snapshot(*, budget: dict[str, Any] | None = None) -> dict[str, Any]:
    bootstrap = {"run_budget": budget} if budget else {}
    return {
        "schema_version": "assistant-turn-contract/v1",
        "run_id": RUN_ID,
        "tenant_id": TENANT_ID,
        "session_id": SESSION_ID,
        "thread_id": SESSION_ID,
        "snapshot_id": "ctx-test",
        "bootstrap": bootstrap,
    }


def _envelope(
    *,
    status: str,
    exit_reason: str,
    snapshot: dict[str, Any] | None = None,
) -> dict[str, Any]:
    context_snapshot = snapshot or _snapshot()
    return {
        "schema_version": "assistant-turn-contract/v1",
        "run_id": RUN_ID,
        "tenant_id": TENANT_ID,
        "session_id": SESSION_ID,
        "thread_id": SESSION_ID,
        "status": status,
        "exit_reason": exit_reason,
        "context_snapshot": context_snapshot,
    }


def _terminal_event(envelope: dict[str, Any]) -> dict[str, Any]:
    return {
        "event_type": "run_finished" if envelope["status"] == "succeeded" else "run_error",
        "data": {
            "run_id": RUN_ID,
            "session_id": SESSION_ID,
            "terminal_envelope": envelope,
            "context_snapshot": envelope["context_snapshot"],
        },
    }


def _artifact(
    *,
    turns: list[dict[str, Any]],
    output: str,
    span_kinds: list[str],
    status: str = "succeeded",
) -> dict[str, Any]:
    return {
        "case_id": "case-under-test",
        "producer": {
            "schema_version": PRODUCER_EVIDENCE_SCHEMA_VERSION,
            "trace": {
                "run_id": RUN_ID,
                "tenant_id": TENANT_ID,
                "session_id": SESSION_ID,
                "status": status,
                "output_preview": output,
                "spans": [{"span_kind": value} for value in span_kinds],
            },
            "turns": turns,
        },
    }


def _tool_turn(turn_index: int, call_id: str, tool_name: str) -> dict[str, Any]:
    envelope = _envelope(status="succeeded", exit_reason="succeeded")
    events = [
        {
            "event_type": "tool_call_start",
            "data": {
                "run_id": RUN_ID,
                "session_id": SESSION_ID,
                "tool_call_id": call_id,
                "tool_name": tool_name,
                "arguments": {"query": call_id},
            },
        },
        {
            "event_type": "tool_call_result",
            "data": {
                "run_id": RUN_ID,
                "session_id": SESSION_ID,
                "tool_call_id": call_id,
                "tool_name": tool_name,
                "status": "completed",
                "success": True,
            },
        },
        {
            "event_type": "tool_call_end",
            "data": {
                "run_id": RUN_ID,
                "session_id": SESSION_ID,
                "tool_call_id": call_id,
                "tool_name": tool_name,
                "status": "completed",
                "success": True,
            },
        },
        _terminal_event(envelope),
    ]
    return {
        "turn_index": turn_index,
        "events": events,
        "terminal_envelope": envelope,
        "context_snapshot": envelope["context_snapshot"],
    }


def _tool_pair_artifact() -> dict[str, Any]:
    row = _artifact(
        turns=[
            _tool_turn(1, "call-search-1", "search_kb"),
            _tool_turn(2, "call-read-1", "read_record"),
        ],
        output="Both tools completed with canonical lifecycle receipts.",
        span_kinds=["lifecycle", "tool_execution"],
    )
    row["case_id"] = "assistant.stateful.tool_pairing"
    return row


def _runtime_hash(arguments: dict[str, Any]) -> str:
    import hashlib

    stripped = {
        key: value
        for key, value in arguments.items()
        if key not in {"_approval_id", "_middleware_approval_required", "_steer_payload"}
    }
    encoded = json.dumps(stripped, ensure_ascii=False, sort_keys=True, default=str).encode()
    return hashlib.sha256(encoded).hexdigest()


def _checkpoint(
    *,
    checkpoint_id: str,
    phase: str,
    arguments_hash: str,
    status: str,
    durability: str = "database",
) -> dict[str, Any]:
    return {
        "checkpoint_id": checkpoint_id,
        "run_id": RUN_ID,
        "tenant_id": TENANT_ID,
        "session_id": SESSION_ID,
        "phase": phase,
        "status": status,
        "approval_id": "approval-1",
        "pending_tool": {
            "tool_id": "call-write-1",
            "tool_name": "approved_write",
            "arguments_hash": arguments_hash,
        },
        "checkpoint_receipt": {"committed": True, "durability": durability},
    }


def _hitl_artifact() -> dict[str, Any]:
    arguments = {
        "document_id": "doc-7",
        "mode": "append",
    }
    arguments_hash = _runtime_hash(arguments)
    pause_envelope = _envelope(status="blocked", exit_reason="approval_pending")
    resume_envelope = _envelope(status="succeeded", exit_reason="succeeded")
    pause = {
        "turn_index": 1,
        "events": [
            {
                "event_type": "approval_required",
                "data": {
                    "run_id": RUN_ID,
                    "session_id": SESSION_ID,
                    "tool_id": "call-write-1",
                    "tool_name": "approved_write",
                    "approval_id": "approval-1",
                    "checkpoint_id": "checkpoint-hitl-1",
                    "terminal_envelope": pause_envelope,
                    "context_snapshot": pause_envelope["context_snapshot"],
                },
            }
        ],
        "terminal_envelope": pause_envelope,
        "context_snapshot": pause_envelope["context_snapshot"],
        "checkpoint": _checkpoint(
            checkpoint_id="checkpoint-hitl-1",
            phase="approval_pending",
            arguments_hash=arguments_hash,
            status="blocked",
        ),
    }
    resume = {
        "turn_index": 2,
        "events": [
            {
                "event_type": "tool_call_start",
                "data": {
                    "run_id": RUN_ID,
                    "session_id": SESSION_ID,
                    "tool_call_id": "call-write-1",
                    "tool_name": "approved_write",
                    "arguments": arguments,
                },
            },
            {
                "event_type": "tool_call_result",
                "data": {
                    "run_id": RUN_ID,
                    "session_id": SESSION_ID,
                    "tool_call_id": "call-write-1",
                    "tool_name": "approved_write",
                    "status": "completed",
                    "success": True,
                },
            },
            {
                "event_type": "tool_call_end",
                "data": {
                    "run_id": RUN_ID,
                    "session_id": SESSION_ID,
                    "tool_call_id": "call-write-1",
                    "tool_name": "approved_write",
                    "status": "completed",
                    "success": True,
                },
            },
            {
                "event_type": "approval_result",
                "data": {
                    "run_id": RUN_ID,
                    "session_id": SESSION_ID,
                    "tool_id": "call-write-1",
                    "tool_name": "approved_write",
                    "approval_id": "approval-1",
                    "approved": True,
                    "success": True,
                },
            },
            _terminal_event(resume_envelope),
        ],
        "terminal_envelope": resume_envelope,
        "context_snapshot": resume_envelope["context_snapshot"],
        "checkpoint": _checkpoint(
            checkpoint_id="checkpoint-resume-1",
            phase="tool_call_pending",
            arguments_hash=arguments_hash,
            status="running",
        ),
    }
    row = _artifact(
        turns=[pause, resume],
        output="The approved operation resumed safely from its checkpoint.",
        span_kinds=["lifecycle", "tool_execution"],
    )
    row["case_id"] = "assistant.stateful.hitl_pause_resume"
    return row


def _budget_snapshot(model_turns: int, *, exhausted: bool) -> dict[str, Any]:
    maximum = 3
    return {
        "schema_version": "assistant-run-budget/v1",
        "limits": {
            "max_model_turns": maximum,
            "max_tool_calls": 4,
            "max_parallel_tool_calls": 2,
            "max_wall_time_seconds": 300.0,
            "max_tool_result_bytes": 1000,
        },
        "usage": {
            "model_turns": model_turns,
            "tool_calls": 1,
            "tool_result_bytes": 10,
            "elapsed_ms": model_turns * 10,
        },
        "remaining": {
            "model_turns": maximum - model_turns,
            "tool_calls": 3,
            "tool_result_bytes": 990,
            "wall_time_ms": 300_000 - model_turns * 10,
        },
        "exhausted": exhausted,
        "reason": "model_turns_exhausted" if exhausted else None,
    }


def _budget_artifact() -> dict[str, Any]:
    before = _budget_snapshot(2, exhausted=False)
    exhausted = _budget_snapshot(3, exhausted=True)
    snapshot = _snapshot(budget=before)
    envelope = _envelope(
        status="failed", exit_reason="run_budget_exceeded", snapshot=snapshot
    )
    return _artifact(
        turns=[
            {
                "turn_index": 1,
                "events": [
                    {
                        "event_type": "run_budget_exceeded",
                        "data": {
                            "run_id": RUN_ID,
                            "session_id": SESSION_ID,
                            "schema_version": "assistant-run-budget/v1",
                            "status": "exhausted",
                            "reason": "model_turns_exhausted",
                            "dimension": "model_turns",
                            "limit": 3,
                            "used": 3,
                            "requested": 4,
                            "budget": exhausted,
                        },
                    },
                    _terminal_event(envelope),
                ],
                "terminal_envelope": envelope,
                "context_snapshot": snapshot,
            }
        ],
        output="budget exhausted",
        span_kinds=["lifecycle"],
        status="failed",
    )


def test_adapter_projects_canonical_tool_lifecycle_and_passes_contract() -> None:
    case_id, replay = adapt_producer_case(_tool_pair_artifact())

    assert case_id == "assistant.stateful.tool_pairing"
    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "verified"
    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "verified"
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is True, result["failures"]


def test_adapter_projects_only_database_bound_hitl_receipt() -> None:
    case_id, replay = adapt_producer_case(_hitl_artifact())

    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "verified"
    assert replay["turns"][0]["tool_calls"][0]["approval_required"] is True
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is True, result["failures"]


def test_adapter_requires_each_hitl_checkpoint_to_bind_its_own_identity() -> None:
    row = _hitl_artifact()
    for turn in row["producer"]["turns"]:
        checkpoint = turn["checkpoint"]
        for field in ("run_id", "tenant_id", "session_id"):
            checkpoint.pop(field)

    case_id, replay = adapt_producer_case(row)
    result = evaluate_case(_golden(case_id), replay)

    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"
    assert result["passed"] is False


def test_adapter_accepts_monotonic_bootstrap_to_exhausted_budget_snapshots() -> None:
    row = _budget_artifact()

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "verified"
    assert replay["turns"][0]["budget"] == {
        "iteration": 3,
        "max_iterations": 3,
        "remaining": 0,
    }


def test_adapter_rejects_negative_budget_arithmetic_in_all_bound_snapshots() -> None:
    row = _budget_artifact()
    turn = row["producer"]["turns"][0]
    snapshots = [
        turn["context_snapshot"]["bootstrap"]["run_budget"],
        turn["events"][0]["data"]["budget"],
    ]
    for snapshot in snapshots:
        snapshot["limits"]["max_tool_calls"] = -10
        snapshot["usage"]["tool_calls"] = -20
        snapshot["remaining"]["tool_calls"] = -30

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


@pytest.mark.parametrize(
    ("section", "field", "value"),
    [
        ("limits", "max_parallel_tool_calls", 0),
        ("limits", "max_wall_time_seconds", float("nan")),
        ("usage", "tool_result_bytes", -1),
        ("remaining", "wall_time_ms", -1),
        ("remaining", "tool_result_bytes", 991),
    ],
)
def test_adapter_rejects_invalid_budget_types_ranges_and_remaining_arithmetic(
    section: str,
    field: str,
    value: object,
) -> None:
    row = _budget_artifact()
    start_budget = row["producer"]["turns"][0]["context_snapshot"]["bootstrap"][
        "run_budget"
    ]
    start_budget[section][field] = value

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("dimension", "tool_calls"),
        ("limit", 4),
        ("used", 2),
        ("requested", 3),
    ],
)
def test_adapter_rejects_unbound_budget_exhaustion_event_arithmetic(
    field: str,
    value: object,
) -> None:
    row = _budget_artifact()
    row["producer"]["turns"][0]["events"][0]["data"][field] = value

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_adapter_marks_unrecorded_plan_claim_unknown_and_gate_fails() -> None:
    row = _tool_pair_artifact()
    row["case_id"] = "assistant.stateful.plan_retention"
    case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["plan"]["status"] == "unknown"
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is False
    assert "canonical producer evidence for plan is unknown" in result["failures"]


def test_adapter_rejects_cross_run_join_even_when_tool_rows_otherwise_pair() -> None:
    row = _tool_pair_artifact()
    row["producer"]["turns"][1]["events"][0]["data"]["run_id"] = "other-run"
    case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"
    assert evaluate_case(_golden(case_id), replay)["passed"] is False


def test_adapter_rejects_trace_only_non_stateful_case_without_turn_receipts() -> None:
    row = _artifact(
        turns=[],
        output="The refund policy is available.",
        span_kinds=["lifecycle", "model_invocation"],
    )
    row["case_id"] = "assistant.refund_policy.basic"
    case_id, replay = adapt_producer_case(row)

    assert replay["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"
    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is False
    assert "canonical producer adapter integrity is not_verified" in result["failures"]


def test_adapter_does_not_treat_process_checkpoint_as_durable_hitl_proof() -> None:
    row = _hitl_artifact()
    row["producer"]["turns"][0]["checkpoint"]["checkpoint_receipt"][
        "durability"
    ] = "process"
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "unknown"


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("phase", "tool_call_completed"),
        ("checkpoint_id", ""),
        ("status", "completed"),
    ],
)
def test_adapter_requires_real_pre_dispatch_resume_checkpoint(
    field: str,
    value: str,
) -> None:
    row = _hitl_artifact()
    row["producer"]["turns"][1]["checkpoint"][field] = value
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("_approval_id", "foreign-approval"),
        ("_middleware_approval_required", False),
    ],
)
def test_adapter_rejects_runtime_control_arguments_in_canonical_dispatch(
    key: str,
    value: object,
) -> None:
    row = _hitl_artifact()
    row["producer"]["turns"][1]["events"][0]["data"]["arguments"][key] = value
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"
    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_adapter_rejects_hitl_result_before_canonical_end_receipt() -> None:
    row = _hitl_artifact()
    resume_events = row["producer"]["turns"][1]["events"]
    approval_result = resume_events.pop(3)
    resume_events.insert(2, approval_result)
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["hitl"]["status"] == "unknown"


def test_adapter_rejects_trace_terminal_status_conflict() -> None:
    row = _tool_pair_artifact()
    row["producer"]["trace"]["status"] = "failed"
    _case_id, replay = adapt_producer_case(row)

    assert replay["status"] == "unknown"
    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"


def test_adapter_rejects_tool_terminal_name_conflict() -> None:
    row = _tool_pair_artifact()
    row["producer"]["turns"][0]["events"][1]["data"]["tool_name"] = "other_tool"
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"


def test_adapter_rejects_conflicting_tool_name_aliases() -> None:
    row = _tool_pair_artifact()
    row["producer"]["turns"][0]["events"][0]["data"]["name"] = "other_tool"
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_adapter_rejects_missing_identity_on_canonical_lifecycle_event() -> None:
    row = _tool_pair_artifact()
    event_data = row["producer"]["turns"][0]["events"][1]["data"]
    event_data.pop("run_id")
    event_data.pop("session_id")
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_adapter_rejects_missing_terminal_event_even_with_trace_and_snapshot() -> None:
    row = _tool_pair_artifact()
    first_turn = row["producer"]["turns"][0]
    first_turn["events"].pop()
    first_turn.pop("terminal_envelope")
    _case_id, replay = adapt_producer_case(row)

    assert replay["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"
    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"


def test_adapter_rejects_terminal_exit_reason_conflict() -> None:
    row = _tool_pair_artifact()
    envelope = row["producer"]["turns"][0]["terminal_envelope"]
    envelope["exit_reason"] = "tool_error"
    row["producer"]["turns"][0]["events"][-1]["data"]["terminal_envelope"] = envelope
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"


@pytest.mark.parametrize("receipt", ["terminal_envelope", "context_snapshot"])
def test_adapter_requires_versioned_nested_turn_receipts(receipt: str) -> None:
    row = _tool_pair_artifact()
    row["producer"]["turns"][0][receipt].pop("schema_version")
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_adapter_accepts_graceful_max_iterations_terminal_mapping() -> None:
    row = _tool_pair_artifact()
    envelope = row["producer"]["turns"][1]["terminal_envelope"]
    envelope["exit_reason"] = "max_iterations"
    _case_id, replay = adapt_producer_case(row)

    assert replay["status"] == "succeeded"
    assert replay["exit_reason"] == "max_iterations"
    assert replay["adapter_evidence"]["components"]["binding"]["status"] == "verified"


def test_adapter_rejects_tool_terminal_receipt_without_name_or_success() -> None:
    row = _tool_pair_artifact()
    result = row["producer"]["turns"][0]["events"][1]["data"]
    result.pop("tool_name")
    result.pop("success")
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"


def test_adapter_rejects_tool_lifecycle_crossing_terminal_turn_boundary() -> None:
    row = _tool_pair_artifact()
    first_events = row["producer"]["turns"][0]["events"]
    second_events = row["producer"]["turns"][1]["events"]
    moved_terminals = first_events[1:3]
    del first_events[1:3]
    second_events[0:0] = moved_terminals
    case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is False


def test_adapter_rejects_tool_status_success_conflict() -> None:
    row = _tool_pair_artifact()
    row["producer"]["turns"][0]["events"][1]["data"]["success"] = False
    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def test_stateful_tool_pairing_requires_successful_terminal_results() -> None:
    row = _tool_pair_artifact()
    for turn in row["producer"]["turns"]:
        for event in turn["events"]:
            if event["event_type"] in {"tool_call_result", "tool_call_end"}:
                event["data"]["status"] = "failed"
                event["data"]["success"] = False
    case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["tool_pairing"]["status"] == "verified"
    result = evaluate_case(_golden(case_id), replay)
    assert result["passed"] is False
    assert "tool pairing requires successful terminal results" in result["failures"]


def test_adapter_rejects_bootstrap_only_budget_snapshot() -> None:
    snapshot = _snapshot(budget=_budget_snapshot(1, exhausted=False))
    envelope = _envelope(status="succeeded", exit_reason="succeeded", snapshot=snapshot)
    row = _artifact(
        turns=[
            {
                "turn_index": 1,
                "events": [_terminal_event(envelope)],
                "terminal_envelope": envelope,
                "context_snapshot": snapshot,
            }
        ],
        output="budget",
        span_kinds=["lifecycle"],
    )

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"


def test_adapter_rejects_non_monotonic_budget_receipts() -> None:
    before = _budget_snapshot(2, exhausted=False)
    regressed = _budget_snapshot(1, exhausted=False)
    snapshot = _snapshot(budget=before)
    envelope = _envelope(
        status="failed", exit_reason="run_budget_exceeded", snapshot=snapshot
    )
    row = _artifact(
        turns=[
            {
                "turn_index": 1,
                "events": [
                    {
                        "event_type": "run_budget_exceeded",
                        "data": {
                            "run_id": RUN_ID,
                            "session_id": SESSION_ID,
                            "schema_version": "assistant-run-budget/v1",
                            "status": "exhausted",
                            "reason": "model_turns_exhausted",
                            "budget": regressed,
                        },
                    },
                    _terminal_event(envelope),
                ],
                "terminal_envelope": envelope,
                "context_snapshot": snapshot,
            }
        ],
        output="budget",
        span_kinds=["lifecycle"],
        status="failed",
    )

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"


def test_adapter_rejects_non_exhausted_budget_exceeded_receipt() -> None:
    before = _budget_snapshot(2, exhausted=False)
    fake_exhaustion = _budget_snapshot(3, exhausted=False)
    snapshot = _snapshot(budget=before)
    envelope = _envelope(
        status="failed", exit_reason="run_budget_exceeded", snapshot=snapshot
    )
    row = _artifact(
        turns=[
            {
                "turn_index": 1,
                "events": [
                    {
                        "event_type": "run_budget_exceeded",
                        "data": {
                            "run_id": RUN_ID,
                            "session_id": SESSION_ID,
                            "schema_version": "assistant-run-budget/v1",
                            "status": "exhausted",
                            "reason": "model_turns_exhausted",
                            "budget": fake_exhaustion,
                        },
                    },
                    _terminal_event(envelope),
                ],
                "terminal_envelope": envelope,
                "context_snapshot": snapshot,
            }
        ],
        output="budget",
        span_kinds=["lifecycle"],
        status="failed",
    )

    _case_id, replay = adapt_producer_case(row)

    assert replay["adapter_evidence"]["components"]["budget"]["status"] == "unknown"
    assert replay["adapter_evidence"]["integrity"]["status"] == "not_verified"


def _load_eval_golden_main():
    script_path = Path(__file__).resolve().parents[3] / "scripts" / "eval_golden.py"
    spec = importlib.util.spec_from_file_location("eval_golden_script", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load Eval golden script: {script_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.main


def test_recorded_candidate_cli_is_explicit_and_defaults_to_offline_evidence(
    tmp_path: Path,
) -> None:
    case = _golden("assistant.stateful.tool_pairing")
    expectation = tmp_path / "candidate.jsonl"
    artifacts = tmp_path / "producer.jsonl"
    output = tmp_path / "candidate-output.json"
    markdown = tmp_path / "candidate-output.md"
    expectation.write_text(json.dumps(case) + "\n", encoding="utf-8")
    artifacts.write_text(json.dumps(_tool_pair_artifact()) + "\n", encoding="utf-8")

    exit_code = _load_eval_golden_main()(
        [
            "candidate",
            str(expectation),
            "--producer-artifacts",
            str(artifacts),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    payload = json.loads(output.read_text(encoding="utf-8"))
    assert exit_code == 0
    assert payload["evidence_scope"] == "recorded_runtime_candidate"
    assert payload["suite_scope"] == "partial"
    assert payload["evidence_tiers"]["fixture_contract"] == "provided_cases_verified"
    assert payload["evidence_tiers"]["runtime_artifact_adapter"] == "verified"
    assert (
        payload["evidence_tiers"]["recorded_runtime_artifacts"]
        == "provided_cases_verified"
    )
    assert payload["evidence_tiers"]["live_runtime_execution"] == "not_run"
    assert payload["evidence_tiers"]["real_provider_call"] == "not_run"


def test_recorded_candidate_cli_does_not_verify_invalid_adapter_evidence(
    tmp_path: Path,
) -> None:
    case = _golden("assistant.stateful.tool_pairing")
    artifact = _tool_pair_artifact()
    artifact["producer"]["turns"][0]["events"][1]["data"]["success"] = False
    expectation = tmp_path / "candidate.jsonl"
    artifacts = tmp_path / "producer.jsonl"
    output = tmp_path / "candidate-output.json"
    markdown = tmp_path / "candidate-output.md"
    expectation.write_text(json.dumps(case) + "\n", encoding="utf-8")
    artifacts.write_text(json.dumps(artifact) + "\n", encoding="utf-8")

    exit_code = _load_eval_golden_main()(
        [
            "candidate",
            str(expectation),
            "--producer-artifacts",
            str(artifacts),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    payload = json.loads(output.read_text(encoding="utf-8"))
    assert exit_code == 1
    assert payload["gate"]["status"] == "fail"
    assert payload["observations"]["adapter"]["status"] == "not_verified"
    assert payload["evidence_tiers"]["runtime_artifact_adapter"] == "not_verified"
    assert payload["evidence_tiers"]["recorded_runtime_artifacts"] == "not_verified"
