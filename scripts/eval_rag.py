#!/usr/bin/env python3
"""Run the deterministic two-track RAG Eval E1 gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

from src.services.eval.rag_regression import (
    evaluate_rag_regression,
    load_resolved_judge_policy,
    validate_rag_cases,
    validate_rag_observations,
    write_rag_report,
)


def _load_jsonl_snapshot(path: str) -> tuple[list[dict], str]:
    """Parse and hash the same immutable byte snapshot."""

    raw = Path(path).read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValueError(f"Invalid UTF-8 JSONL: {path}") from exc
    rows: list[dict] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"Invalid JSONL at line {line_no}: {exc}") from exc
        if not isinstance(row, dict):
            raise ValueError(f"JSONL row {line_no} must be an object")
        rows.append(row)
    return rows, digest


def _observation_map(rows: list[dict]) -> dict[str, dict]:
    observations: dict[str, dict] = {}
    for line_no, row in enumerate(rows, start=1):
        case_id = row.get("case_id")
        replay = row.get("replay")
        if not isinstance(case_id, str) or not case_id:
            raise ValueError(f"Observation at line {line_no} requires case_id")
        if case_id in observations:
            raise ValueError(f"Duplicate observation case_id {case_id!r}")
        if not isinstance(replay, dict) or not replay:
            raise ValueError(f"Observation {case_id!r} requires replay")
        observations[case_id] = replay
    return observations


def _print(payload: dict[str, object]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def _fixture_validation(
    args: argparse.Namespace,
) -> tuple[list[dict], dict[str, dict], dict, dict[str, str]]:
    cases, expectations_sha256 = _load_jsonl_snapshot(args.expectations)
    track = getattr(args, "track", None)
    if track:
        cases = [case for case in cases if case.get("track") == track]
        if not cases:
            raise ValueError(f"expectations contain no {track!r}-track cases")
    hashes = {"expectations": expectations_sha256, "observations": None}
    if getattr(args, "observations", None):
        observation_rows, observations_sha256 = _load_jsonl_snapshot(args.observations)
        observations = _observation_map(observation_rows)
        if track:
            case_ids = {str(case.get("case_id")) for case in cases}
            observations = {
                case_id: replay for case_id, replay in observations.items() if case_id in case_ids
            }
        hashes["observations"] = observations_sha256
    else:
        observations = {}
    case_validation = validate_rag_cases(cases)
    if getattr(args, "observations", None):
        observation_validation = validate_rag_observations(cases, observations)
        validation = {
            "valid": case_validation["valid"] and observation_validation["valid"],
            "expectations": case_validation,
            "observations": observation_validation,
        }
    else:
        # Expectations-only mode (kb-golden-gate): the KB golden set ships
        # without observations because its live evidence is recorded per
        # dataset binding by scripts/regen_rag_observations.py.
        validation = {
            "valid": case_validation["valid"],
            "expectations": case_validation,
            "observations": {"valid": True, "skipped": "no --observations given"},
        }
    return cases, observations, validation, hashes


def cmd_validate(args: argparse.Namespace) -> int:
    _cases, _observations, validation, _hashes = _fixture_validation(args)
    _print(validation)
    return 0 if validation["valid"] else 1


def cmd_gate(args: argparse.Namespace) -> int:
    cases, observations, validation, fixture_hashes = _fixture_validation(args)
    if not validation["valid"]:
        _print(validation)
        return 1
    judge_rows = None
    judge_artifact_sha256 = None
    if args.judge_observations:
        judge_rows, judge_artifact_sha256 = _load_jsonl_snapshot(args.judge_observations)
    judge_policy = (
        load_resolved_judge_policy(args.judge_policy_manifest)
        if args.judge_policy_manifest
        else None
    )
    k_values = [int(item.strip()) for item in args.k_values.split(",") if item.strip()]
    # A --track join is single-track by construction; enforce the per-track
    # minimum on the selected track only. Sample sufficiency for the join
    # still holds via min_total_samples over the selected subset.
    required_tracks = [args.track] if getattr(args, "track", None) else None
    result = evaluate_rag_regression(
        cases,
        observations,
        k_values=k_values,
        retrieval_thresholds={
            "recall_at_k": args.min_recall,
            "mrr": args.min_mrr,
            "ndcg_at_k": args.min_ndcg,
        },
        min_total_samples=args.min_total_samples,
        min_track_samples=args.min_track_samples,
        required_tracks=required_tracks,
        bootstrap_samples=args.bootstrap_samples,
        enable_external_judge_gate=args.enable_external_judge_gate,
        judge_rows=judge_rows,
        judge_policy=judge_policy,
        answer_thresholds={
            "faithfulness": args.min_faithfulness,
            "response_relevancy": args.min_response_relevancy,
        },
        min_answer_samples=args.min_answer_samples,
    )
    result["provenance"] = {
        "expectations": {
            "source": str(Path(args.expectations)),
            "sha256": fixture_hashes["expectations"],
        },
        "observations": {
            "source": str(Path(args.observations)),
            "sha256": fixture_hashes["observations"],
        },
        "judge_observations": (
            {
                "source": str(Path(args.judge_observations)),
                "raw_source_sha256": judge_artifact_sha256,
                "artifact_binding": result["answer_quality"].get("artifact_binding"),
            }
            if args.judge_observations
            else None
        ),
        "judge_policy": result["answer_quality"].get("policy"),
        "bootstrap": {"seed": 42, "samples": args.bootstrap_samples},
        "provider_execution": "not_performed_by_this_command",
    }
    write_rag_report(result, args.output)
    _print(result)
    return 0 if result["gate"]["status"] == "pass" else 1


def _add_fixture_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("expectations")
    parser.add_argument("--observations", required=True)
    parser.add_argument(
        "--track",
        choices=("retrieval_only", "answer_aware"),
        default=None,
        help="restrict the join to one track (e.g. record-only baseline runs that "
        "only regenerated retrieval evidence)",
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run deterministic RAG Eval E1 gates.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate")
    validate.add_argument("expectations")
    validate.add_argument(
        "--observations",
        default=None,
        help="omit to validate expectations only (used by kb-golden-gate)",
    )
    validate.add_argument("--track", choices=("retrieval_only", "answer_aware"), default=None)
    validate.set_defaults(func=cmd_validate)

    gate = subparsers.add_parser("gate")
    _add_fixture_arguments(gate)
    gate.add_argument("--output", default="tmp/eval-e1/rag-latest.json")
    gate.add_argument("--k-values", default="1,3,5")
    gate.add_argument("--min-total-samples", type=int, default=8)
    gate.add_argument("--min-track-samples", type=int, default=4)
    gate.add_argument("--bootstrap-samples", type=int, default=2_000)
    gate.add_argument("--min-recall", type=float, default=0.8)
    gate.add_argument("--min-mrr", type=float, default=0.8)
    gate.add_argument("--min-ndcg", type=float, default=0.8)
    gate.add_argument("--judge-observations")
    gate.add_argument("--judge-policy-manifest")
    gate.add_argument("--enable-external-judge-gate", action="store_true")
    gate.add_argument("--min-answer-samples", type=int, default=4)
    gate.add_argument("--min-faithfulness", type=float, default=0.7)
    gate.add_argument("--min-response-relevancy", type=float, default=0.7)
    gate.set_defaults(func=cmd_gate)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.func(args))
    except Exception as exc:  # noqa: BLE001 - CLI must fail closed with concise evidence
        print(f"eval_rag failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
