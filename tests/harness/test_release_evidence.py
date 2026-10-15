from __future__ import annotations

import copy
import json
from pathlib import Path

import pytest

from scripts.release import release_evidence

ROOT = Path(__file__).resolve().parents[2]


def _matrix() -> dict:
    return json.loads(release_evidence.DEFAULT_MATRIX.read_text(encoding="utf-8"))


def test_checked_in_release_matrix_is_truthful_not_run_and_not_candidate() -> None:
    matrix = _matrix()

    result = release_evidence.validate_release_matrix(ROOT, matrix, level="draft")

    assert result == {"status": "NOT_RUN", "scenarios": 12, "passed": 0, "not_passed": 12}
    assert {scenario["status"] for scenario in matrix["scenarios"]} == {"NOT_RUN"}
    assert not any(scenario["evidence"] for scenario in matrix["scenarios"])
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="blocked by matrix status"):
        release_evidence.validate_release_matrix(ROOT, matrix, level="candidate")


def test_release_matrix_rejects_missing_command_entrypoints() -> None:
    matrix = _matrix()
    matrix["scenarios"][0]["commands"][0] = ["make", "not-a-real-release-target"]

    with pytest.raises(release_evidence.ReleaseEvidenceError, match="unknown Make target"):
        release_evidence.validate_release_matrix(ROOT, matrix, level="draft")

    matrix = _matrix()
    assistant = next(
        scenario for scenario in matrix["scenarios"] if scenario["id"] == "assistant-live-journeys"
    )
    assistant["commands"][-1][-1] = "e2e/not-a-real-live.spec.ts"
    with pytest.raises(
        release_evidence.ReleaseEvidenceError, match="Playwright entrypoint is missing"
    ):
        release_evidence.validate_release_matrix(ROOT, matrix, level="draft")


def test_pass_requires_durable_existing_evidence_and_exact_aggregate(tmp_path: Path) -> None:
    matrix = _matrix()
    (tmp_path / "Makefile").write_text((ROOT / "Makefile").read_text(encoding="utf-8"))
    (tmp_path / "scripts/release").mkdir(parents=True)
    (tmp_path / "scripts/release/version_agreement_gate.py").write_text("", encoding="utf-8")
    (tmp_path / "web/e2e").mkdir(parents=True)
    (tmp_path / "web/package.json").write_text("{}", encoding="utf-8")
    (tmp_path / "web/playwright.live.config.ts").write_text("", encoding="utf-8")
    for name in (
        "assistant-memory",
        "assistant-history",
        "knowledge-workflow",
        "quiz-workflow",
        "site-walkthrough",
    ):
        (tmp_path / f"web/e2e/{name}.spec.ts").write_text("", encoding="utf-8")
    scenario = matrix["scenarios"][0]
    scenario.update({"status": "PASS", "blocker": None, "evidence": []})

    with pytest.raises(release_evidence.ReleaseEvidenceError, match="no durable evidence"):
        release_evidence.validate_release_matrix(tmp_path, matrix, level="draft")

    evidence = tmp_path / "reports/evidence/offline.json"
    evidence.parent.mkdir(parents=True)
    evidence.write_text("{}", encoding="utf-8")
    scenario["evidence"] = ["reports/evidence/offline.json"]
    matrix["release_id"] = "platform-test-release"
    matrix["source_git_sha"] = "a" * 40
    matrix["status"] = "PASS"
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="receipt schema"):
        release_evidence.validate_release_matrix(tmp_path, matrix, level="draft")

    evidence.write_text(
        json.dumps(
            {
                "schema_version": release_evidence.RECEIPT_SCHEMA,
                "gate": "offline-release-suite",
                "release_id": matrix["release_id"],
                "source_git_sha": matrix["source_git_sha"],
                "result": "pass",
                "unexpected_skips": 0,
                "steps": [
                    {
                        "command": command,
                        "exit_code": 0,
                        "skip_markers": 0,
                        "output_sha256": "1" * 64,
                    }
                    for command in scenario["commands"]
                ],
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="aggregate drift"):
        release_evidence.validate_release_matrix(tmp_path, matrix, level="draft")


def test_pass_receipt_must_cover_every_declared_command(tmp_path: Path) -> None:
    matrix = _matrix()
    (tmp_path / "Makefile").write_text((ROOT / "Makefile").read_text(encoding="utf-8"))
    (tmp_path / "scripts/release").mkdir(parents=True)
    (tmp_path / "scripts/release/version_agreement_gate.py").write_text("", encoding="utf-8")
    (tmp_path / "web/e2e").mkdir(parents=True)
    (tmp_path / "web/package.json").write_text("{}", encoding="utf-8")
    (tmp_path / "web/playwright.live.config.ts").write_text("", encoding="utf-8")
    for name in (
        "assistant-memory",
        "assistant-history",
        "knowledge-workflow",
        "quiz-workflow",
        "site-walkthrough",
    ):
        (tmp_path / f"web/e2e/{name}.spec.ts").write_text("", encoding="utf-8")

    scenario = matrix["scenarios"][0]
    scenario.update(
        {
            "status": "PASS",
            "blocker": None,
            "evidence": ["reports/evidence/incomplete.json"],
        }
    )
    matrix["release_id"] = "platform-test-release"
    matrix["source_git_sha"] = "b" * 40
    receipt = tmp_path / scenario["evidence"][0]
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        json.dumps(
            {
                "schema_version": release_evidence.RECEIPT_SCHEMA,
                "gate": "offline-release-suite",
                "release_id": matrix["release_id"],
                "source_git_sha": matrix["source_git_sha"],
                "result": "pass",
                "unexpected_skips": 0,
                "steps": [
                    {
                        "command": scenario["commands"][0],
                        "exit_code": 0,
                        "skip_markers": 0,
                        "output_sha256": "2" * 64,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    with pytest.raises(release_evidence.ReleaseEvidenceError, match="omits declared commands"):
        release_evidence.validate_release_matrix(tmp_path, matrix, level="draft")


def test_not_run_or_blocked_cannot_carry_pass_evidence() -> None:
    matrix = _matrix()
    matrix["scenarios"][0]["evidence"] = ["reports/evidence/stale.json"]

    with pytest.raises(release_evidence.ReleaseEvidenceError, match="must not carry pass evidence"):
        release_evidence.validate_release_matrix(ROOT, matrix, level="draft")


def test_receipt_dry_run_and_skip_cannot_satisfy_candidate() -> None:
    receipt = {
        "schema_version": release_evidence.RECEIPT_SCHEMA,
        "gate": "knowledge",
        "result": "dry-run",
        "unexpected_skips": 0,
        "steps": [],
    }
    assert (
        release_evidence.validate_integration_receipt(receipt, require_pass=False)["result"]
        == "dry-run"
    )
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="zero-skip pass"):
        release_evidence.validate_integration_receipt(receipt, require_pass=True)

    receipt.update(
        {
            "result": "pass",
            "unexpected_skips": 1,
            "steps": [
                {
                    "command": ["make", "knowledge-integration-gate"],
                    "exit_code": 0,
                    "skip_markers": 1,
                    "output_sha256": "1" * 64,
                }
            ],
        }
    )
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="zero-skip pass"):
        release_evidence.validate_integration_receipt(receipt, require_pass=True)


def test_checked_in_retirement_manifest_names_only_marked_non_prd_history() -> None:
    manifest = json.loads(release_evidence.DEFAULT_RETIREMENT.read_text(encoding="utf-8"))

    result = release_evidence.validate_retirement_manifest(ROOT, manifest)

    assert result == {"entries": 7, "superseded": 5, "archived": 2}
    assert all("prd" not in Path(entry["path"]).name.lower() for entry in manifest["entries"])
    tampered = copy.deepcopy(manifest)
    tampered["entries"][0]["document_marker"] = "fabricated PASS marker"
    with pytest.raises(release_evidence.ReleaseEvidenceError, match="marker is absent"):
        release_evidence.validate_retirement_manifest(ROOT, tampered)


def test_release_schema_documents_are_present_and_hashable() -> None:
    digests = release_evidence.validate_schema_documents(ROOT)

    assert set(digests) == {"release_matrix", "integration_receipt", "retirement"}
    assert all(len(value) == 64 for value in digests.values())
