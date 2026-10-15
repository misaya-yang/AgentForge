from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import re
from pathlib import Path

import pytest

from src.services.eval.rag_regression import (
    JUDGE_ARTIFACT_HASH_DOMAIN,
    JUDGE_POLICY_PIN_ENV,
    JUDGE_POLICY_SCHEMA_VERSION,
    ResolvedJudgePolicy,
    bootstrap_mean_ci,
    evaluate_rag_regression,
    evidence_sha256,
    judge_rows_sha256,
    load_rag_fixture_pair,
    load_resolved_judge_policy,
    validate_rag_cases,
    validate_rag_observations,
)

GOLDEN = Path("tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl")
OBSERVATIONS = Path("tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl")


def _fixtures() -> tuple[list[dict], dict[str, dict]]:
    return load_rag_fixture_pair(GOLDEN, OBSERVATIONS)


def _large_rag_cohort(size_per_track: int = 100) -> tuple[list[dict], dict[str, dict]]:
    cases: list[dict] = []
    observations: dict[str, dict] = {}
    for track in ("retrieval_only", "answer_aware"):
        for index in range(size_per_track):
            prefix = "retrieval" if track == "retrieval_only" else "answer"
            subject = "tenant" if track == "retrieval_only" else "password"
            case_id = f"rag.{prefix}.{subject}-{index}"
            segment_id = f"segment-{prefix}-{index}"
            case = {
                "case_id": case_id,
                "track": track,
                "query": f"query {track} {index}",
                "relevance": {segment_id: 1.0},
                "metadata": {"critical": index == 0},
            }
            replay = {
                "status": "succeeded",
                "ranked_segment_ids": [
                    segment_id,
                    f"irrelevant-{prefix}-{index}-1",
                    f"irrelevant-{prefix}-{index}-2",
                    f"irrelevant-{prefix}-{index}-3",
                    f"irrelevant-{prefix}-{index}-4",
                ],
                "answer_source": "retrieval_only",
            }
            if track == "answer_aware":
                case["reference_answer"] = f"reference {index}"
                replay.update(
                    {
                        "answer": f"generated answer {index}",
                        "answer_source": "generated",
                    }
                )
            cases.append(case)
            observations[case_id] = replay
    return cases, observations


def _load_eval_rag_main():
    script_path = Path(__file__).resolve().parents[3] / "scripts" / "eval_rag.py"
    spec = importlib.util.spec_from_file_location("eval_rag_script", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load eval RAG script: {script_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.main


eval_rag_main = _load_eval_rag_main()


def test_rag_fixture_runs_both_tracks_with_retrieval_metrics_and_ci() -> None:
    cases, observations = _fixtures()

    assert validate_rag_cases(cases)["valid"] is True
    assert validate_rag_observations(cases, observations)["valid"] is True
    result = evaluate_rag_regression(cases, observations)

    assert result["gate"] == {
        "status": "pass",
        "outcome": "retrieval_pass_answer_not_run",
        "scope": "offline_retrieval_only",
        "not_run": ["answer_quality"],
        "failures": [],
    }
    assert result["sample_count"] == 12
    assert result["track_counts"] == {"retrieval_only": 6, "answer_aware": 6}
    assert result["retrieval"]["status"] == "pass"
    assert result["retrieval"]["critical"]["case_count"] == 12
    assert result["retrieval"]["critical"]["status"] == "pass"
    assert result["retrieval"]["all"]["metrics"]["5"]["recall_at_k"] == 1.0
    assert result["retrieval"]["all"]["metrics"]["5"]["mrr"] == 1.0
    assert result["retrieval"]["all"]["metrics"]["5"]["ndcg_at_k"] == 1.0
    assert result["retrieval"]["all"]["confidence_intervals_95"] == {
        "recall_at_k": [1.0, 1.0],
        "mrr": [1.0, 1.0],
        "ndcg_at_k": [1.0, 1.0],
    }
    assert result["answer_quality"]["status"] == "not_run"
    assert result["answer_quality"]["critical"] == {
        "case_count": 6,
        "status": "not_run",
        "results": [],
    }
    assert result["answer_quality"]["evidence_scope"] == "external_judge_opt_in_required"
    assert result["answer_quality"]["metrics"]["faithfulness"]["mean"] is None
    assert result["evidence_tiers"]["real_provider_call"] == "not_run"


def test_rag_case_critical_metadata_is_optional_boolean_only() -> None:
    cases, _observations = _fixtures()
    missing = copy.deepcopy(cases[0])
    missing["metadata"].pop("critical")
    explicit_false = copy.deepcopy(cases[1])
    explicit_false["metadata"]["critical"] = False
    invalid = copy.deepcopy(cases[2])
    invalid["metadata"]["critical"] = "yes"

    assert validate_rag_cases([missing, explicit_false])["valid"] is True
    validation = validate_rag_cases([invalid])
    assert validation["valid"] is False
    assert "metadata.critical must be boolean" in validation["errors"][0]["errors"]


def test_one_critical_retrieval_miss_blocks_an_otherwise_strong_large_cohort() -> None:
    cases, observations = _large_rag_cohort()
    critical_case_id = "rag.retrieval.tenant-0"
    observations[critical_case_id]["ranked_segment_ids"] = [
        "irrelevant-a",
        "irrelevant-b",
        "irrelevant-c",
        "irrelevant-d",
        "irrelevant-e",
    ]

    result = evaluate_rag_regression(cases, observations, bootstrap_samples=500)

    assert result["retrieval"]["all"]["confidence_intervals_95"]["recall_at_k"][0] > 0.8
    assert result["gate"]["status"] == "fail"
    assert result["retrieval"]["critical"]["status"] == "fail"
    assert "critical RAG retrieval case" in " ".join(result["gate"]["failures"])


def test_rag_gate_fails_closed_on_empty_or_undersized_samples() -> None:
    empty = evaluate_rag_regression([], {})
    assert empty["gate"]["status"] == "fail"
    assert "sample_count 0 < minimum 8" in empty["gate"]["failures"]

    cases, observations = _fixtures()
    subset = [cases[0], cases[6]]
    subset_observations = {str(case["case_id"]): observations[str(case["case_id"])] for case in subset}
    undersized = evaluate_rag_regression(subset, subset_observations)

    assert undersized["gate"]["status"] == "fail"
    assert "sample_count 2 < minimum 8" in undersized["gate"]["failures"]
    assert "retrieval_only sample_count 1 < minimum 4" in undersized["gate"]["failures"]
    assert "answer_aware sample_count 1 < minimum 4" in undersized["gate"]["failures"]


def test_rag_gate_rejects_valid_but_low_quality_rankings() -> None:
    cases, observations = _fixtures()
    degraded = copy.deepcopy(observations)
    for replay in degraded.values():
        replay["ranked_segment_ids"] = [
            "irrelevant-1",
            "irrelevant-2",
            "irrelevant-3",
            "irrelevant-4",
            "irrelevant-5",
        ]

    result = evaluate_rag_regression(cases, degraded)

    assert result["gate"]["status"] == "fail"
    failures = " ".join(result["gate"]["failures"])
    assert "recall_at_k 0.0000 < 0.8000" in failures
    assert "mrr 0.0000 < 0.8000" in failures
    assert "ndcg_at_k 0.0000 < 0.8000" in failures


def test_rag_bootstrap_and_gate_are_invariant_to_fixture_order() -> None:
    cases, observations = _fixtures()

    forward = evaluate_rag_regression(cases, observations, bootstrap_samples=500)
    reversed_order = evaluate_rag_regression(
        list(reversed(cases)),
        observations,
        bootstrap_samples=500,
    )

    assert reversed_order["retrieval"] == forward["retrieval"]
    assert reversed_order["gate"] == forward["gate"]


@pytest.mark.parametrize(
    ("case_id", "mutation", "expected_failure"),
    [
        (
            "rag.retrieval.refund",
            {"answer": "document count is not a generated answer"},
            "retrieval_only observation must not contain an answer",
        ),
        (
            "rag.answer.refund",
            {"answer": None},
            "answer_aware observation requires a generated answer",
        ),
        (
            "rag.retrieval.password",
            {"answer_metrics": {"faithfulness": 1.0}},
            "answer_metrics must use the explicit judge observation channel",
        ),
    ],
)
def test_rag_tracks_reject_answer_metric_contamination(
    case_id: str,
    mutation: dict,
    expected_failure: str,
) -> None:
    cases, observations = _fixtures()
    contaminated = copy.deepcopy(observations)
    contaminated[case_id].update(mutation)

    validation = validate_rag_observations(cases, contaminated)
    result = evaluate_rag_regression(cases, contaminated)

    assert validation["valid"] is False
    assert expected_failure in json.dumps(validation["errors"])
    assert result["gate"]["status"] == "fail"
    assert result["answer_quality"]["status"] == "not_run"


def test_bootstrap_ci_is_seeded_and_deterministic() -> None:
    values = [1.0, 0.5, 0.0, 1.0, 0.5, 1.0]

    first = bootstrap_mean_ci(values, samples=500, seed=42)
    second = bootstrap_mean_ci(values, samples=500, seed=42)

    assert first == second
    assert first is not None
    assert 0.0 <= first[0] <= first[1] <= 1.0


def test_bootstrap_gate_does_not_round_just_below_threshold_up_to_a_pass(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations, score=0.69996)

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "fail"
    assert result["answer_quality"]["metrics"]["faithfulness"]["ci_95"][0] < 0.7


@pytest.mark.parametrize(
    ("kwargs", "message"),
    [
        ({"k_values": []}, "k_values must contain integers between 1 and 100"),
        ({"k_values": [0, 5]}, "k_values must contain integers between 1 and 100"),
        ({"k_values": [101]}, "k_values must contain integers between 1 and 100"),
        ({"k_values": [5, 5]}, "k_values must not contain duplicates"),
        ({"bootstrap_samples": 99}, "bootstrap samples must be at least 100"),
        ({"bootstrap_samples": 100.5}, "bootstrap samples must be at least 100"),
        ({"min_total_samples": True}, "minimum sample counts must be positive"),
        (
            {"retrieval_thresholds": {"recall_at_k": 1.01}},
            "retrieval threshold recall_at_k must be finite and in [0, 1]",
        ),
        (
            {"retrieval_thresholds": {"precision_at_k": 0.8}},
            "unsupported retrieval thresholds: precision_at_k",
        ),
    ],
)
def test_gate_rejects_invalid_statistical_configuration(
    kwargs: dict[str, object],
    message: str,
) -> None:
    cases, observations = _fixtures()

    with pytest.raises(ValueError, match=re.escape(message)):
        evaluate_rag_regression(cases, observations, **kwargs)


def _judge_rows(
    cases: list[dict],
    observations: dict[str, dict],
    *,
    score: float = 0.9,
) -> list[dict]:
    return [
        {
            "case_id": case["case_id"],
            "provider": "recorded-provider",
            "model": "recorded-judge-model",
            "judge_run_id": "judge-batch-1",
            "rubric_version": "rag-answer-v1",
            "judge_prompt_sha256": "a" * 64,
            "expectation_sha256": evidence_sha256(case),
            "observation_sha256": evidence_sha256(observations[str(case["case_id"])]),
            "metrics": {
                "faithfulness": score,
                "answer_relevancy": score,
            },
        }
        for case in cases
        if case["track"] == "answer_aware"
    ]


def _judge_cohort() -> dict[str, str]:
    return {
        "provider": "recorded-provider",
        "model": "recorded-judge-model",
        "judge_run_id": "judge-batch-1",
        "rubric_version": "rag-answer-v1",
        "judge_prompt_sha256": "a" * 64,
    }


def _judge_policy_payload(
    rows: list[dict],
    *,
    cohort: dict[str, str] | None = None,
) -> dict:
    return {
        "schema_version": JUDGE_POLICY_SCHEMA_VERSION,
        "policy_id": "rag-answer-regression",
        "revision": "v1",
        "artifact": {
            "hash_domain": JUDGE_ARTIFACT_HASH_DOMAIN,
            "sha256": judge_rows_sha256(rows),
        },
        "cohort": cohort or _judge_cohort(),
    }


def _resolved_judge_policy(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    rows: list[dict],
    *,
    cohort: dict[str, str] | None = None,
) -> ResolvedJudgePolicy:
    path = tmp_path / "judge-policy.json"
    raw = json.dumps(
        _judge_policy_payload(rows, cohort=cohort),
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    path.write_bytes(raw)
    monkeypatch.setenv(JUDGE_POLICY_PIN_ENV, hashlib.sha256(raw).hexdigest())
    return load_resolved_judge_policy(path)


def test_external_answer_judge_gate_requires_explicit_opt_in_and_provenance(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)

    with pytest.raises(ValueError, match="enable-external-judge-gate"):
        evaluate_rag_regression(cases, observations, judge_rows=rows)
    with pytest.raises(ValueError, match="requires judge observations"):
        evaluate_rag_regression(cases, observations, enable_external_judge_gate=True)
    with pytest.raises(ValueError, match="judge observations must be a list"):
        evaluate_rag_regression(
            cases,
            observations,
            enable_external_judge_gate=True,
            judge_rows={},  # type: ignore[arg-type]
        )
    with pytest.raises(ValueError, match="requires a trusted resolved judge policy"):
        evaluate_rag_regression(
            cases,
            observations,
            enable_external_judge_gate=True,
            judge_rows=rows,
        )
    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "pass"
    assert result["answer_quality"]["status"] == "pass"
    assert result["answer_quality"]["evidence_scope"] == "recorded_external_judge_opt_in"
    assert result["answer_quality"]["metrics"]["faithfulness"]["sample_count"] == 6
    assert result["answer_quality"]["metrics"]["response_relevancy"]["mean"] == 0.9
    assert result["answer_quality"]["cohort"] == {
        "provider": "recorded-provider",
        "model": "recorded-judge-model",
        "judge_run_id": "judge-batch-1",
        "rubric_version": "rag-answer-v1",
        "judge_prompt_sha256": "a" * 64,
        "cohort_sha256": result["answer_quality"]["cohort"]["cohort_sha256"],
    }
    assert len(result["answer_quality"]["cohort"]["cohort_sha256"]) == 64
    assert result["answer_quality"]["artifact_binding"]["matched"] is True
    assert len(result["answer_quality"]["provenance"]) == 6
    assert result["gate"]["outcome"] == "retrieval_and_recorded_answer_pass"
    assert result["evidence_tiers"]["external_answer_judge"] == "recorded_bound_opt_in"
    assert result["evidence_tiers"]["real_provider_call"] == "not_run"


def test_external_answer_judge_gate_rejects_mixed_judge_cohorts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)
    rows[1]["provider"] = "other-provider"
    rows[1]["model"] = "other-model"
    rows[1]["rubric_version"] = "totally-different-v99"
    rows[1]["judge_prompt_sha256"] = "b" * 64

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "fail"
    assert "one provider/model/run/rubric/prompt cohort" in " ".join(
        result["gate"]["failures"]
    )
    assert result["answer_quality"]["cohort"] is None


def test_external_answer_judge_gate_must_match_pinned_expected_cohort(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)
    wrong_expected = {**_judge_cohort(), "model": "different-expected-model"}

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(
            tmp_path,
            monkeypatch,
            rows,
            cohort=wrong_expected,
        ),
    )

    assert result["gate"]["status"] == "fail"
    assert "does not match the pinned expected cohort" in " ".join(
        result["gate"]["failures"]
    )


def test_external_answer_judge_gate_is_bound_to_the_evaluated_answer(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)
    tampered = copy.deepcopy(observations)
    tampered["rag.answer.refund"]["answer"] = "Completely unsupported answer."

    result = evaluate_rag_regression(
        cases,
        tampered,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "fail"
    assert any("observation hash mismatch" in item for item in result["gate"]["failures"])
    assert result["retrieval"]["status"] == "pass"
    assert result["answer_quality"]["status"] == "fail"


def test_one_critical_answer_failure_blocks_an_otherwise_strong_large_cohort(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _large_rag_cohort()
    rows = _judge_rows(cases, observations)
    critical = next(row for row in rows if row["case_id"] == "rag.answer.password-0")
    critical["metrics"] = {"faithfulness": 0.1, "answer_relevancy": 0.1}

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
        bootstrap_samples=500,
    )

    assert result["answer_quality"]["metrics"]["faithfulness"]["ci_95"][0] > 0.7
    assert result["gate"]["status"] == "fail"
    assert result["retrieval"]["status"] == "pass"
    assert result["answer_quality"]["critical"]["status"] == "fail"
    assert "critical RAG answer case" in " ".join(result["gate"]["failures"])


def test_critical_answer_provenance_mismatch_is_a_hard_failure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)
    rows[0]["observation_sha256"] = "f" * 64

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "fail"
    assert result["retrieval"]["status"] == "pass"
    assert result["answer_quality"]["critical"]["status"] == "fail"
    critical_result = next(
        item
        for item in result["answer_quality"]["critical"]["results"]
        if item["case_id"] == rows[0]["case_id"]
    )
    assert "observation provenance hash mismatch" in " ".join(
        critical_result["failures"]
    )


def test_external_answer_judge_gate_fails_missing_or_low_evidence(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations, score=0.2)
    rows.pop()

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, rows),
    )

    assert result["gate"]["status"] == "fail"
    failures = " ".join(result["gate"]["failures"])
    assert "missing answer judge rows" in failures
    assert "faithfulness lower_ci" in failures
    assert "response_relevancy lower_ci" in failures


def test_external_answer_judge_gate_rejects_metrics_changed_after_policy_pin(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    trusted_rows = _judge_rows(cases, observations, score=0.2)
    tampered_rows = copy.deepcopy(trusted_rows)
    for row in tampered_rows:
        row["metrics"] = {"faithfulness": 0.9, "answer_relevancy": 0.9}

    result = evaluate_rag_regression(
        cases,
        observations,
        enable_external_judge_gate=True,
        judge_rows=tampered_rows,
        judge_policy=_resolved_judge_policy(tmp_path, monkeypatch, trusted_rows),
    )

    assert result["gate"]["status"] == "fail"
    assert result["answer_quality"]["artifact_binding"]["matched"] is False
    assert "policy-pinned SHA-256" in " ".join(result["gate"]["failures"])
    assert result["evidence_tiers"]["external_answer_judge"] == "not_verified"


def test_rag_cli_binds_judge_rows_to_one_hashed_byte_snapshot(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    rows = _judge_rows(cases, observations)
    judge_path = tmp_path / "judge.jsonl"
    output = tmp_path / "rag-judge-report.json"
    judge_path.write_text(
        "\n".join(json.dumps(row, sort_keys=True) for row in rows) + "\n",
        encoding="utf-8",
    )
    raw_judge_sha256 = hashlib.sha256(judge_path.read_bytes()).hexdigest()
    policy_path = tmp_path / "judge-policy.json"
    policy_raw = json.dumps(
        _judge_policy_payload(rows),
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    policy_path.write_bytes(policy_raw)
    policy_sha256 = hashlib.sha256(policy_raw).hexdigest()
    monkeypatch.setenv(JUDGE_POLICY_PIN_ENV, policy_sha256)

    exit_code = eval_rag_main(
        [
            "gate",
            str(GOLDEN),
            "--observations",
            str(OBSERVATIONS),
            "--judge-observations",
            str(judge_path),
            "--judge-policy-manifest",
            str(policy_path),
            "--enable-external-judge-gate",
            "--bootstrap-samples",
            "500",
            "--output",
            str(output),
        ]
    )

    payload = json.loads(output.read_text(encoding="utf-8"))
    assert exit_code == 0
    assert payload["gate"]["status"] == "pass"
    assert payload["provenance"]["judge_observations"] == {
        "source": str(judge_path),
        "raw_source_sha256": raw_judge_sha256,
        "artifact_binding": {
            "algorithm": "sha256",
            "hash_domain": JUDGE_ARTIFACT_HASH_DOMAIN,
            "expected_sha256": judge_rows_sha256(rows),
            "observed_sha256": judge_rows_sha256(rows),
            "matched": True,
        },
    }
    assert payload["provenance"]["judge_policy"]["manifest_sha256"] == policy_sha256
    assert payload["provenance"]["judge_policy"]["resolver"] == (
        f"environment:{JUDGE_POLICY_PIN_ENV}"
    )


def test_judge_policy_rejects_unpinned_or_wrongly_pinned_manifest(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cases, observations = _fixtures()
    path = tmp_path / "self-authored-policy.json"
    path.write_text(
        json.dumps(_judge_policy_payload(_judge_rows(cases, observations))),
        encoding="utf-8",
    )
    monkeypatch.delenv(JUDGE_POLICY_PIN_ENV, raising=False)

    with pytest.raises(ValueError, match=JUDGE_POLICY_PIN_ENV):
        load_resolved_judge_policy(path)

    monkeypatch.setenv(JUDGE_POLICY_PIN_ENV, "f" * 64)
    with pytest.raises(ValueError, match="trusted environment pin"):
        load_resolved_judge_policy(path)


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ("unknown_field", "invalid fields"),
        ("hash_domain", "hash domain is unsupported"),
        ("artifact_digest", "digest must be lowercase SHA-256"),
        ("cohort", "invalid expected judge cohort"),
    ],
)
def test_judge_policy_rejects_semantic_tampering_even_when_raw_bytes_are_pinned(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    mutation: str,
    message: str,
) -> None:
    cases, observations = _fixtures()
    payload = _judge_policy_payload(_judge_rows(cases, observations))
    if mutation == "unknown_field":
        payload["unexpected"] = True
    elif mutation == "hash_domain":
        payload["artifact"]["hash_domain"] = "untrusted-domain/v9"
    elif mutation == "artifact_digest":
        payload["artifact"]["sha256"] = "not-a-digest"
    elif mutation == "cohort":
        payload["cohort"]["unexpected"] = "drift"
    raw = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    path = tmp_path / f"tampered-{mutation}.json"
    path.write_bytes(raw)
    monkeypatch.setenv(JUDGE_POLICY_PIN_ENV, hashlib.sha256(raw).hexdigest())

    with pytest.raises(ValueError, match=message):
        load_resolved_judge_policy(path)


def test_rag_cli_writes_a_provenanced_offline_report(tmp_path: Path) -> None:
    output = tmp_path / "rag-report.json"

    exit_code = eval_rag_main(
        [
            "gate",
            str(GOLDEN),
            "--observations",
            str(OBSERVATIONS),
            "--output",
            str(output),
            "--bootstrap-samples",
            "500",
        ]
    )

    assert exit_code == 0
    payload = json.loads(output.read_text(encoding="utf-8"))
    assert payload["gate"]["status"] == "pass"
    assert payload["answer_quality"]["status"] == "not_run"
    assert len(payload["provenance"]["expectations"]["sha256"]) == 64
    assert len(payload["provenance"]["observations"]["sha256"]) == 64
    assert payload["provenance"]["provider_execution"] == "not_performed_by_this_command"
