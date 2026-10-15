from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

DEFAULT_GATE_THRESHOLDS = {
    "overall_score": 0.85,
    "trajectory_pass_rate": 0.95,
    "critical_pass_rate": 1.0,
    "stateful_pass_rate": 1.0,
    "baseline_tolerance": 0.02,
}
GATE_METRICS_SCHEMA_VERSION = "eval-gate-metrics/v2"
GATE_RATE_ABS_TOLERANCE = 0.00005

__all__ = [
    "DEFAULT_GATE_THRESHOLDS",
    "GATE_METRICS_SCHEMA_VERSION",
    "GATE_RATE_ABS_TOLERANCE",
    "apply_gate",
    "write_gate_report",
]


def apply_gate(
    metrics: dict[str, Any],
    *,
    thresholds: dict[str, float] | None = None,
    baseline_metrics: dict[str, Any] | None = None,
    require_critical_coverage: bool = False,
    require_stateful_coverage: bool = False,
) -> dict[str, Any]:
    failures: list[str] = []
    skipped_thresholds: list[str] = []
    normalized_metrics = dict(metrics) if isinstance(metrics, dict) else {}

    def finite_rate(value: Any, label: str) -> float | None:
        if (
            isinstance(value, bool)
            or not isinstance(value, int | float)
            or not math.isfinite(float(value))
            or not 0.0 <= float(value) <= 1.0
        ):
            failures.append(f"{label} must be a finite number in [0, 1]")
            return None
        return float(value)

    def nonnegative_count(value: Any, label: str) -> int | None:
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            failures.append(f"{label} must be a non-negative integer")
            return None
        return value

    gate_thresholds = dict(DEFAULT_GATE_THRESHOLDS)
    if thresholds is not None and not isinstance(thresholds, dict):
        failures.append("thresholds must be an object")
    elif isinstance(thresholds, dict):
        unknown_thresholds = sorted(set(thresholds) - set(DEFAULT_GATE_THRESHOLDS))
        if unknown_thresholds:
            failures.append(
                "unsupported gate thresholds: " + ", ".join(unknown_thresholds)
            )
        for name in DEFAULT_GATE_THRESHOLDS:
            if name not in thresholds:
                continue
            value = finite_rate(thresholds[name], f"threshold {name}")
            if value is not None:
                gate_thresholds[name] = value

    if not isinstance(metrics, dict):
        failures.append("metrics must be an object")
        metrics = {}
    if metrics.get("schema_version") != GATE_METRICS_SCHEMA_VERSION:
        failures.append(f"metrics require schema_version {GATE_METRICS_SCHEMA_VERSION}")

    overall_score_value = metrics.get("overall_score")
    if overall_score_value is None:
        overall_score_value = metrics.get("average_score")
    overall_score = finite_rate(overall_score_value, "overall_score")
    trajectory_pass_rate = finite_rate(
        metrics.get("trajectory_pass_rate"), "trajectory_pass_rate"
    )
    pass_rate = finite_rate(metrics.get("pass_rate"), "pass_rate")
    case_count = nonnegative_count(metrics.get("case_count"), "case_count")
    score_sum_value = metrics.get("score_sum")
    score_sum = None
    if (
        isinstance(score_sum_value, bool)
        or not isinstance(score_sum_value, int | float)
        or not math.isfinite(float(score_sum_value))
        or float(score_sum_value) < 0.0
    ):
        failures.append("score_sum must be a finite non-negative number")
    else:
        score_sum = float(score_sum_value)
    failed_case_count = nonnegative_count(
        metrics.get("failed_case_count"), "failed_case_count"
    )
    if (
        case_count is not None
        and failed_case_count is not None
        and failed_case_count > case_count
    ):
        failures.append("failed_case_count exceeds case_count")
    elif case_count is not None and failed_case_count is not None and pass_rate is not None:
        expected_pass_rate = (
            (case_count - failed_case_count) / case_count if case_count else 0.0
        )
        if not math.isclose(
            pass_rate,
            expected_pass_rate,
            rel_tol=0.0,
            abs_tol=GATE_RATE_ABS_TOLERANCE,
        ):
            failures.append("pass_rate is inconsistent with case counts")
        normalized_metrics["pass_rate"] = expected_pass_rate
        pass_rate = expected_pass_rate
    if case_count is not None and score_sum is not None:
        if score_sum > case_count:
            failures.append("score_sum exceeds case_count")
        expected_overall_score = score_sum / case_count if case_count else 0.0
        if overall_score is not None and not math.isclose(
            overall_score,
            expected_overall_score,
            rel_tol=0.0,
            abs_tol=GATE_RATE_ABS_TOLERANCE,
        ):
            failures.append("overall_score is inconsistent with score_sum and case_count")
        normalized_metrics["overall_score"] = expected_overall_score
        normalized_metrics["average_score"] = expected_overall_score
        overall_score = expected_overall_score

    trajectory_case_count = nonnegative_count(
        metrics.get("trajectory_case_count"), "trajectory_case_count"
    )
    trajectory_failed_count = nonnegative_count(
        metrics.get("trajectory_failed_count"), "trajectory_failed_count"
    )
    if (
        trajectory_case_count is not None
        and trajectory_failed_count is not None
        and trajectory_failed_count > trajectory_case_count
    ):
        failures.append("trajectory_failed_count exceeds trajectory_case_count")
    elif (
        trajectory_case_count is not None
        and trajectory_failed_count is not None
        and trajectory_pass_rate is not None
    ):
        expected_trajectory_rate = (
            (trajectory_case_count - trajectory_failed_count) / trajectory_case_count
            if trajectory_case_count
            else 0.0
        )
        if not math.isclose(
            trajectory_pass_rate,
            expected_trajectory_rate,
            rel_tol=0.0,
            abs_tol=GATE_RATE_ABS_TOLERANCE,
        ):
            failures.append(
                "trajectory_pass_rate is inconsistent with trajectory case counts"
            )
        normalized_metrics["trajectory_pass_rate"] = expected_trajectory_rate
        trajectory_pass_rate = expected_trajectory_rate
    if (
        case_count is not None
        and trajectory_case_count is not None
        and trajectory_case_count != case_count
    ):
        failures.append("trajectory coverage must equal case_count")
    if (
        failed_case_count is not None
        and trajectory_failed_count is not None
        and trajectory_failed_count > failed_case_count
    ):
        failures.append("trajectory_failed_count exceeds failed_case_count")

    critical_count = nonnegative_count(
        metrics.get("critical_case_count"), "critical_case_count"
    )
    critical_pass_rate_value = metrics.get("critical_pass_rate")
    if "critical_pass_rate" not in metrics:
        critical_pass_rate_value = metrics.get("pass_rate")
    if critical_count == 0:
        critical_pass_rate = None
        if critical_pass_rate_value is not None:
            failures.append(
                "critical_pass_rate must be null when critical_case_count is zero"
            )
    else:
        critical_pass_rate = finite_rate(
            critical_pass_rate_value, "critical_pass_rate"
        )

    critical_failed_count = nonnegative_count(
        metrics.get("critical_failed_count"), "critical_failed_count"
    )
    if (
        case_count is not None
        and critical_count is not None
        and critical_count > case_count
    ):
        failures.append("critical_case_count exceeds case_count")
    if (
        critical_count is not None
        and critical_failed_count is not None
        and critical_failed_count > critical_count
    ):
        failures.append("critical_failed_count exceeds critical_case_count")
    elif (
        critical_count
        and critical_failed_count is not None
        and critical_pass_rate is not None
    ):
        expected_rate = (critical_count - critical_failed_count) / critical_count
        if not math.isclose(
            critical_pass_rate,
            expected_rate,
            rel_tol=0.0,
            abs_tol=GATE_RATE_ABS_TOLERANCE,
        ):
            failures.append(
                "critical_pass_rate is inconsistent with critical case counts"
            )
        normalized_metrics["critical_pass_rate"] = expected_rate
        critical_pass_rate = expected_rate
    if (
        failed_case_count is not None
        and critical_failed_count is not None
        and critical_failed_count > failed_case_count
    ):
        failures.append("critical_failed_count exceeds failed_case_count")

    stateful_count_present = "stateful_case_count" in metrics
    stateful_case_count = (
        nonnegative_count(metrics.get("stateful_case_count"), "stateful_case_count")
        if stateful_count_present
        else None
    )
    stateful_pass_rate_value = metrics.get("stateful_pass_rate")
    if stateful_case_count == 0:
        stateful_pass_rate = None
        if stateful_pass_rate_value is not None:
            failures.append(
                "stateful_pass_rate must be null when stateful_case_count is zero"
            )
    elif stateful_case_count is not None:
        stateful_pass_rate = finite_rate(
            stateful_pass_rate_value, "stateful_pass_rate"
        )
    else:
        stateful_pass_rate = None
        if "stateful_pass_rate" in metrics:
            failures.append("stateful_pass_rate requires stateful_case_count")

    stateful_failed_count = None
    if stateful_count_present:
        stateful_failed_count = nonnegative_count(
            metrics.get("stateful_failed_count"), "stateful_failed_count"
        )
        if (
            stateful_case_count is not None
            and stateful_failed_count is not None
            and stateful_failed_count > stateful_case_count
        ):
            failures.append("stateful_failed_count exceeds stateful_case_count")
        elif (
            stateful_case_count
            and stateful_failed_count is not None
            and stateful_pass_rate is not None
        ):
            expected_rate = (stateful_case_count - stateful_failed_count) / stateful_case_count
            if not math.isclose(
                stateful_pass_rate,
                expected_rate,
                rel_tol=0.0,
                abs_tol=GATE_RATE_ABS_TOLERANCE,
            ):
                failures.append(
                    "stateful_pass_rate is inconsistent with stateful case counts"
                )
            normalized_metrics["stateful_pass_rate"] = expected_rate
            stateful_pass_rate = expected_rate
    if (
        case_count is not None
        and stateful_case_count is not None
        and stateful_case_count > case_count
    ):
        failures.append("stateful_case_count exceeds case_count")
    if (
        failed_case_count is not None
        and stateful_failed_count is not None
        and stateful_failed_count > failed_case_count
    ):
        failures.append("stateful_failed_count exceeds failed_case_count")

    if overall_score is not None and overall_score < gate_thresholds["overall_score"]:
        failures.append(
            f"overall_score {overall_score:.4f} < {gate_thresholds['overall_score']:.4f}"
        )
    if (
        trajectory_pass_rate is not None
        and trajectory_pass_rate < gate_thresholds["trajectory_pass_rate"]
    ):
        failures.append(
            f"trajectory_pass_rate {trajectory_pass_rate:.4f} < {gate_thresholds['trajectory_pass_rate']:.4f}"
        )
    if critical_count == 0:
        skipped_thresholds.append("critical_pass_rate:no_critical_cases")
    elif gate_thresholds["critical_pass_rate"] == 1.0 and critical_failed_count is not None:
        if critical_failed_count != 0:
            failures.append("critical hard gate requires zero failed critical cases")
    elif (
        critical_pass_rate is not None
        and critical_pass_rate < gate_thresholds["critical_pass_rate"]
    ):
        failures.append(
            f"critical_pass_rate {critical_pass_rate:.4f} < {gate_thresholds['critical_pass_rate']:.4f}"
        )
    if stateful_count_present:
        if stateful_case_count == 0:
            skipped_thresholds.append("stateful_pass_rate:no_stateful_cases")
        elif (
            gate_thresholds["stateful_pass_rate"] == 1.0
            and stateful_failed_count is not None
        ):
            if stateful_failed_count != 0:
                failures.append("stateful hard gate requires zero failed stateful cases")
        elif (
            stateful_pass_rate is not None
            and stateful_pass_rate < gate_thresholds["stateful_pass_rate"]
        ):
            failures.append(
                f"stateful_pass_rate {stateful_pass_rate:.4f} < "
                f"{gate_thresholds['stateful_pass_rate']:.4f}"
            )
    if require_critical_coverage and (critical_count is None or critical_count == 0):
        failures.append("release gate requires non-zero critical case coverage")
    if require_stateful_coverage and (
        not stateful_count_present
        or stateful_case_count is None
        or stateful_case_count == 0
    ):
        failures.append("release gate requires non-zero stateful case coverage")
    if baseline_metrics is not None:
        if not isinstance(baseline_metrics, dict):
            failures.append("baseline_metrics must be an object")
            baseline_metrics = {}
        baseline_receipt_gate = apply_gate(
            baseline_metrics,
            thresholds=dict.fromkeys(DEFAULT_GATE_THRESHOLDS, 0.0),
        )
        failures.extend(
            f"baseline {failure}" for failure in baseline_receipt_gate["failures"]
        )
        baseline_score = (
            baseline_receipt_gate["metrics"].get("overall_score")
            if not baseline_receipt_gate["failures"]
            else None
        )
        allowed = (
            float(baseline_score) - gate_thresholds["baseline_tolerance"]
            if isinstance(baseline_score, int | float)
            else None
        )
        if overall_score is not None and allowed is not None and overall_score < allowed:
            failures.append(
                f"candidate score {overall_score:.4f} < baseline tolerance {allowed:.4f}"
            )

    return {
        "status": "fail" if failures else "pass",
        "thresholds": gate_thresholds,
        "metrics": normalized_metrics,
        "failures": failures,
        "skipped_thresholds": skipped_thresholds,
        "coverage": {
            "case_count": case_count,
            "trajectory_case_count": trajectory_case_count,
            "critical_case_count": critical_count,
            "stateful_case_count": stateful_case_count,
            "critical_required": require_critical_coverage,
            "stateful_required": require_stateful_coverage,
        },
    }


def write_gate_report(
    result: dict[str, Any], json_path: str | Path, markdown_path: str | Path
) -> None:
    json_target = Path(json_path)
    md_target = Path(markdown_path)
    json_target.parent.mkdir(parents=True, exist_ok=True)
    md_target.parent.mkdir(parents=True, exist_ok=True)
    json_target.write_text(json.dumps(result, indent=2, sort_keys=True), encoding="utf-8")
    failures = result.get("gate", {}).get("failures") or []
    lines = [
        "# Eval Regression Gate",
        "",
        f"Status: `{result.get('gate', {}).get('status', 'unknown')}`",
        f"Evidence scope: `{result.get('evidence_scope', 'unknown')}`",
        f"Joined observations: `{result.get('observations', {}).get('joined_count', 0)}`",
        f"Cases: `{result.get('summary', {}).get('case_count', 0)}`",
        f"Overall score: `{result.get('metrics', {}).get('overall_score', 0)}`",
        f"Trajectory pass rate: `{result.get('metrics', {}).get('trajectory_pass_rate', 0)}`",
        f"Critical pass rate: `{result.get('metrics', {}).get('critical_pass_rate', 0)}`",
        "",
        "## Failures",
        "",
    ]
    lines.extend([f"- {failure}" for failure in failures] or ["- None"])
    md_target.write_text("\n".join(lines) + "\n", encoding="utf-8")
