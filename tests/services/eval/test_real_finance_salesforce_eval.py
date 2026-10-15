from __future__ import annotations

import hashlib
import json
from copy import deepcopy
from pathlib import Path

import pytest

from scripts.real_agent_scenario_runner import load_scenarios
from scripts.validate_real_finance_eval import (
    FIXTURE_DIR,
    FinanceEvalError,
    _load_json,
    evaluate_receipt,
    evaluate_run,
    validate_fixture,
)

GOLDEN = _load_json(FIXTURE_DIR / "golden.v1.json")


def _passing_run(run_id: str = "run-1") -> dict[str, object]:
    metrics = {
        metric["id"]: {
            "value": metric["expected"],
            "evidence_ids": [metric["evidence_ids"][0]],
        }
        for metric in GOLDEN["metrics"]
    }
    conclusions = {
        conclusion["id"]: conclusion["expected"] for conclusion in GOLDEN["required_conclusions"]
    }
    return {
        "run_id": run_id,
        "status": "completed",
        "metrics": metrics,
        "conclusions": conclusions,
        "subagents": [
            {
                "role": role,
                "batch_call_id": "batch-1",
                "dispatch_index": index,
                "status": "completed",
                "started_monotonic_ms": 1000 + index * 10,
                "finished_monotonic_ms": 1200 + index * 10,
                "evidence_ids": ["10Q.CF.7"],
                "input_artifact_sha256": (
                    "2ed163986166adf9283ac64e5d6875a51922a2a69eacb0ae29803548118e1899"
                ),
                "side_effects": [],
                "terminal_receipt_id": f"terminal-{index}",
            }
            for index, role in enumerate(
                [
                    "gaap_filing_analyst",
                    "non_gaap_reconciliation_analyst",
                    "skeptical_credit_reviewer",
                ]
            )
        ],
        "final_answer_sha256": hashlib.sha256(run_id.encode()).hexdigest(),
    }


def _write_receipt(tmp_path: Path, runs: list[dict[str, object]]) -> Path:
    path = tmp_path / "receipt.json"
    path.write_text(
        json.dumps(
            {
                "schema_version": "real-finance-eval-receipt/v1",
                "task_id": GOLDEN["task_id"],
                "runs": runs,
            }
        ),
        encoding="utf-8",
    )
    return path


def test_fixture_hashes_and_formulas_validate_offline() -> None:
    report = validate_fixture()
    assert report["fixture_valid"] is True
    assert report["metric_count"] == 25
    assert report["source_bytes"] == []


def test_live_scenario_is_three_run_parallel_and_keeps_goldens_host_side() -> None:
    scenario_path = FIXTURE_DIR / "scenario.v1.json"
    scenario = load_scenarios(scenario_path)["scenarios"][0]
    assert scenario["repetitions"] == 3
    assert scenario["require_parallel"] is True
    assert scenario["required_agent_ids"] == [
        "community-engineering-reviewers:technical-writer",
        "community-engineering-reviewers:system-architecture-reviewer",
        "community-doublecheck:doublecheck",
    ]
    assert len(scenario["expected_assertions"]) == 47
    for hidden_value in ("104.549763", "54.103343", "63.336766", "25188", "3374"):
        assert hidden_value not in scenario["prompt"]


def test_live_scenario_numeric_goldens_match_independent_fixture() -> None:
    scenario = load_scenarios(FIXTURE_DIR / "scenario.v1.json")["scenarios"][0]
    assertions = {
        assertion["path"].split("/")[2]: assertion
        for assertion in scenario["expected_assertions"]
        if assertion["kind"] == "json_number" and assertion["path"].startswith("/metrics/")
    }
    assert set(assertions) == {metric["id"] for metric in GOLDEN["metrics"]}
    for metric in GOLDEN["metrics"]:
        assertion = assertions[metric["id"]]
        assert assertion["expected"] == metric["expected"]
        assert assertion["absolute_tolerance"] == metric["tolerance_abs"]


def test_perfect_three_real_run_receipt_clears_deterministic_gate(tmp_path: Path) -> None:
    path = _write_receipt(tmp_path, [_passing_run(f"run-{index}") for index in range(3)])
    report = evaluate_receipt(path)
    assert report["passed"] is True
    assert report["score"] == 100.0


def test_one_weak_repeat_fails_minimum_aggregation(tmp_path: Path) -> None:
    runs = [_passing_run(f"run-{index}") for index in range(3)]
    runs[1]["metrics"]["q1_fcf_usd_m"]["value"] = 25188  # type: ignore[index]
    report = evaluate_receipt(_write_receipt(tmp_path, runs))
    assert report["passed"] is False
    assert report["score"] <= 75


def test_accepting_misleading_annualization_caps_score_at_55() -> None:
    run = _passing_run()
    run["conclusions"]["misleading_statement_verdict"] = "accept"  # type: ignore[index]
    report = evaluate_run(run, GOLDEN)
    assert report["score"] == 55
    assert "required conclusion failed: misleading_statement_verdict" in report["failures"]


def test_non_gaap_misclassification_caps_score_at_60() -> None:
    run = _passing_run()
    run["conclusions"]["non_gaap_classification"] = "gaap"  # type: ignore[index]
    assert evaluate_run(run, GOLDEN)["score"] == 60


def test_sequential_children_cannot_claim_parallel_dispatch() -> None:
    run = _passing_run()
    for index, child in enumerate(run["subagents"]):  # type: ignore[union-attr]
        child["started_monotonic_ms"] = 1000 + index * 200
        child["finished_monotonic_ms"] = 1100 + index * 200
    report = evaluate_run(run, GOLDEN)
    assert report["score"] == 85
    assert "three specialist intervals did not overlap concurrently" in report["failures"]


def test_child_not_bound_to_fixed_packet_cannot_clear_delegation_gate() -> None:
    run = _passing_run()
    run["subagents"][0]["input_artifact_sha256"] = "0" * 64  # type: ignore[index]
    report = evaluate_run(run, GOLDEN)
    assert report["score"] == 85
    assert "child gaap_filing_analyst is not bound to the fixed packet" in report["failures"]


def test_unapproved_source_caps_score_at_70() -> None:
    run = _passing_run()
    run["metrics"]["q1_fcf_usd_m"]["evidence_ids"].append("BLOG.SECONDARY")  # type: ignore[index]
    assert evaluate_run(run, GOLDEN)["score"] == 70


def test_wrong_unit_scale_on_critical_metric_caps_score_at_75() -> None:
    run = _passing_run()
    run["metrics"]["q1_net_cash_including_marketable_usd_m"]["value"] = 8.973  # type: ignore[index]
    assert evaluate_run(run, GOLDEN)["score"] <= 75


def test_requires_exactly_three_unique_runs(tmp_path: Path) -> None:
    path = _write_receipt(
        tmp_path, [_passing_run("duplicate"), _passing_run("duplicate"), _passing_run("third")]
    )
    with pytest.raises(FinanceEvalError, match="unique"):
        evaluate_receipt(path)


def test_duplicate_json_keys_fail_closed(tmp_path: Path) -> None:
    path = tmp_path / "duplicate.json"
    path.write_text('{"a": 1, "a": 2}', encoding="utf-8")
    with pytest.raises(FinanceEvalError, match="duplicate JSON key"):
        _load_json(path)


def test_missing_final_answer_hash_cannot_reach_92() -> None:
    run = deepcopy(_passing_run())
    run["final_answer_sha256"] = "not-a-hash"
    report = evaluate_run(run, GOLDEN)
    assert report["passed"] is False
    assert report["score"] == 90
