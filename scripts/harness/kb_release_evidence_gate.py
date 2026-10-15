#!/usr/bin/env python3
"""Fail-closed release-evidence gate for the KB RAG T0 contract.

``kb-golden-gate`` deliberately checks only development-fixture structure.  This
gate answers the different release question: is there a reviewed, manifest-bound
200--400 case bilingual set and a reviewed real-corpus retrieval baseline bound
to one dataset?  It is offline and never creates, promotes, or rewrites evidence.

Exit code 0 means PASS.  Missing, pending, malformed, or contradictory evidence
is reported as BLOCKED with a non-zero exit code.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "tests/fixtures/eval/rag/golden/manifest.json"
DEFAULT_CASES = ROOT / "tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl"
DEFAULT_RELEASE_POINTER = ROOT / "reports/kb-eval-baseline/release-pointer.json"

MIN_CASE_COUNT = 200
MAX_CASE_COUNT = 400
MIN_SOURCE_SHARE = 0.40
MAX_SOURCE_SHARE = 0.60
APPROVED = "approved"
POINTER_SCHEMA = "kb-release-evidence/v1"
BASELINE_SCHEMA = "kb-baseline-evidence/v1"
SOURCE_KINDS = ("real", "synthetic")
RETRIEVAL_METRICS = ("hit_rate", "mrr", "ndcg_at_k", "recall_at_k")


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def _load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("top-level JSON value must be an object")
    return value


def _load_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError(f"line {line_number} must be an object")
        rows.append(row)
    return rows


def _repo_path(root: Path, value: Any, *, label: str) -> Path:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{label} must be a non-empty repository-relative path")
    if Path(value).is_absolute():
        raise ValueError(f"{label} must be repository-relative")
    path = (root / value).resolve()
    try:
        path.relative_to(root.resolve())
    except ValueError as exc:
        raise ValueError(f"{label} escapes the repository root") from exc
    return path


def _review_errors(review: Any, *, label: str) -> list[str]:
    if not isinstance(review, dict):
        return [f"{label} review must be an object"]
    errors: list[str] = []
    if review.get("status") != APPROVED:
        errors.append(f"{label} review.status must be {APPROVED!r}")
    for field in ("reviewer", "reviewed_at"):
        value = review.get(field)
        if not isinstance(value, str) or not value.strip():
            errors.append(f"{label} review.{field} must be non-empty")
    return errors


def _reason(code: str, message: str) -> dict[str, str]:
    return {"code": code, "message": message}


def _validate_manifest(
    root: Path,
    manifest_path: Path,
    cases_path: Path,
) -> tuple[dict[str, Any] | None, str | None, list[dict[str, str]]]:
    reasons: list[dict[str, str]] = []
    if not manifest_path.is_file():
        return None, None, [_reason("manifest_missing", f"manifest not found: {manifest_path}")]
    try:
        manifest = _load_json(manifest_path)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        return None, None, [_reason("manifest_invalid", f"cannot read manifest: {exc}")]

    manifest_digest = _sha256(manifest_path)
    version = manifest.get("version")
    if not isinstance(version, str) or not version.strip():
        reasons.append(_reason("manifest_version_missing", "manifest.version must be non-empty"))
    files = manifest.get("files")
    if not isinstance(files, dict) or not files:
        reasons.append(_reason("manifest_files_missing", "manifest.files must be non-empty"))
        return manifest, manifest_digest, reasons

    cases_resolved = cases_path.resolve()
    cases_manifest_key: str | None = None
    for relative_name, spec in sorted(files.items()):
        if not isinstance(relative_name, str) or not isinstance(spec, dict):
            reasons.append(_reason("manifest_entry_invalid", f"invalid manifest entry {relative_name!r}"))
            continue
        expected_digest = spec.get("sha256")
        if not _is_sha256(expected_digest):
            reasons.append(
                _reason(
                    "manifest_digest_invalid",
                    f"manifest entry {relative_name!r} has no lowercase SHA-256 digest",
                )
            )
            continue
        target = (manifest_path.parent / relative_name).resolve()
        try:
            target.relative_to(root.resolve())
        except ValueError:
            reasons.append(
                _reason(
                    "manifest_path_escape",
                    f"manifest entry {relative_name!r} escapes the repository root",
                )
            )
            continue
        if target == cases_resolved:
            cases_manifest_key = relative_name
        if not target.is_file():
            reasons.append(
                _reason("manifest_file_missing", f"manifest entry {relative_name!r} is missing")
            )
            continue
        actual_digest = _sha256(target)
        if actual_digest != expected_digest:
            reasons.append(
                _reason("manifest_hash_mismatch", f"manifest hash mismatch for {relative_name!r}")
            )
    if cases_manifest_key is None:
        reasons.append(
            _reason("cases_not_manifested", f"golden cases file is not listed in {manifest_path}")
        )
    return manifest, manifest_digest, reasons


def _validate_cases(
    cases_path: Path,
    manifest_version: str | None,
) -> tuple[list[dict[str, Any]], dict[str, int], list[dict[str, str]]]:
    reasons: list[dict[str, str]] = []
    if not cases_path.is_file():
        return [], {}, [_reason("cases_missing", f"golden cases not found: {cases_path}")]
    try:
        cases = _load_jsonl(cases_path)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        return [], {}, [_reason("cases_invalid", f"cannot read golden cases: {exc}")]

    case_count = len(cases)
    if not MIN_CASE_COUNT <= case_count <= MAX_CASE_COUNT:
        reasons.append(
            _reason(
                "case_count_out_of_range",
                f"case_count={case_count}; release requires {MIN_CASE_COUNT}..{MAX_CASE_COUNT}",
            )
        )

    case_ids = [case.get("case_id") for case in cases]
    invalid_case_ids = [str(index) for index, case_id in enumerate(case_ids, 1) if not case_id]
    if invalid_case_ids:
        reasons.append(
            _reason("case_id_missing", f"rows without case_id: {', '.join(invalid_case_ids[:10])}")
        )
    case_id_counts: dict[str, int] = {}
    for case_id in case_ids:
        if case_id:
            normalized = str(case_id)
            case_id_counts[normalized] = case_id_counts.get(normalized, 0) + 1
    duplicate_ids = sorted(case_id for case_id, count in case_id_counts.items() if count > 1)
    if duplicate_ids:
        reasons.append(
            _reason("duplicate_case_ids", f"duplicate case_ids: {', '.join(duplicate_ids[:10])}")
        )

    source_counts = {"real": 0, "synthetic": 0}
    provenance_failures: list[str] = []
    review_failures: list[str] = []
    version_failures: list[str] = []
    for index, case in enumerate(cases, 1):
        case_id = str(case.get("case_id") or f"row-{index}")
        metadata = case.get("metadata")
        if not isinstance(metadata, dict):
            provenance_failures.append(case_id)
            review_failures.append(case_id)
            version_failures.append(case_id)
            continue
        provenance = metadata.get("provenance")
        if not isinstance(provenance, dict):
            provenance_failures.append(case_id)
        else:
            kind = provenance.get("kind")
            source_ref = provenance.get("source_ref")
            if kind not in SOURCE_KINDS or not isinstance(source_ref, str) or not source_ref.strip():
                provenance_failures.append(case_id)
            else:
                source_counts[str(kind)] += 1
        if _review_errors(metadata.get("review"), label=case_id):
            review_failures.append(case_id)
        if manifest_version and metadata.get("version") != manifest_version:
            version_failures.append(case_id)

    if provenance_failures:
        reasons.append(
            _reason(
                "case_provenance_incomplete",
                "every case requires metadata.provenance.kind=real|synthetic and a non-empty "
                f"source_ref; invalid: {', '.join(provenance_failures[:10])}"
                + (f" (+{len(provenance_failures) - 10} more)" if len(provenance_failures) > 10 else ""),
            )
        )
    if review_failures:
        reasons.append(
            _reason(
                "case_review_incomplete",
                "every case requires approved metadata.review with reviewer/reviewed_at; invalid: "
                f"{', '.join(review_failures[:10])}"
                + (f" (+{len(review_failures) - 10} more)" if len(review_failures) > 10 else ""),
            )
        )
    if version_failures:
        reasons.append(
            _reason(
                "case_version_mismatch",
                f"case metadata.version must match manifest.version={manifest_version!r}; invalid: "
                f"{', '.join(version_failures[:10])}"
                + (f" (+{len(version_failures) - 10} more)" if len(version_failures) > 10 else ""),
            )
        )

    valid_sources = sum(source_counts.values())
    if valid_sources != case_count:
        reasons.append(
            _reason(
                "source_mix_unverifiable",
                f"only {valid_sources}/{case_count} cases have valid provenance source kinds",
            )
        )
    elif case_count:
        real_share = source_counts["real"] / case_count
        if not MIN_SOURCE_SHARE <= real_share <= MAX_SOURCE_SHARE:
            reasons.append(
                _reason(
                    "source_mix_out_of_range",
                    f"real={source_counts['real']}, synthetic={source_counts['synthetic']}; "
                    f"real share {real_share:.1%} must be {MIN_SOURCE_SHARE:.0%}..{MAX_SOURCE_SHARE:.0%}",
                )
            )
    return cases, source_counts, reasons


def _validate_retrieval_distribution(
    baseline: dict[str, Any],
    expected_queries: int,
) -> list[dict[str, str]]:
    reasons: list[dict[str, str]] = []
    retrieval = baseline.get("retrieval")
    all_results = retrieval.get("all") if isinstance(retrieval, dict) else None
    metrics = all_results.get("metrics") if isinstance(all_results, dict) else None
    if not isinstance(metrics, dict) or not metrics:
        return [
            _reason(
                "baseline_distribution_missing",
                "baseline must contain retrieval.all.metrics distributions",
            )
        ]
    query_counts: list[int] = []
    for k_value, values in metrics.items():
        if not isinstance(values, dict):
            reasons.append(
                _reason("baseline_metrics_invalid", f"retrieval metrics at k={k_value} is not an object")
            )
            continue
        missing = [metric for metric in RETRIEVAL_METRICS if metric not in values]
        if missing:
            reasons.append(
                _reason(
                    "baseline_metrics_incomplete",
                    f"retrieval metrics at k={k_value} missing {', '.join(missing)}",
                )
            )
        invalid_metrics = [
            metric
            for metric in RETRIEVAL_METRICS
            if metric in values
            and (
                not isinstance(values[metric], (int, float))
                or isinstance(values[metric], bool)
                or not math.isfinite(values[metric])
                or not 0 <= values[metric] <= 1
            )
        ]
        if invalid_metrics:
            reasons.append(
                _reason(
                    "baseline_metrics_invalid",
                    f"retrieval metrics at k={k_value} must be finite numbers in [0, 1]: "
                    f"{', '.join(invalid_metrics)}",
                )
            )
        num_queries = values.get("num_queries")
        if isinstance(num_queries, int) and not isinstance(num_queries, bool):
            query_counts.append(num_queries)
    if not query_counts or max(query_counts) != expected_queries:
        reasons.append(
            _reason(
                "baseline_case_binding_mismatch",
                f"baseline num_queries must equal retrieval case count {expected_queries}; "
                f"observed {sorted(set(query_counts)) or 'none'}",
            )
        )
    return reasons


def _validate_pointer_and_baseline(
    root: Path,
    pointer_path: Path,
    manifest_path: Path,
    manifest: dict[str, Any] | None,
    manifest_digest: str | None,
    cases_path: Path,
    cases: list[dict[str, Any]],
) -> tuple[str | None, list[dict[str, str]]]:
    reasons: list[dict[str, str]] = []
    if not pointer_path.is_file():
        return None, [
            _reason("release_pointer_missing", f"release pointer not found: {pointer_path}")
        ]
    try:
        pointer = _load_json(pointer_path)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        return None, [_reason("release_pointer_invalid", f"cannot read release pointer: {exc}")]

    if pointer.get("schema_version") != POINTER_SCHEMA:
        reasons.append(
            _reason("release_pointer_schema", f"schema_version must be {POINTER_SCHEMA!r}")
        )
    release_key = pointer.get("release_key")
    if not isinstance(release_key, str) or not release_key.strip():
        reasons.append(
            _reason("release_pointer_key", "release pointer release_key must be non-empty")
        )
    for error in _review_errors(pointer.get("review"), label="release pointer"):
        reasons.append(_reason("release_pointer_review", error))
    dataset_id = pointer.get("dataset_id")
    if not isinstance(dataset_id, str) or not dataset_id.strip():
        reasons.append(
            _reason("release_pointer_dataset", "release pointer dataset_id must be non-empty")
        )
        dataset_id = None
    if manifest and pointer.get("golden_version") != manifest.get("version"):
        reasons.append(
            _reason("release_pointer_version", "release pointer golden_version mismatches manifest")
        )
    if manifest_digest and pointer.get("golden_manifest_sha256") != manifest_digest:
        reasons.append(
            _reason("release_pointer_manifest_hash", "release pointer manifest SHA-256 mismatches")
        )
    try:
        pointer_manifest = _repo_path(root, pointer.get("golden_manifest"), label="golden_manifest")
        if pointer_manifest != manifest_path.resolve():
            reasons.append(
                _reason("release_pointer_manifest", "release pointer cites a different manifest")
            )
    except ValueError as exc:
        reasons.append(_reason("release_pointer_manifest", str(exc)))

    try:
        baseline_path = _repo_path(root, pointer.get("baseline_report"), label="baseline_report")
    except ValueError as exc:
        reasons.append(_reason("baseline_report_path", str(exc)))
        return dataset_id, reasons
    if not baseline_path.is_file():
        reasons.append(
            _reason("baseline_report_missing", f"baseline report not found: {baseline_path}")
        )
        return dataset_id, reasons
    baseline_digest = _sha256(baseline_path)
    if pointer.get("baseline_report_sha256") != baseline_digest:
        reasons.append(
            _reason("baseline_report_hash", "release pointer baseline report SHA-256 mismatches")
        )
    try:
        baseline = _load_json(baseline_path)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        reasons.append(_reason("baseline_report_invalid", f"cannot read baseline report: {exc}"))
        return dataset_id, reasons

    evidence = baseline.get("release_evidence")
    if not isinstance(evidence, dict):
        reasons.append(
            _reason(
                "baseline_release_evidence_missing",
                "baseline requires a release_evidence object",
            )
        )
        return dataset_id, reasons
    if evidence.get("schema_version") != BASELINE_SCHEMA:
        reasons.append(
            _reason("baseline_schema", f"baseline schema_version must be {BASELINE_SCHEMA!r}")
        )
    for error in _review_errors(evidence.get("review"), label="baseline"):
        reasons.append(_reason("baseline_review", error))
    if evidence.get("dataset_id") != dataset_id:
        reasons.append(
            _reason("baseline_dataset_binding", "baseline dataset_id mismatches release pointer")
        )
    if manifest and evidence.get("golden_version") != manifest.get("version"):
        reasons.append(_reason("baseline_version", "baseline golden_version mismatches manifest"))
    if manifest_digest and evidence.get("golden_manifest_sha256") != manifest_digest:
        reasons.append(
            _reason("baseline_manifest_hash", "baseline manifest SHA-256 mismatches")
        )

    provenance = baseline.get("provenance")
    expectations = provenance.get("expectations") if isinstance(provenance, dict) else None
    observations = provenance.get("observations") if isinstance(provenance, dict) else None
    cases_digest = _sha256(cases_path) if cases_path.is_file() else None
    if (
        cases_digest is None
        or not isinstance(expectations, dict)
        or expectations.get("sha256") != cases_digest
    ):
        reasons.append(
            _reason("baseline_expectations_hash", "baseline expectations hash mismatches cases")
        )
    if not isinstance(observations, dict) or not _is_sha256(observations.get("sha256")):
        reasons.append(
            _reason("baseline_observations_hash", "baseline observations SHA-256 is missing")
        )
    retrieval_case_count = sum(case.get("track") == "retrieval_only" for case in cases)
    reasons.extend(_validate_retrieval_distribution(baseline, retrieval_case_count))
    return dataset_id, reasons


def evaluate_release_evidence(
    *,
    root: Path = ROOT,
    manifest_path: Path = DEFAULT_MANIFEST,
    cases_path: Path = DEFAULT_CASES,
    pointer_path: Path = DEFAULT_RELEASE_POINTER,
) -> dict[str, Any]:
    """Evaluate all independently checkable T0 release evidence without mutation."""

    manifest, manifest_digest, manifest_reasons = _validate_manifest(
        root, manifest_path, cases_path
    )
    manifest_version = manifest.get("version") if isinstance(manifest, dict) else None
    cases, source_counts, case_reasons = _validate_cases(cases_path, manifest_version)
    dataset_id, release_reasons = _validate_pointer_and_baseline(
        root,
        pointer_path,
        manifest_path,
        manifest,
        manifest_digest,
        cases_path,
        cases,
    )
    reasons = [*manifest_reasons, *case_reasons, *release_reasons]
    return {
        "status": "PASS" if not reasons else "BLOCKED",
        "case_count": len(cases),
        "source_counts": source_counts,
        "manifest": str(manifest_path),
        "manifest_sha256": manifest_digest,
        "release_pointer": str(pointer_path),
        "dataset_id": dataset_id,
        "reasons": reasons,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--cases", type=Path, default=DEFAULT_CASES)
    parser.add_argument("--release-pointer", type=Path, default=DEFAULT_RELEASE_POINTER)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    report = evaluate_release_evidence(
        root=args.repo_root.resolve(),
        manifest_path=args.manifest.resolve(),
        cases_path=args.cases.resolve(),
        pointer_path=args.release_pointer.resolve(),
    )
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
