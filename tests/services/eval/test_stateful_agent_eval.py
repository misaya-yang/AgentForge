from __future__ import annotations

import copy
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from src.services.eval.golden import (
    evaluate_case,
    load_jsonl,
    load_observations,
    validate_cases,
)

GOLDEN = Path("tests/fixtures/eval/golden/assistant_regression_v1.jsonl")
OBSERVATIONS = Path("tests/fixtures/eval/observations/assistant_regression_v1.jsonl")
STATEFUL_CASE_IDS = {
    "assistant.stateful.plan_retention",
    "assistant.stateful.tool_pairing",
    "assistant.stateful.budget_termination",
    "assistant.stateful.hitl_pause_resume",
    "assistant.stateful.compaction_retention",
    "assistant.security.prompt_injection",
    "assistant.security.tenant_isolation",
}


def _fixtures() -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    cases = {str(case["case_id"]): case for case in load_jsonl(GOLDEN)}
    return cases, load_observations(OBSERVATIONS)


def test_stateful_agent_fixture_covers_and_passes_all_e1_contracts() -> None:
    cases, observations = _fixtures()

    assert STATEFUL_CASE_IDS.issubset(cases)
    for case_id in STATEFUL_CASE_IDS:
        result = evaluate_case(cases[case_id], observations[case_id])
        assert result["passed"] is True, (case_id, result["failures"])
        assert result["stateful_pass"] is True
        assert result["trajectory_pass"] is True


def _break_plan(replay: dict[str, Any]) -> None:
    replay["turns"][1]["plan"]["plan_id"] = "changed-plan"


def _break_plan_with_missing_id(replay: dict[str, Any]) -> None:
    replay["turns"][1]["plan"]["plan_id"] = None


def _break_plan_completion_monotonicity(replay: dict[str, Any]) -> None:
    replay["turns"][1]["plan"]["completed_steps"] = []


def _break_tool_pair(replay: dict[str, Any]) -> None:
    replay["turns"][1]["tool_results"].pop()


def _break_budget(replay: dict[str, Any]) -> None:
    replay["turns"][1]["budget"]["remaining"] = 2


def _break_hitl(replay: dict[str, Any]) -> None:
    replay["turns"][1]["hitl"]["checkpoint_id"] = "other-checkpoint"


def _break_hitl_dispatch_evidence(replay: dict[str, Any]) -> None:
    replay["turns"][0]["tool_calls"][0]["dispatch_state"] = "dispatched"


def _break_hitl_postapproval_dispatch(replay: dict[str, Any]) -> None:
    replay["turns"][1]["tool_calls"] = []
    replay["turns"][1]["tool_results"] = []


def _break_hitl_result_order(replay: dict[str, Any]) -> None:
    replay["turns"][0]["tool_results"] = replay["turns"][1].pop("tool_results")


def _break_forbidden_tool_evidence(replay: dict[str, Any]) -> None:
    del replay["turns"][0]["tool_calls"]


def _break_compaction(replay: dict[str, Any]) -> None:
    replay["turns"][2]["compaction"]["retained_facts"] = ["account=acct-7"]


def _break_compaction_final_evidence(replay: dict[str, Any]) -> None:
    replay["turns"][0]["compaction"] = replay["turns"][2].pop("compaction")


def _break_injection(replay: dict[str, Any]) -> None:
    replay["turns"][1]["tool_calls"] = [
        {"call_id": "forbidden-call", "name": "export_private_data"}
    ]


def _break_tenant(replay: dict[str, Any]) -> None:
    replay["security"]["observed_tenant_ids"].append("tenant-b")


@pytest.mark.parametrize(
    ("case_id", "mutator", "failure_text"),
    [
        ("assistant.stateful.plan_retention", _break_plan, "plan_id changed"),
        (
            "assistant.stateful.plan_retention",
            _break_plan_with_missing_id,
            "plan_id changed",
        ),
        (
            "assistant.stateful.plan_retention",
            _break_plan_completion_monotonicity,
            "completed plan steps regressed",
        ),
        ("assistant.stateful.tool_pairing", _break_tool_pair, "not one-to-one"),
        ("assistant.stateful.budget_termination", _break_budget, "remaining count"),
        ("assistant.stateful.hitl_pause_resume", _break_hitl, "checkpoint identity"),
        (
            "assistant.stateful.hitl_pause_resume",
            _break_hitl_dispatch_evidence,
            "dispatched a tool before approval",
        ),
        (
            "assistant.stateful.hitl_pause_resume",
            _break_hitl_postapproval_dispatch,
            "insufficient postapproval tool dispatches",
        ),
        (
            "assistant.stateful.hitl_pause_resume",
            _break_hitl_result_order,
            "requires one terminal paired result",
        ),
        (
            "assistant.stateful.compaction_retention",
            _break_compaction,
            "dropped required facts",
        ),
        (
            "assistant.stateful.compaction_retention",
            _break_compaction_final_evidence,
            "requires evidence on the final turn",
        ),
        ("assistant.security.prompt_injection", _break_injection, "forbidden tools called"),
        (
            "assistant.security.prompt_injection",
            _break_forbidden_tool_evidence,
            "requires explicit tool_calls evidence",
        ),
        ("assistant.security.tenant_isolation", _break_tenant, "foreign or missing tenant"),
    ],
)
def test_stateful_agent_contracts_fail_closed_on_regression(
    case_id: str,
    mutator: Callable[[dict[str, Any]], None],
    failure_text: str,
) -> None:
    cases, observations = _fixtures()
    replay = copy.deepcopy(observations[case_id])
    mutator(replay)

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert result["stateful_pass"] is False
    assert failure_text in " ".join(result["failures"])


def test_stateful_contract_rejects_unknown_expectation_fields() -> None:
    cases, _observations = _fixtures()
    case = copy.deepcopy(cases["assistant.stateful.plan_retention"])
    case["expected_trajectory"]["stateful"]["future_rule"] = True

    validation = validate_cases([case])

    assert validation["valid"] is False
    assert "unsupported fields future_rule" in " ".join(validation["errors"][0]["errors"])


@pytest.mark.parametrize(
    ("case_id", "section", "field"),
    [
        ("assistant.stateful.plan_retention", "plan", "plan_id"),
        ("assistant.stateful.budget_termination", "budget", "max_iterations"),
        ("assistant.stateful.hitl_pause_resume", "hitl", "approved_calls"),
        ("assistant.stateful.hitl_pause_resume", "hitl", "protected_tools"),
        ("assistant.stateful.hitl_pause_resume", "hitl", "resume_count"),
        (
            "assistant.stateful.hitl_pause_resume",
            "hitl",
            "minimum_pending_approval_calls",
        ),
        (
            "assistant.stateful.hitl_pause_resume",
            "hitl",
            "minimum_postapproval_dispatches",
        ),
        (
            "assistant.stateful.compaction_retention",
            "compaction",
            "compaction_id",
        ),
        (
            "assistant.stateful.compaction_retention",
            "compaction",
            "max_dropped_required_facts",
        ),
    ],
)
def test_stateful_contract_rejects_missing_required_section_fields(
    case_id: str,
    section: str,
    field: str,
) -> None:
    cases, _observations = _fixtures()
    case = copy.deepcopy(cases[case_id])
    del case["expected_trajectory"]["stateful"][section][field]

    validation = validate_cases([case])

    assert validation["valid"] is False
    assert f"missing required fields {field}" in " ".join(validation["errors"][0]["errors"])


def test_stateful_evidence_rejects_non_integer_turn_index_without_crashing() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.plan_retention"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][0]["turn_index"] = "one"

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "stateful turn_index values must be integers" in result["failures"]


def test_max_iteration_contract_requires_terminal_turn_to_exhaust_budget() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.budget_termination"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["stateful"]["minimum_turns"] = 2
    case["expected_trajectory"]["stateful"]["budget"]["terminal_turn"] = 2
    replay = copy.deepcopy(observations[case_id])
    replay["turns"] = replay["turns"][:2]

    result = evaluate_case(case, replay)

    assert result["passed"] is False
    assert "terminal_turn must equal max_iterations" in result["failures"][0]


def test_plan_evidence_rejects_unhashable_step_values_without_crashing() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.plan_retention"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["plan"]["steps"] = [{"name": "research"}]

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "plan steps evidence must be a non-empty string list" in result["failures"]


def test_compaction_drop_tolerance_requires_consistent_missing_fact_evidence() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.compaction_retention"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["stateful"]["compaction"][  # type: ignore[index]
        "max_dropped_required_facts"
    ] = 1
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][2]["compaction"] = {
        "event": "post_compaction_snapshot",
        "compaction_id": "compaction-1",
        "retained_facts": ["account=acct-7"],
        "dropped_required_facts": ["plan=annual"],
    }

    allowed = evaluate_case(case, replay)
    replay["turns"][2]["compaction"]["dropped_required_facts"] = []
    inconsistent = evaluate_case(case, replay)

    assert allowed["passed"] is True
    assert inconsistent["passed"] is False
    assert any(
        "retained and dropped fact evidence is inconsistent" in failure
        for failure in inconsistent["failures"]
    )


def test_tool_result_cannot_precede_its_call() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][0]["tool_results"] = [
        {"tool_call_id": "call-read-1", "status": "succeeded"}
    ]
    replay["turns"][1]["tool_results"] = [
        {"tool_call_id": "call-search-1", "status": "succeeded"}
    ]

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "tool result appeared before its tool call" in result["failures"]


@pytest.mark.parametrize(
    "terminal_status",
    [
        "completed",
        "succeeded",
        "error",
        "failed",
        "cancelled",
        "budget_rejected",
        "invalid_arguments",
        "not_executed",
        "deduplicated",
    ],
)
def test_tool_pairing_requires_success_when_the_contract_claims_completion(
    terminal_status: str,
) -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    replay = copy.deepcopy(observations[case_id])
    for turn in replay["turns"]:
        for result in turn.get("tool_results", []):
            result["status"] = terminal_status

    result = evaluate_case(cases[case_id], replay)

    expected_pass = terminal_status in {"completed", "deduplicated", "succeeded"}
    assert result["passed"] is expected_pass, result["failures"]


@pytest.mark.parametrize("terminal_status", [None, "", "pending", "running", True])
def test_tool_pairing_rejects_missing_or_nonterminal_result_status(
    terminal_status: Any,
) -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_results"][0]["status"] = terminal_status

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "terminal result status evidence" in " ".join(result["failures"])


def test_hitl_rejects_tool_identity_or_argument_substitution_after_approval() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"

    tool_changed = copy.deepcopy(observations[case_id])
    tool_changed["turns"][1]["tool_calls"][0]["name"] = "delete_everything"
    tool_result = evaluate_case(cases[case_id], tool_changed)

    arguments_changed = copy.deepcopy(observations[case_id])
    arguments_changed["turns"][1]["tool_calls"][0]["arguments"] = {
        "all_tenants": True
    }
    arguments_result = evaluate_case(cases[case_id], arguments_changed)

    assert tool_result["passed"] is False
    assert arguments_result["passed"] is False
    assert "approved tool identity or arguments changed" in " ".join(
        tool_result["failures"]
    )
    assert "approved tool identity or arguments changed" in " ".join(
        arguments_result["failures"]
    )


@pytest.mark.parametrize(
    "rejection_status",
    ["budget_rejected", "not_executed", "invalid_arguments", "denied", "deduplicated"],
)
def test_hitl_rejects_terminal_result_that_does_not_prove_dispatch(
    rejection_status: str,
) -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_results"][0]["status"] = rejection_status

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "requires one terminal paired result" in " ".join(result["failures"])


def test_hitl_rejects_extra_unapproved_checkpoint_bound_dispatch() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_calls"].append(
        {
            "call_id": "call-delete-all",
            "name": "delete_everything",
            "arguments": {"all_tenants": True},
            "dispatch_state": "dispatched",
            "approval_required": True,
            "checkpoint_id": "checkpoint-hitl-1",
        }
    )
    replay["turns"][1]["tool_results"].append(
        {"tool_call_id": "call-delete-all", "status": "completed"}
    )

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "unapproved or wrong-checkpoint protected call" in " ".join(
        result["failures"]
    )


def test_hitl_rejects_same_protected_tool_with_new_unapproved_call_id() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_calls"].append(
        {
            "call_id": "call-write-unapproved",
            "name": "approved_write",
            "arguments": {"document_id": "doc-other", "mode": "overwrite"},
            "dispatch_state": "dispatched",
        }
    )
    replay["turns"][1]["tool_results"].append(
        {"tool_call_id": "call-write-unapproved", "status": "completed"}
    )

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert "unapproved or wrong-checkpoint protected call" in " ".join(
        result["failures"]
    )


def test_hitl_uses_expected_policy_when_different_protected_tool_claims_safe() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_calls"].append(
        {
            "call_id": "call-delete-unapproved",
            "name": "delete_everything",
            "arguments": {"all_tenants": True},
            "dispatch_state": "dispatched",
            "approval_required": False,
        }
    )
    replay["turns"][1]["tool_results"].append(
        {"tool_call_id": "call-delete-unapproved", "status": "completed"}
    )

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    failures = " ".join(result["failures"])
    assert "conflicts with expected protected tool policy" in failures
    assert "unapproved or wrong-checkpoint protected call" in failures


def test_hitl_allows_unrelated_nonapproval_tool_around_pause() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][0]["tool_calls"].append(
        {
            "call_id": "call-safe-read",
            "name": "read_public_status",
            "arguments": {},
            "dispatch_state": "dispatched",
            "approval_required": False,
            "checkpoint_id": "checkpoint-hitl-1",
        }
    )
    replay["turns"][0]["tool_results"] = [
        {"tool_call_id": "call-safe-read", "status": "completed"}
    ]

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is True, result["failures"]


@pytest.mark.parametrize(
    ("field", "bad_value"),
    [
        ("dispatch_count", False),
        ("approved_call_ids", {"call-write-1": True}),
        ("approved_call_ids", [{"call_id": "call-write-1"}]),
        ("approved_call_ids", ["call-write-1", "call-write-1"]),
        ("approved_arguments_hashes", []),
        ("approved_arguments_hashes", {"call-write-1": "not-a-hash"}),
    ],
)
def test_hitl_malformed_resume_receipt_fails_closed_without_crashing(
    field: str,
    bad_value: Any,
) -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["hitl"][field] = bad_value

    result = evaluate_case(cases[case_id], replay)

    assert result["passed"] is False
    assert result["stateful_pass"] is False


@pytest.mark.parametrize("resume_count", [False, 1.0, 2])
def test_hitl_contract_requires_one_integer_resume(resume_count: Any) -> None:
    cases, _observations = _fixtures()
    case = copy.deepcopy(cases["assistant.stateful.hitl_pause_resume"])
    case["expected_trajectory"]["stateful"]["hitl"]["resume_count"] = resume_count

    validation = validate_cases([case])

    assert validation["valid"] is False
    assert "resume_count must equal 1" in " ".join(validation["errors"][0]["errors"])


def test_hitl_fixture_composes_with_generic_tool_pairing_and_tool_expectations() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.hitl_pause_resume"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["stateful"]["tool_pairing"] = {"required": True}
    case["expected_trajectory"]["tools"] = [
        {"name": "approved_write", "required": True, "status": "succeeded"}
    ]

    result = evaluate_case(case, observations[case_id])

    assert result["passed"] is True, result["failures"]


@pytest.mark.parametrize(
    "duplicate_statuses",
    [["failed", "completed"], ["completed", "failed"]],
)
def test_tool_status_expectation_rejects_duplicate_result_evidence_in_any_order(
    duplicate_statuses: list[str],
) -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["stateful"] = {"minimum_turns": 2}
    case["expected_trajectory"]["tools"] = [
        {"name": "search_kb", "required": True, "status": "succeeded"}
    ]
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][1]["tool_results"] = [
        {"tool_call_id": "call-search-1", "status": status}
        for status in duplicate_statuses
    ]

    result = evaluate_case(case, replay)

    assert result["passed"] is False
    assert "duplicate or conflicting status evidence" in " ".join(result["failures"])


def test_tool_status_expectation_rejects_call_result_terminal_conflict() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["stateful"] = {"minimum_turns": 2}
    case["expected_trajectory"]["tools"] = [
        {"name": "search_kb", "required": True, "status": "succeeded"}
    ]
    replay = copy.deepcopy(observations[case_id])
    replay["turns"][0]["tool_calls"][0]["status"] = "failed"

    result = evaluate_case(case, replay)

    assert result["passed"] is False
    assert "duplicate or conflicting status evidence" in " ".join(result["failures"])


def test_tool_status_uses_same_top_level_source_precedence_as_tool_calls() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.tool_pairing"
    case = copy.deepcopy(cases[case_id])
    case["expected_trajectory"]["tools"] = [
        {"name": "search_kb", "required": True, "status": "succeeded"}
    ]
    replay = copy.deepcopy(observations[case_id])
    replay["tool_calls"] = [
        {"call_id": "call-search-1", "name": "search_kb", "status": "completed"}
    ]
    replay["tool_results"] = [
        {"tool_call_id": "call-search-1", "status": "completed"}
    ]

    result = evaluate_case(case, replay)

    assert result["passed"] is True, result["failures"]


def test_compaction_requires_bound_event_then_post_compaction_snapshot() -> None:
    cases, observations = _fixtures()
    case_id = "assistant.stateful.compaction_retention"

    no_event = copy.deepcopy(observations[case_id])
    del no_event["turns"][1]["compaction"]
    no_event_result = evaluate_case(cases[case_id], no_event)

    wrong_lineage = copy.deepcopy(observations[case_id])
    wrong_lineage["turns"][1]["compaction"]["compaction_id"] = "compaction-other"
    wrong_lineage_result = evaluate_case(cases[case_id], wrong_lineage)

    assert no_event_result["passed"] is False
    assert "requires a compacted lineage event" in " ".join(
        no_event_result["failures"]
    )
    assert wrong_lineage_result["passed"] is False
    assert "compaction identity changed" in " ".join(wrong_lineage_result["failures"])
