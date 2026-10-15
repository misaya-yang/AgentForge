#!/usr/bin/env python3
"""Validate the fixed Salesforce finance fixture and real three-run receipts.

This module is deliberately independent of the general-agent evaluator. It uses
only the Python standard library and never reads model-provider credentials.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "src" / "services" / "eval" / "fixtures" / "real_finance_salesforce_fy26_q1"
GOLDEN_PATH = FIXTURE_DIR / "golden.v1.json"
SOURCES_PATH = FIXTURE_DIR / "sources.v1.json"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REQUIRED_ROLES = {
    "gaap_filing_analyst",
    "non_gaap_reconciliation_analyst",
    "skeptical_credit_reviewer",
}


class FinanceEvalError(ValueError):
    """Raised when fixture or receipt integrity fails closed."""


def _load_json(path: Path) -> dict[str, Any]:
    def reject_duplicate(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FinanceEvalError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        payload = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicate)
    except (OSError, json.JSONDecodeError) as exc:
        raise FinanceEvalError(f"cannot load {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise FinanceEvalError(f"{path} must contain one JSON object")
    return payload


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _golden_calculations(inputs: dict[str, Any]) -> dict[str, float]:
    q1 = inputs["q1"]
    fy = inputs["fy25"]
    q1_fcf = q1["cfo_2025"] - q1["capex_2025"]
    prior_q1_fcf = q1["cfo_2024"] - q1["capex_2024"]
    gaap_margin = q1["gaap_operating_income_2025"] / q1["revenue_2025"]
    prior_gaap_margin = q1["gaap_operating_income_2024"] / q1["revenue_2024"]
    non_gaap_margin = q1["non_gaap_operating_income_2025"] / q1["revenue_2025"]
    prior_non_gaap_margin = q1["non_gaap_operating_income_2024"] / q1["revenue_2024"]
    wc_benefit = sum(q1["working_capital_changes_2025"])
    q1_addbacks = sum(q1["non_gaap_addbacks_2025"])
    fy25_fcf = fy["cfo_2025"] - fy["capex_2025"]
    fy24_fcf = fy["cfo_2024"] - fy["capex_2024"]
    q1_quick = (
        q1["cash_2025"] + q1["marketable_securities_2025"] + q1["accounts_receivable_2025"]
    ) / q1["current_liabilities_2025"]
    fy25_quick = (
        fy["cash_2025"] + fy["marketable_securities_2025"] + fy["accounts_receivable_2025"]
    ) / fy["current_liabilities_2025"]
    q1_net_cash = q1["cash_2025"] + q1["marketable_securities_2025"] - q1["debt_2025"]
    fy25_net_cash = fy["cash_2025"] + fy["marketable_securities_2025"] - fy["debt_2025"]
    return {
        "q1_revenue_growth_pct": (q1["revenue_2025"] / q1["revenue_2024"] - 1) * 100,
        "q1_gaap_operating_margin_pct": gaap_margin * 100,
        "q1_prior_gaap_operating_margin_pct": prior_gaap_margin * 100,
        "q1_gaap_margin_expansion_bps": (gaap_margin - prior_gaap_margin) * 10000,
        "q1_non_gaap_operating_margin_pct": non_gaap_margin * 100,
        "q1_non_gaap_margin_expansion_bps": (non_gaap_margin - prior_non_gaap_margin) * 10000,
        "q1_non_gaap_addbacks_usd_m": q1_addbacks,
        "q1_addbacks_pct_gaap_operating_income": q1_addbacks
        / q1["gaap_operating_income_2025"]
        * 100,
        "q1_fcf_usd_m": q1_fcf,
        "q1_fcf_growth_pct": (q1_fcf / prior_q1_fcf - 1) * 100,
        "q1_cfo_to_net_income_x": q1["cfo_2025"] / q1["net_income_2025"],
        "q1_disclosed_working_capital_cash_benefit_usd_m": wc_benefit,
        "q1_working_capital_share_of_cfo_net_income_gap_pct": wc_benefit
        / (q1["cfo_2025"] - q1["net_income_2025"])
        * 100,
        "q1_current_ratio_x": q1["current_assets_2025"] / q1["current_liabilities_2025"],
        "q1_quick_ratio_x": q1_quick,
        "q1_net_cash_including_marketable_usd_m": q1_net_cash,
        "q1_cash_only_minus_debt_usd_m": q1["cash_2025"] - q1["debt_2025"],
        "q1_interest_coverage_x": q1["gaap_operating_income_2025"] / q1["interest_expense_2025"],
        "fy25_fcf_usd_m": fy25_fcf,
        "fy25_fcf_growth_pct": (fy25_fcf / fy24_fcf - 1) * 100,
        "fy25_cfo_to_net_income_x": fy["cfo_2025"] / fy["net_income_2025"],
        "fy24_cfo_to_net_income_x": fy["cfo_2024"] / fy["net_income_2024"],
        "mechanical_q1_annualized_fcf_usd_m": q1_fcf * 4,
        "q1_net_cash_increase_from_fy25_usd_m": q1_net_cash - fy25_net_cash,
        "q1_quick_ratio_change_from_fy25_x": q1_quick - fy25_quick,
    }


def validate_fixture(*, verify_source_bytes: bool = False) -> dict[str, Any]:
    golden = _load_json(GOLDEN_PATH)
    sources = _load_json(SOURCES_PATH)
    packet = (FIXTURE_DIR / sources["source_packet"]["path"]).read_bytes()
    packet_hash = _sha256_bytes(packet)
    if packet_hash != sources["source_packet"]["sha256"]:
        raise FinanceEvalError("source packet SHA-256 mismatch")

    calculated = _golden_calculations(golden["inputs"])
    metrics = golden.get("metrics")
    if not isinstance(metrics, list) or not metrics:
        raise FinanceEvalError("golden metrics must be a non-empty array")
    if sum(int(metric["points"]) for metric in metrics) != 50:
        raise FinanceEvalError("golden metric points must sum to 50")
    conclusion_points = sum(int(item["points"]) for item in golden["required_conclusions"])
    if conclusion_points != 25:
        raise FinanceEvalError("golden conclusion points must sum to 25")
    for metric in metrics:
        metric_id = metric["id"]
        if metric_id not in calculated:
            raise FinanceEvalError(f"no independent formula for {metric_id}")
        if not math.isclose(
            float(metric["expected"]),
            float(calculated[metric_id]),
            rel_tol=0,
            abs_tol=max(float(metric["tolerance_abs"]) / 10, 1e-9),
        ):
            raise FinanceEvalError(f"golden value does not match formula: {metric_id}")

    source_results: list[dict[str, Any]] = []
    if verify_source_bytes:
        for source in sources["documents"]:
            request = urllib.request.Request(
                source["url"],
                headers={"User-Agent": "AI-Platform finance eval research eval@example.invalid"},
            )
            try:
                with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310
                    data = response.read()
            except OSError as exc:
                raise FinanceEvalError(
                    f"cannot fetch pinned source {source['source_id']}: {exc}"
                ) from exc
            actual_hash = _sha256_bytes(data)
            if actual_hash != source["sha256"] or len(data) != int(source["bytes"]):
                raise FinanceEvalError(f"pinned source bytes changed: {source['source_id']}")
            source_results.append({"source_id": source["source_id"], "verified": True})

    return {
        "fixture_valid": True,
        "task_id": golden["task_id"],
        "metric_count": len(metrics),
        "source_packet_sha256": packet_hash,
        "source_bytes": source_results,
    }


def _metric_receipt(entry: Any) -> tuple[float | None, set[str]]:
    if not isinstance(entry, dict):
        return None, set()
    value = entry.get("value")
    evidence = entry.get("evidence_ids")
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
    ):
        return None, set()
    if not isinstance(evidence, list) or any(not isinstance(item, str) for item in evidence):
        return float(value), set()
    return float(value), set(evidence)


def _delegation_score(
    subagents: Any,
    allowed_evidence: set[str],
    source_packet_sha256: str,
) -> tuple[float, list[str]]:
    failures: list[str] = []
    if not isinstance(subagents, list) or len(subagents) != 3:
        return 0.0, ["delegation must contain exactly three child receipts"]
    roles = {child.get("role") for child in subagents if isinstance(child, dict)}
    if roles != REQUIRED_ROLES:
        failures.append("delegation roles do not match the required independent specialists")
    batch_ids = {child.get("batch_call_id") for child in subagents if isinstance(child, dict)}
    if len(batch_ids) != 1 or None in batch_ids or "" in batch_ids:
        failures.append("children must share one non-empty batch_call_id")
    dispatch_indexes = {
        child.get("dispatch_index") for child in subagents if isinstance(child, dict)
    }
    if dispatch_indexes != {0, 1, 2}:
        failures.append("dispatch indexes must be exactly 0, 1, and 2")
    terminal_receipt_ids = {
        child.get("terminal_receipt_id") for child in subagents if isinstance(child, dict)
    }
    if len(terminal_receipt_ids) != 3 or None in terminal_receipt_ids or "" in terminal_receipt_ids:
        failures.append("children must have three unique terminal receipt IDs")

    intervals: list[tuple[float, float]] = []
    for child in subagents:
        if not isinstance(child, dict) or child.get("status") != "completed":
            failures.append("every delegated child must have one completed terminal receipt")
            continue
        started = child.get("started_monotonic_ms")
        finished = child.get("finished_monotonic_ms")
        if (
            isinstance(started, bool)
            or isinstance(finished, bool)
            or not isinstance(started, (int, float))
            or not isinstance(finished, (int, float))
            or float(finished) <= float(started)
        ):
            failures.append(f"invalid child timing receipt for role {child.get('role')}")
            continue
        intervals.append((float(started), float(finished)))
        evidence = child.get("evidence_ids")
        if not isinstance(evidence, list) or not evidence:
            failures.append(f"child {child.get('role')} has no evidence receipt")
        elif any(item not in allowed_evidence for item in evidence):
            failures.append(f"child {child.get('role')} used unapproved evidence")
        if child.get("input_artifact_sha256") != source_packet_sha256:
            failures.append(f"child {child.get('role')} is not bound to the fixed packet")
        if child.get("side_effects") != []:
            failures.append(f"child {child.get('role')} did not remain read-only")

    if len(intervals) == 3:
        latest_start = max(start for start, _ in intervals)
        earliest_finish = min(finish for _, finish in intervals)
        if latest_start >= earliest_finish:
            failures.append("three specialist intervals did not overlap concurrently")
    return (10.0 if not failures else 0.0), failures


def evaluate_run(run: Any, golden: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(run, dict):
        return {"score": 0.0, "passed": False, "failures": ["run is not an object"]}
    failures: list[str] = []
    cap = 100.0
    metric_score = 0.0
    citation_score = 10.0
    allowed_evidence = set(golden["allowed_evidence_ids"])
    metrics = run.get("metrics")
    if not isinstance(metrics, dict):
        metrics = {}
        failures.append("metrics must be an object")

    for metric in golden["metrics"]:
        value, evidence = _metric_receipt(metrics.get(metric["id"]))
        correct = value is not None and math.isclose(
            value,
            float(metric["expected"]),
            rel_tol=0,
            abs_tol=float(metric["tolerance_abs"]),
        )
        grounded = bool(evidence) and bool(evidence.intersection(metric["evidence_ids"]))
        if correct:
            metric_score += float(metric["points"])
        else:
            failures.append(f"metric incorrect or missing: {metric['id']}")
            if metric["critical"]:
                cap = min(cap, 75.0)
        if not grounded:
            citation_score -= 10.0 / len(golden["metrics"])
            failures.append(f"metric lacks required evidence: {metric['id']}")
        if evidence - allowed_evidence:
            cap = min(cap, 70.0)
            failures.append(f"metric uses unapproved evidence: {metric['id']}")

    conclusion_score = 0.0
    conclusions = run.get("conclusions")
    if not isinstance(conclusions, dict):
        conclusions = {}
    for requirement in golden["required_conclusions"]:
        if conclusions.get(requirement["id"]) == requirement["expected"]:
            conclusion_score += float(requirement["points"])
        else:
            cap = min(cap, float(requirement["hard_cap_on_failure"]))
            failures.append(f"required conclusion failed: {requirement['id']}")

    sources = _load_json(SOURCES_PATH)
    delegation_score, delegation_failures = _delegation_score(
        run.get("subagents"),
        allowed_evidence,
        sources["source_packet"]["sha256"],
    )
    failures.extend(delegation_failures)
    if delegation_failures:
        cap = min(cap, 85.0)

    trace_score = 5.0
    if run.get("status") != "completed":
        trace_score = 0.0
        cap = 0.0
        failures.append("run status is not completed")
    answer_hash = run.get("final_answer_sha256")
    if not isinstance(answer_hash, str) or not SHA256_RE.fullmatch(answer_hash):
        trace_score = 0.0
        cap = min(cap, 90.0)
        failures.append("missing valid final-answer SHA-256 receipt")

    raw_score = (
        metric_score + conclusion_score + max(citation_score, 0.0) + delegation_score + trace_score
    )
    score = min(raw_score, cap)
    return {
        "run_id": run.get("run_id"),
        "raw_score": raw_score,
        "hard_cap": cap,
        "score": score,
        "passed": score >= float(golden["minimum_score"]),
        "component_scores": {
            "metrics": metric_score,
            "conclusions": conclusion_score,
            "citations": max(citation_score, 0.0),
            "parallel_delegation": delegation_score,
            "trace_integrity": trace_score,
        },
        "failures": failures,
    }


def evaluate_receipt(path: Path) -> dict[str, Any]:
    validate_fixture()
    golden = _load_json(GOLDEN_PATH)
    receipt = _load_json(path)
    if receipt.get("schema_version") != "real-finance-eval-receipt/v1":
        raise FinanceEvalError("unsupported receipt schema_version")
    if receipt.get("task_id") != golden["task_id"]:
        raise FinanceEvalError("receipt task_id mismatch")
    runs = receipt.get("runs")
    if not isinstance(runs, list) or len(runs) != int(golden["repetitions"]):
        raise FinanceEvalError("critical finance case requires exactly three real runs")
    run_ids = [run.get("run_id") for run in runs if isinstance(run, dict)]
    if len(set(run_ids)) != 3 or any(
        not isinstance(run_id, str) or not run_id for run_id in run_ids
    ):
        raise FinanceEvalError("run_id values must be three unique non-empty strings")
    reports = [evaluate_run(run, golden) for run in runs]
    score = min(float(report["score"]) for report in reports)
    return {
        "schema_version": "real-finance-evaluation-report/v1",
        "task_id": golden["task_id"],
        "aggregation": "minimum_run_score",
        "score": score,
        "passed": all(report["passed"] for report in reports) and score >= golden["minimum_score"],
        "minimum_score": golden["minimum_score"],
        "runs": reports,
        "note": "External LLM judge score must be combined as min(deterministic, judge) per run.",
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", nargs="?", help="real three-run receipt JSON")
    parser.add_argument("--output", help="write deterministic report JSON")
    parser.add_argument(
        "--fixture-only", action="store_true", help="validate only the fixed fixture"
    )
    parser.add_argument(
        "--verify-source-bytes",
        action="store_true",
        help="network-fetch and SHA-256 verify the pinned SEC documents",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.fixture_only:
            report = validate_fixture(verify_source_bytes=args.verify_source_bytes)
        else:
            if not args.receipt:
                raise FinanceEvalError("receipt is required unless --fixture-only is used")
            report = evaluate_receipt(Path(args.receipt))
        rendered = json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        if args.output:
            Path(args.output).write_text(rendered, encoding="utf-8")
        print(rendered, end="")
        return 0 if report.get("passed", report.get("fixture_valid", False)) else 1
    except Exception as exc:  # noqa: BLE001 - gate must fail closed
        print(
            f"real-finance evaluation failed closed: {type(exc).__name__}: {exc}", file=sys.stderr
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
