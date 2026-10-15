"""Offline contract tests for the honest KB T0 release-evidence gate."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from scripts.harness import kb_release_evidence_gate as gate


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")


def _case(index: int, *, kind: str | None = None) -> dict[str, Any]:
    source_kind = kind or ("real" if index < 100 else "synthetic")
    return {
        "case_id": f"case-{index:03d}",
        "track": "retrieval_only",
        "query": f"query {index}",
        "relevance": {f"segment-{index}": 3},
        "metadata": {
            "version": "v1",
            "provenance": {"kind": source_kind, "source_ref": f"source-{index}"},
            "review": {
                "status": "approved",
                "reviewer": "reviewer@example.test",
                "reviewed_at": "2026-08-29T12:00:00Z",
            },
        },
    }


def _release_fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    root = tmp_path
    cases_path = root / "fixtures/golden.jsonl"
    cases = [_case(index) for index in range(200)]
    cases_path.parent.mkdir(parents=True)
    cases_path.write_text(
        "\n".join(json.dumps(row, sort_keys=True) for row in cases) + "\n",
        encoding="utf-8",
    )
    manifest_path = root / "fixtures/manifest.json"
    _write_json(
        manifest_path,
        {
            "version": "v1",
            "files": {"golden.jsonl": {"sha256": _digest(cases_path)}},
        },
    )
    manifest_digest = _digest(manifest_path)
    baseline_path = root / "reports/baseline.json"
    review = {
        "status": "approved",
        "reviewer": "reviewer@example.test",
        "reviewed_at": "2026-08-29T13:00:00Z",
    }
    _write_json(
        baseline_path,
        {
            "release_evidence": {
                "schema_version": gate.BASELINE_SCHEMA,
                "dataset_id": "dataset-real-corpus",
                "golden_version": "v1",
                "golden_manifest_sha256": manifest_digest,
                "review": review,
            },
            "provenance": {
                "expectations": {"sha256": _digest(cases_path)},
                "observations": {"sha256": "a" * 64},
            },
            "retrieval": {
                "all": {
                    "metrics": {
                        "5": {
                            "hit_rate": 0.8,
                            "mrr": 0.7,
                            "ndcg_at_k": 0.75,
                            "recall_at_k": 0.85,
                            "num_queries": 200,
                        }
                    }
                }
            },
        },
    )
    pointer_path = root / "reports/release-pointer.json"
    _write_json(
        pointer_path,
        {
            "schema_version": gate.POINTER_SCHEMA,
            "release_key": "current",
            "golden_version": "v1",
            "golden_manifest": "fixtures/manifest.json",
            "golden_manifest_sha256": manifest_digest,
            "baseline_report": "reports/baseline.json",
            "baseline_report_sha256": _digest(baseline_path),
            "dataset_id": "dataset-real-corpus",
            "review": review,
        },
    )
    return manifest_path, cases_path, pointer_path


def _evaluate(root: Path, paths: tuple[Path, Path, Path]) -> dict[str, Any]:
    manifest, cases, pointer = paths
    return gate.evaluate_release_evidence(
        root=root,
        manifest_path=manifest,
        cases_path=cases,
        pointer_path=pointer,
    )


def _codes(report: dict[str, Any]) -> set[str]:
    return {reason["code"] for reason in report["reasons"]}


def test_complete_reviewed_and_bound_evidence_passes(tmp_path: Path) -> None:
    report = _evaluate(tmp_path, _release_fixture(tmp_path))
    assert report["status"] == "PASS", report["reasons"]
    assert report["source_counts"] == {"real": 100, "synthetic": 100}


def test_current_seed_assets_are_explicitly_blocked() -> None:
    report = gate.evaluate_release_evidence()
    assert report["status"] == "BLOCKED"
    assert report["case_count"] == 18
    assert {
        "case_count_out_of_range",
        "case_provenance_incomplete",
        "case_review_incomplete",
        "release_pointer_missing",
    } <= _codes(report)


def test_manifest_tamper_is_blocked(tmp_path: Path) -> None:
    paths = _release_fixture(tmp_path)
    paths[1].write_text(paths[1].read_text(encoding="utf-8") + "\n", encoding="utf-8")
    report = _evaluate(tmp_path, paths)
    assert report["status"] == "BLOCKED"
    assert "manifest_hash_mismatch" in _codes(report)


def test_missing_cases_fail_closed_without_exception(tmp_path: Path) -> None:
    paths = _release_fixture(tmp_path)
    paths[1].unlink()
    report = _evaluate(tmp_path, paths)
    assert report["status"] == "BLOCKED"
    assert {"manifest_file_missing", "cases_missing", "baseline_expectations_hash"} <= _codes(
        report
    )


def test_unreviewed_case_and_wrong_source_mix_are_blocked(tmp_path: Path) -> None:
    manifest, cases_path, pointer = _release_fixture(tmp_path)
    cases = [_case(index, kind="synthetic" if index < 190 else "real") for index in range(200)]
    cases[0]["metadata"]["review"]["status"] = "pending"
    cases_path.write_text(
        "\n".join(json.dumps(row, sort_keys=True) for row in cases) + "\n",
        encoding="utf-8",
    )
    manifest_payload = json.loads(manifest.read_text(encoding="utf-8"))
    manifest_payload["files"]["golden.jsonl"]["sha256"] = _digest(cases_path)
    _write_json(manifest, manifest_payload)
    report = _evaluate(tmp_path, (manifest, cases_path, pointer))
    assert {"case_review_incomplete", "source_mix_out_of_range"} <= _codes(report)


def test_baseline_review_and_dataset_binding_are_independent_blocks(tmp_path: Path) -> None:
    manifest, cases, pointer = _release_fixture(tmp_path)
    pointer_payload = json.loads(pointer.read_text(encoding="utf-8"))
    baseline = tmp_path / pointer_payload["baseline_report"]
    baseline_payload = json.loads(baseline.read_text(encoding="utf-8"))
    baseline_payload["release_evidence"]["review"]["status"] = "pending"
    baseline_payload["release_evidence"]["dataset_id"] = "different-dataset"
    _write_json(baseline, baseline_payload)
    pointer_payload["baseline_report_sha256"] = _digest(baseline)
    _write_json(pointer, pointer_payload)
    report = _evaluate(tmp_path, (manifest, cases, pointer))
    assert {"baseline_review", "baseline_dataset_binding"} <= _codes(report)


def test_release_pointer_cannot_escape_repository(tmp_path: Path) -> None:
    manifest, cases, pointer = _release_fixture(tmp_path)
    pointer_payload = json.loads(pointer.read_text(encoding="utf-8"))
    pointer_payload["baseline_report"] = "../outside.json"
    _write_json(pointer, pointer_payload)
    report = _evaluate(tmp_path, (manifest, cases, pointer))
    assert "baseline_report_path" in _codes(report)
