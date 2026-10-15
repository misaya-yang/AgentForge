"""Deterministic two-track RAG regression evaluation for CI.

Retrieval metrics are computed locally from labelled ranked lists. Answer
quality remains a separate, explicitly enabled recorded-judge gate so CI never
reports faithfulness or response relevance when no answer/judge evidence ran.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
from collections.abc import Sequence
from dataclasses import dataclass
from dataclasses import field as dataclass_field
from pathlib import Path
from typing import Any

from ai_gateway_contracts.retrieval_metrics import (
    QueryRetrievalJudgement,
    evaluate_retrieval,
)

from .golden import load_jsonl, load_observations

RAG_TRACKS = ("retrieval_only", "answer_aware")
ANSWER_METRICS = ("faithfulness", "response_relevancy")
ANSWER_METRIC_ALIASES = {"answer_relevancy": "response_relevancy"}
DEFAULT_RETRIEVAL_THRESHOLDS = {
    "recall_at_k": 0.8,
    "mrr": 0.8,
    "ndcg_at_k": 0.8,
}
DEFAULT_ANSWER_THRESHOLDS = {
    "faithfulness": 0.7,
    "response_relevancy": 0.7,
}
JUDGE_COHORT_FIELDS = (
    "provider",
    "model",
    "judge_run_id",
    "rubric_version",
    "judge_prompt_sha256",
)
JUDGE_ARTIFACT_HASH_DOMAIN = "rag-recorded-judge-rows/v1"
JUDGE_POLICY_SCHEMA_VERSION = "rag-judge-policy/v1"
JUDGE_POLICY_PIN_ENV = "RAG_JUDGE_POLICY_SHA256"
_RESOLVED_POLICY_TOKEN = object()


@dataclass(frozen=True)
class ResolvedJudgePolicy:
    """Judge policy whose manifest bytes were verified by a server-owned pin."""

    policy_id: str
    revision: str
    artifact_sha256: str
    cohort_fields: tuple[tuple[str, str], ...]
    manifest_sha256: str
    source: str
    resolver: str
    _authority_token: object = dataclass_field(repr=False, compare=False)

    def __post_init__(self) -> None:
        if self._authority_token is not _RESOLVED_POLICY_TOKEN:
            raise ValueError("judge policy must be constructed by the trusted resolver")

    @property
    def cohort(self) -> dict[str, str]:
        return dict(self.cohort_fields)


def _known_score(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    score = float(value)
    return score if math.isfinite(score) and 0.0 <= score <= 1.0 else None


def _validated_thresholds(
    supplied: dict[str, float] | None,
    *,
    defaults: dict[str, float],
    label: str,
) -> dict[str, float]:
    overrides = supplied or {}
    unknown = sorted(set(overrides) - set(defaults))
    if unknown:
        raise ValueError(f"unsupported {label} thresholds: {', '.join(unknown)}")
    thresholds = {**defaults, **overrides}
    for metric, value in thresholds.items():
        if _known_score(value) is None:
            raise ValueError(f"{label} threshold {metric} must be finite and in [0, 1]")
    return {metric: float(value) for metric, value in thresholds.items()}


def _percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def bootstrap_mean_ci(
    values: list[float],
    *,
    samples: int = 2_000,
    seed: int = 42,
) -> list[float] | None:
    """Return a deterministic percentile bootstrap CI for a sample mean."""

    if not values:
        return None
    if isinstance(samples, bool) or not isinstance(samples, int) or samples < 100:
        raise ValueError("bootstrap samples must be at least 100")
    if isinstance(seed, bool) or not isinstance(seed, int):
        raise ValueError("bootstrap seed must be an integer")
    if any(_known_score(value) is None for value in values):
        raise ValueError("bootstrap values must be finite scores in [0, 1]")
    rng = random.Random(seed)
    count = len(values)
    means = [
        sum(values[rng.randrange(count)] for _ in range(count)) / count
        for _ in range(samples)
    ]
    return [_percentile(means, 0.025), _percentile(means, 0.975)]


def evidence_sha256(payload: Any) -> str:
    """Hash a JSON observation/expectation with a stable canonical encoding."""

    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def judge_rows_sha256(rows: list[dict[str, Any]]) -> str:
    """Hash parsed judge rows in one versioned canonical domain."""

    _snapshot, digest = _snapshot_judge_rows(rows)
    return digest


def _snapshot_judge_rows(
    rows: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], str]:
    encoded = json.dumps(
        {
            "hash_domain": JUDGE_ARTIFACT_HASH_DOMAIN,
            "rows": rows,
        },
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    frozen = json.loads(encoded)["rows"]
    return frozen, hashlib.sha256(encoded).hexdigest()


def _is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def _validated_judge_cohort(value: Any) -> dict[str, str]:
    if not isinstance(value, dict):
        raise ValueError("external judge gate requires expected judge cohort")
    unknown = sorted(set(value) - set(JUDGE_COHORT_FIELDS))
    missing = sorted(set(JUDGE_COHORT_FIELDS) - set(value))
    if unknown or missing:
        details = []
        if missing:
            details.append("missing " + ", ".join(missing))
        if unknown:
            details.append("unsupported " + ", ".join(unknown))
        raise ValueError("invalid expected judge cohort: " + "; ".join(details))
    if any(not isinstance(value[field], str) or not value[field].strip() for field in JUDGE_COHORT_FIELDS):
        raise ValueError("expected judge cohort fields must be non-empty strings")
    if not _is_sha256(value["judge_prompt_sha256"]):
        raise ValueError("expected judge cohort prompt hash must be lowercase SHA-256")
    return {field: str(value[field]) for field in JUDGE_COHORT_FIELDS}


def load_resolved_judge_policy(path: str | Path) -> ResolvedJudgePolicy:
    """Resolve a policy manifest only when its raw bytes match the trusted env pin."""

    raw = Path(path).read_bytes()
    manifest_sha256 = hashlib.sha256(raw).hexdigest()
    trusted_sha256 = os.environ.get(JUDGE_POLICY_PIN_ENV)
    if not _is_sha256(trusted_sha256):
        raise ValueError(f"{JUDGE_POLICY_PIN_ENV} must pin a lowercase policy SHA-256")
    if manifest_sha256 != trusted_sha256:
        raise ValueError("judge policy manifest does not match the trusted environment pin")
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("judge policy manifest must be valid UTF-8 JSON") from exc
    if not isinstance(payload, dict) or set(payload) != {
        "schema_version",
        "policy_id",
        "revision",
        "artifact",
        "cohort",
    }:
        raise ValueError("judge policy manifest has invalid fields")
    if payload.get("schema_version") != JUDGE_POLICY_SCHEMA_VERSION:
        raise ValueError("judge policy manifest has an unsupported schema")
    policy_id = _nonempty_policy_string(payload.get("policy_id"), "policy_id")
    revision = _nonempty_policy_string(payload.get("revision"), "revision")
    artifact = payload.get("artifact")
    if not isinstance(artifact, dict) or set(artifact) != {"hash_domain", "sha256"}:
        raise ValueError("judge policy artifact binding has invalid fields")
    if artifact.get("hash_domain") != JUDGE_ARTIFACT_HASH_DOMAIN:
        raise ValueError("judge policy artifact hash domain is unsupported")
    if not _is_sha256(artifact.get("sha256")):
        raise ValueError("judge policy artifact digest must be lowercase SHA-256")
    cohort = _validated_judge_cohort(payload.get("cohort"))
    return ResolvedJudgePolicy(
        policy_id=policy_id,
        revision=revision,
        artifact_sha256=str(artifact["sha256"]),
        cohort_fields=tuple((field, cohort[field]) for field in JUDGE_COHORT_FIELDS),
        manifest_sha256=manifest_sha256,
        source=str(Path(path)),
        resolver=f"environment:{JUDGE_POLICY_PIN_ENV}",
        _authority_token=_RESOLVED_POLICY_TOKEN,
    )


def _nonempty_policy_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"judge policy {label} must be a non-empty string")
    return value.strip()


def validate_rag_cases(cases: list[dict[str, Any]]) -> dict[str, Any]:
    errors: list[dict[str, Any]] = []
    seen: set[str] = set()
    allowed = {
        "case_id",
        "track",
        "query",
        "relevance",
        "reference_answer",
        "metadata",
    }
    for index, case in enumerate(cases, start=1):
        case_id = str(case.get("case_id") or f"line-{index}")
        case_errors: list[str] = []
        unknown = sorted(set(case) - allowed)
        if unknown:
            case_errors.append(f"unsupported fields {', '.join(unknown)}")
        if case_id in seen:
            case_errors.append("duplicate case_id")
        seen.add(case_id)
        if not isinstance(case.get("case_id"), str) or not case_id.strip():
            case_errors.append("case_id must be a non-empty string")
        if case.get("track") not in RAG_TRACKS:
            case_errors.append("track must be retrieval_only or answer_aware")
        if not isinstance(case.get("query"), str) or not case.get("query", "").strip():
            case_errors.append("query must be a non-empty string")
        relevance = case.get("relevance")
        if not isinstance(relevance, dict) or not relevance:
            case_errors.append("relevance must be a non-empty object")
        else:
            positive = 0
            for segment_id, grade in relevance.items():
                if not isinstance(segment_id, str) or not segment_id.strip():
                    case_errors.append("relevance IDs must be non-empty strings")
                    continue
                if isinstance(grade, bool) or not isinstance(grade, int | float):
                    case_errors.append("relevance grades must be numeric")
                    continue
                numeric_grade = float(grade)
                if not math.isfinite(numeric_grade) or numeric_grade < 0.0:
                    case_errors.append("relevance grades must be finite and non-negative")
                elif numeric_grade > 0.0:
                    positive += 1
            if positive == 0:
                case_errors.append("relevance must contain at least one positive grade")
        if case.get("track") == "answer_aware" and (
            not isinstance(case.get("reference_answer"), str)
            or not case.get("reference_answer", "").strip()
        ):
            case_errors.append("answer_aware cases require reference_answer")
        metadata = case.get("metadata")
        if metadata is not None and not isinstance(metadata, dict):
            case_errors.append("metadata must be an object")
        elif isinstance(metadata, dict) and "critical" in metadata and not isinstance(
            metadata.get("critical"), bool
        ):
            case_errors.append("metadata.critical must be boolean")
        if case_errors:
            errors.append({"case_id": case_id, "errors": case_errors})
    return {"valid": not errors, "case_count": len(cases), "errors": errors}


def validate_rag_observations(
    cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    case_by_id = {str(case.get("case_id") or ""): case for case in cases}
    errors: list[dict[str, Any]] = []
    for case_id in sorted(set(case_by_id) - set(observations)):
        errors.append({"case_id": case_id, "errors": ["missing RAG observation"]})
    for case_id in sorted(set(observations) - set(case_by_id)):
        errors.append({"case_id": case_id, "errors": ["observation has no RAG expectation"]})
    for case_id in sorted(set(case_by_id) & set(observations)):
        case = case_by_id[case_id]
        replay = observations[case_id]
        case_errors: list[str] = []
        if replay.get("status") != "succeeded":
            case_errors.append("RAG replay status must be succeeded")
        ranked = replay.get("ranked_segment_ids")
        if (
            not isinstance(ranked, list)
            or not ranked
            or any(not isinstance(item, str) or not item.strip() for item in ranked)
        ):
            case_errors.append("ranked_segment_ids must be a non-empty string list")
        if "answer_metrics" in replay:
            case_errors.append("answer_metrics must use the explicit judge observation channel")
        answer = replay.get("answer")
        answer_source = str(replay.get("answer_source") or "")
        if case.get("track") == "retrieval_only":
            if answer is not None and answer != "":
                case_errors.append("retrieval_only observation must not contain an answer")
            if answer_source != "retrieval_only":
                case_errors.append("retrieval_only observation requires answer_source=retrieval_only")
        else:
            if not isinstance(answer, str) or not answer.strip():
                case_errors.append("answer_aware observation requires a generated answer")
            if answer_source != "generated":
                case_errors.append("answer_aware observation requires answer_source=generated")
        if case_errors:
            errors.append({"case_id": case_id, "errors": case_errors})
    return {
        "valid": not errors,
        "case_count": len(case_by_id),
        "observation_count": len(observations),
        "joined_count": len(set(case_by_id) & set(observations)),
        "errors": errors,
    }


def _retrieval_metrics_for_cases(
    cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]],
    *,
    k_values: list[int],
    bootstrap_samples: int,
) -> dict[str, Any]:
    cases = sorted(cases, key=lambda case: str(case["case_id"]))
    judgements = [
        QueryRetrievalJudgement(
            query_id=str(case["case_id"]),
            retrieved=list(observations[str(case["case_id"])]["ranked_segment_ids"]),
            relevance=dict(case["relevance"]),
        )
        for case in cases
    ]
    report = evaluate_retrieval(judgements, k_values=k_values)
    payload = report.to_dict()
    primary_k = max(k_values)
    metric_values = {
        metric: [
            float(payload["per_query"][str(case["case_id"])]["by_k"][str(primary_k)][metric])
            for case in cases
        ]
        for metric in DEFAULT_RETRIEVAL_THRESHOLDS
    }
    payload["primary_k"] = primary_k
    payload["confidence_intervals_95"] = {
        metric: bootstrap_mean_ci(values, samples=bootstrap_samples)
        for metric, values in metric_values.items()
    }
    return payload


def _evaluate_recorded_judges(
    answer_cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]],
    rows: list[dict[str, Any]],
    *,
    expected_cohort: dict[str, str],
    thresholds: dict[str, float],
    min_answer_samples: int,
    bootstrap_samples: int,
) -> tuple[dict[str, Any], list[str]]:
    failures: list[str] = []
    expected_ids = {str(case["case_id"]) for case in answer_cases}
    cases_by_id = {str(case["case_id"]): case for case in answer_cases}
    by_case: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(rows, start=1):
        case_id = str(row.get("case_id") or "")
        if not case_id or case_id in by_case:
            failures.append(f"judge row {index} has missing or duplicate case_id")
            continue
        if case_id not in expected_ids:
            failures.append(f"judge row {case_id!r} is not an answer_aware case")
        for field in (
            "provider",
            "model",
            "judge_run_id",
            "rubric_version",
            "judge_prompt_sha256",
            "expectation_sha256",
            "observation_sha256",
        ):
            if not isinstance(row.get(field), str) or not row.get(field, "").strip():
                failures.append(f"judge row {case_id!r} requires {field} provenance")
        if not _is_sha256(row.get("judge_prompt_sha256")):
            failures.append(f"judge row {case_id!r} requires lowercase judge_prompt_sha256")
        if case_id in expected_ids:
            expected_expectation_hash = evidence_sha256(cases_by_id[case_id])
            if row.get("expectation_sha256") != expected_expectation_hash:
                failures.append(f"judge row {case_id!r} expectation hash mismatch")
            observation = observations.get(case_id)
            if not isinstance(observation, dict):
                failures.append(f"judge row {case_id!r} has no observation to bind")
            elif row.get("observation_sha256") != evidence_sha256(observation):
                failures.append(f"judge row {case_id!r} observation hash mismatch")
        raw_metrics = row.get("metrics")
        normalized: dict[str, float] = {}
        if not isinstance(raw_metrics, dict):
            failures.append(f"judge row {case_id!r} requires metrics")
        else:
            for raw_name, raw_score in raw_metrics.items():
                name = ANSWER_METRIC_ALIASES.get(str(raw_name), str(raw_name))
                if name not in ANSWER_METRICS:
                    failures.append(f"judge row {case_id!r} has unsupported metric {raw_name!r}")
                    continue
                score = _known_score(raw_score)
                if score is None:
                    failures.append(f"judge row {case_id!r} has invalid {name} score")
                    continue
                if name in normalized:
                    failures.append(f"judge row {case_id!r} duplicates metric {name}")
                    continue
                normalized[name] = score
            missing = sorted(set(ANSWER_METRICS) - set(normalized))
            if missing:
                failures.append(f"judge row {case_id!r} missing metrics {', '.join(missing)}")
        by_case[case_id] = {**row, "metrics": normalized}
    missing_cases = sorted(expected_ids - set(by_case))
    if missing_cases:
        failures.append("missing answer judge rows: " + ", ".join(missing_cases))
    if len(answer_cases) < min_answer_samples:
        failures.append(
            f"answer judge sample_count {len(answer_cases)} < minimum {min_answer_samples}"
        )

    cohort_keys = {
        (
            row.get("provider"),
            row.get("model"),
            row.get("judge_run_id"),
            row.get("rubric_version"),
            row.get("judge_prompt_sha256"),
        )
        for case_id, row in by_case.items()
        if case_id in expected_ids
        and all(
            isinstance(row.get(field), str)
            for field in JUDGE_COHORT_FIELDS
        )
    }
    if len(cohort_keys) != 1:
        failures.append(
            "answer judge rows must use one provider/model/run/rubric/prompt cohort"
        )
        cohort: dict[str, Any] | None = None
    else:
        provider, model, judge_run_id, rubric_version, judge_prompt_sha256 = next(
            iter(cohort_keys)
        )
        cohort = {
            "provider": provider,
            "model": model,
            "judge_run_id": judge_run_id,
            "rubric_version": rubric_version,
            "judge_prompt_sha256": judge_prompt_sha256,
        }
        if cohort != expected_cohort:
            failures.append("answer judge cohort does not match the pinned expected cohort")
        cohort["cohort_sha256"] = evidence_sha256(cohort)

    metrics: dict[str, Any] = {}
    for metric in ANSWER_METRICS:
        values = [
            float(by_case[case_id]["metrics"][metric])
            for case_id in sorted(expected_ids & set(by_case))
            if metric in by_case[case_id].get("metrics", {})
        ]
        mean = sum(values) / len(values) if values else None
        interval = (
            bootstrap_mean_ci(values, samples=bootstrap_samples) if values else None
        )
        metrics[metric] = {
            "sample_count": len(values),
            "mean": round(mean, 4) if mean is not None else None,
            "ci_95": interval,
        }
        if interval is None or interval[0] < thresholds[metric]:
            failures.append(
                f"{metric} lower_ci {interval[0] if interval else None} < {thresholds[metric]:.4f}"
            )
    critical_results: list[dict[str, Any]] = []
    for case in sorted(answer_cases, key=lambda item: str(item["case_id"])):
        metadata = case.get("metadata")
        if not isinstance(metadata, dict) or metadata.get("critical") is not True:
            continue
        case_id = str(case["case_id"])
        row = by_case.get(case_id)
        case_failures: list[str] = []
        if row is None:
            case_failures.append("judge row is missing")
            normalized_metrics: dict[str, float] = {}
        else:
            normalized_metrics = row.get("metrics") or {}
            if row.get("expectation_sha256") != evidence_sha256(case):
                case_failures.append("expectation provenance hash mismatch")
            observation = observations.get(case_id)
            if not isinstance(observation, dict) or row.get(
                "observation_sha256"
            ) != evidence_sha256(observation):
                case_failures.append("observation provenance hash mismatch")
            if any(row.get(field) != expected_cohort[field] for field in JUDGE_COHORT_FIELDS):
                case_failures.append("judge cohort does not match policy")
        for metric, threshold in thresholds.items():
            score = normalized_metrics.get(metric)
            if score is None:
                case_failures.append(f"{metric} is missing")
            elif score < threshold:
                case_failures.append(f"{metric} {score:.4f} < {threshold:.4f}")
        critical_results.append(
            {
                "case_id": case_id,
                "passed": not case_failures,
                "metrics": {
                    metric: normalized_metrics.get(metric) for metric in ANSWER_METRICS
                },
                "failures": case_failures,
            }
        )
        if case_failures:
            failures.append(
                f"critical RAG answer case {case_id} failed: "
                + "; ".join(case_failures)
            )
    return {
        "status": "fail" if failures else "pass",
        "evidence_scope": "recorded_external_judge_opt_in",
        "eligible_case_count": len(answer_cases),
        "expected_cohort": {
            **expected_cohort,
            "cohort_sha256": evidence_sha256(expected_cohort),
        },
        "cohort": cohort,
        "thresholds": thresholds,
        "metrics": metrics,
        "critical": {
            "case_count": len(critical_results),
            "status": (
                "pass"
                if critical_results and all(row["passed"] for row in critical_results)
                else "fail"
                if critical_results
                else "not_present"
            ),
            "results": critical_results,
        },
        "provenance": [
            {
                "case_id": case_id,
                "provider": row.get("provider"),
                "model": row.get("model"),
                "judge_run_id": row.get("judge_run_id"),
                "rubric_version": row.get("rubric_version"),
                "judge_prompt_sha256": row.get("judge_prompt_sha256"),
                "expectation_sha256": row.get("expectation_sha256"),
                "observation_sha256": row.get("observation_sha256"),
            }
            for case_id, row in sorted(by_case.items())
        ],
    }, failures


def evaluate_rag_regression(
    cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]],
    *,
    k_values: list[int] | None = None,
    retrieval_thresholds: dict[str, float] | None = None,
    min_total_samples: int = 8,
    min_track_samples: int = 4,
    required_tracks: Sequence[str] | None = None,
    bootstrap_samples: int = 2_000,
    enable_external_judge_gate: bool = False,
    judge_rows: list[dict[str, Any]] | None = None,
    judge_policy: ResolvedJudgePolicy | None = None,
    answer_thresholds: dict[str, float] | None = None,
    min_answer_samples: int = 4,
) -> dict[str, Any]:
    """Evaluate deterministic retrieval and optional recorded answer judges."""

    if judge_rows is not None and not enable_external_judge_gate:
        raise ValueError("judge observations require --enable-external-judge-gate")
    if enable_external_judge_gate and judge_rows is None:
        raise ValueError("external judge gate requires judge observations")
    if judge_policy is not None and not enable_external_judge_gate:
        raise ValueError("judge policy requires --enable-external-judge-gate")
    ks = [1, 3, 5] if k_values is None else k_values
    if not isinstance(ks, list) or not ks or any(
        isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 100
        for value in ks
    ):
        raise ValueError("k_values must contain integers between 1 and 100")
    if len(set(ks)) != len(ks):
        raise ValueError("k_values must not contain duplicates")
    minimums = (min_total_samples, min_track_samples, min_answer_samples)
    if any(
        isinstance(value, bool) or not isinstance(value, int) or value <= 0
        for value in minimums
    ):
        raise ValueError("minimum sample counts must be positive")
    if (
        isinstance(bootstrap_samples, bool)
        or not isinstance(bootstrap_samples, int)
        or bootstrap_samples < 100
    ):
        raise ValueError("bootstrap samples must be at least 100")
    if not isinstance(enable_external_judge_gate, bool):
        raise ValueError("enable_external_judge_gate must be boolean")
    if judge_rows is not None and not isinstance(judge_rows, list):
        raise ValueError("judge observations must be a list")
    if enable_external_judge_gate and not isinstance(judge_policy, ResolvedJudgePolicy):
        raise ValueError("external judge gate requires a trusted resolved judge policy")
    pinned_judge_cohort = judge_policy.cohort if judge_policy is not None else None
    frozen_judge_rows: list[dict[str, Any]] = []
    observed_judge_artifact_sha256 = None
    if enable_external_judge_gate:
        frozen_judge_rows, observed_judge_artifact_sha256 = _snapshot_judge_rows(
            judge_rows or []
        )
    judge_artifact_matches = bool(
        enable_external_judge_gate
        and judge_policy is not None
        and observed_judge_artifact_sha256 == judge_policy.artifact_sha256
    )
    retrieval_thresholds = _validated_thresholds(
        retrieval_thresholds,
        defaults=DEFAULT_RETRIEVAL_THRESHOLDS,
        label="retrieval",
    )
    answer_thresholds = _validated_thresholds(
        answer_thresholds,
        defaults=DEFAULT_ANSWER_THRESHOLDS,
        label="answer",
    )

    case_validation = validate_rag_cases(cases)
    observation_validation = validate_rag_observations(cases, observations)
    failures = [
        f"{item['case_id']}: {'; '.join(item['errors'])}"
        for item in [*case_validation["errors"], *observation_validation["errors"]]
    ]
    tracks = {
        track: [case for case in cases if case.get("track") == track] for track in RAG_TRACKS
    }
    if len(cases) < min_total_samples:
        failures.append(f"sample_count {len(cases)} < minimum {min_total_samples}")
    checked_tracks = RAG_TRACKS if required_tracks is None else tuple(required_tracks)
    if set(checked_tracks) - set(RAG_TRACKS):
        raise ValueError("required_tracks must be a subset of the RAG tracks")
    for track in checked_tracks:
        track_cases = tracks.get(track, [])
        if len(track_cases) < min_track_samples:
            failures.append(
                f"{track} sample_count {len(track_cases)} < minimum {min_track_samples}"
            )

    can_score = case_validation["valid"] and observation_validation["valid"] and bool(cases)
    retrieval: dict[str, Any] = {
        "status": "not_scored",
        "thresholds": retrieval_thresholds,
        "all": None,
        "tracks": {},
        "critical": {
            "case_count": sum(
                (case.get("metadata") or {}).get("critical") is True
                for case in cases
                if isinstance(case.get("metadata") or {}, dict)
            ),
            "status": "not_scored",
            "results": [],
        },
    }
    if can_score:
        retrieval["all"] = _retrieval_metrics_for_cases(
            cases,
            observations,
            k_values=ks,
            bootstrap_samples=bootstrap_samples,
        )
        retrieval["tracks"] = {
            track: _retrieval_metrics_for_cases(
                track_cases,
                observations,
                k_values=ks,
                bootstrap_samples=bootstrap_samples,
            )
            for track, track_cases in tracks.items()
            if track_cases
        }
        for scope, payload in [("all", retrieval["all"]), *retrieval["tracks"].items()]:
            primary_k = str(payload["primary_k"])
            primary = payload["metrics"][primary_k]
            intervals = payload["confidence_intervals_95"]
            for metric, threshold in retrieval_thresholds.items():
                point = float(primary[metric])
                interval = intervals.get(metric)
                if point < threshold:
                    failures.append(f"{scope} {metric} {point:.4f} < {threshold:.4f}")
                if interval is None or interval[0] < threshold:
                    failures.append(
                        f"{scope} {metric} lower_ci "
                        f"{interval[0] if interval else None} < {threshold:.4f}"
                    )
        critical_results: list[dict[str, Any]] = []
        primary_k = str(retrieval["all"]["primary_k"])
        per_query = retrieval["all"]["per_query"]
        for case in sorted(cases, key=lambda item: str(item["case_id"])):
            metadata = case.get("metadata")
            if not isinstance(metadata, dict) or metadata.get("critical") is not True:
                continue
            case_id = str(case["case_id"])
            values = per_query[case_id]["by_k"][primary_k]
            case_failures = []
            if values["hit_rate"] != 1.0:
                case_failures.append("required retrieval hit is missing")
            for metric, threshold in retrieval_thresholds.items():
                if float(values[metric]) < threshold:
                    case_failures.append(
                        f"{metric} {float(values[metric]):.4f} < {threshold:.4f}"
                    )
            critical_results.append(
                {
                    "case_id": case_id,
                    "track": case["track"],
                    "passed": not case_failures,
                    "metrics": {
                        metric: values[metric]
                        for metric in ("hit_rate", *retrieval_thresholds)
                    },
                    "failures": case_failures,
                }
            )
            if case_failures:
                failures.append(
                    f"critical RAG retrieval case {case_id} failed: "
                    + "; ".join(case_failures)
                )
        retrieval["critical"] = {
            "case_count": len(critical_results),
            "status": (
                "pass"
                if critical_results and all(row["passed"] for row in critical_results)
                else "fail"
                if critical_results
                else "not_present"
            ),
            "results": critical_results,
        }
        retrieval["status"] = "pass" if not failures else "fail"

    answer_cases = tracks["answer_aware"]
    if enable_external_judge_gate:
        answer_quality, answer_failures = _evaluate_recorded_judges(
            answer_cases,
            observations,
            frozen_judge_rows,
            expected_cohort=pinned_judge_cohort or {},
            thresholds=answer_thresholds,
            min_answer_samples=min_answer_samples,
            bootstrap_samples=bootstrap_samples,
        )
        artifact_failures = []
        if not judge_artifact_matches:
            artifact_failures.append(
                "recorded answer judge artifact does not match the policy-pinned SHA-256"
            )
        answer_failures = [*artifact_failures, *answer_failures]
        answer_quality["status"] = "fail" if answer_failures else "pass"
        answer_quality["failures"] = list(dict.fromkeys(answer_failures))
        answer_quality["artifact_binding"] = {
            "algorithm": "sha256",
            "hash_domain": JUDGE_ARTIFACT_HASH_DOMAIN,
            "expected_sha256": judge_policy.artifact_sha256 if judge_policy else None,
            "observed_sha256": observed_judge_artifact_sha256,
            "matched": judge_artifact_matches,
        }
        answer_quality["policy"] = (
            {
                "schema_version": JUDGE_POLICY_SCHEMA_VERSION,
                "policy_id": judge_policy.policy_id,
                "revision": judge_policy.revision,
                "manifest_sha256": judge_policy.manifest_sha256,
                "source": judge_policy.source,
                "resolver": judge_policy.resolver,
            }
            if judge_policy is not None
            else None
        )
        failures.extend(answer_failures)
    else:
        answer_quality = {
            "status": "not_run",
            "evidence_scope": "external_judge_opt_in_required",
            "eligible_case_count": len(answer_cases),
            "metrics": {
                metric: {"sample_count": 0, "mean": None, "ci_95": None}
                for metric in ANSWER_METRICS
            },
            "critical": {
                "case_count": sum(
                    (case.get("metadata") or {}).get("critical") is True
                    for case in answer_cases
                    if isinstance(case.get("metadata") or {}, dict)
                ),
                "status": "not_run",
                "results": [],
            },
        }

    failures = list(dict.fromkeys(failures))
    if failures:
        gate_outcome = "fail"
    elif enable_external_judge_gate:
        gate_outcome = "retrieval_and_recorded_answer_pass"
    else:
        gate_outcome = "retrieval_pass_answer_not_run"
    return {
        "schema_version": "rag-eval-regression-v1",
        "evidence_scope": "recorded_offline_retrieval",
        "sample_count": len(cases),
        "track_counts": {track: len(track_cases) for track, track_cases in tracks.items()},
        "minimum_samples": {
            "total": min_total_samples,
            "per_track": min_track_samples,
            "answer_judge": min_answer_samples,
        },
        "retrieval": retrieval,
        "answer_quality": answer_quality,
        "gate": {
            "status": "fail" if failures else "pass",
            "outcome": gate_outcome,
            "scope": (
                "offline_retrieval_and_recorded_answer"
                if enable_external_judge_gate
                else "offline_retrieval_only"
            ),
            "not_run": [] if enable_external_judge_gate else ["answer_quality"],
            "failures": failures,
        },
        "evidence_tiers": {
            "offline_fixture_contract": "verified" if can_score else "invalid",
            "runtime_retrieval": "not_run",
            "external_answer_judge": (
                "recorded_bound_opt_in"
                if enable_external_judge_gate
                and judge_artifact_matches
                and answer_quality.get("status") == "pass"
                else "not_verified"
                if enable_external_judge_gate
                else "not_run"
            ),
            "real_provider_call": "not_run",
        },
    }


def load_rag_fixture_pair(
    expectation_path: str | Path,
    observation_path: str | Path,
) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    return load_jsonl(expectation_path), load_observations(observation_path)


def write_rag_report(result: dict[str, Any], output_path: str | Path) -> None:
    target = Path(output_path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(result, indent=2, sort_keys=True), encoding="utf-8")
