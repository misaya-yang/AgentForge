"""Offline structure tests for the KB development golden candidate (T0-#2/#7).

These are the unit-level teeth behind ``make kb-golden-gate``: the committed
bilingual candidate must stay reproducible from the seed script, must keep
validating against the RAG expectations contract, and the manifest hashes must
match the bytes on disk. This does not establish human review or release
readiness. No network, no live stack.
"""

from __future__ import annotations

import json
from pathlib import Path

import scripts.eval_rag as eval_rag
import scripts.regen_rag_observations as replay
from scripts.seed_kb_golden_set import build_seed_rows, render_golden_jsonl
from src.services.eval.rag_regression import validate_rag_cases

REPO_ROOT = Path(__file__).resolve().parents[2]
GOLDEN_DIR = REPO_ROOT / "tests/fixtures/eval/rag/golden"
KB_GOLDEN = GOLDEN_DIR / "kb_golden_qa_v1.jsonl"
MANIFEST = GOLDEN_DIR / "manifest.json"
REGRESSION_GOLDEN = GOLDEN_DIR / "rag_regression_v1.jsonl"
REGRESSION_OBSERVATIONS = (
    REPO_ROOT / "tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl"
)


def _rows(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def test_committed_golden_set_has_no_seed_drift() -> None:
    """The committed file must be exactly what the seed script renders."""

    assert KB_GOLDEN.read_bytes() == render_golden_jsonl(build_seed_rows())


def test_golden_set_satisfies_rag_expectations_contract() -> None:
    rows = _rows(KB_GOLDEN)
    validation = validate_rag_cases(rows)
    assert validation["valid"] is True, validation["errors"]
    assert len(rows) >= 12


def test_golden_set_is_bilingual_with_cross_lingual_coverage() -> None:
    retrieval_rows = [row for row in _rows(KB_GOLDEN) if row.get("track") == "retrieval_only"]
    assert retrieval_rows
    languages = {str((row.get("metadata") or {}).get("language")) for row in retrieval_rows}
    assert {"en", "zh"} <= languages
    assert any((row.get("metadata") or {}).get("cross_lingual") is True for row in retrieval_rows)
    tracks = {str(row.get("track")) for row in _rows(KB_GOLDEN)}
    assert "answer_aware" in tracks, "starter set lost its answer track; gates depend on it"


def test_manifest_verify_passes_on_committed_files() -> None:
    report = replay.verify_manifest(MANIFEST)
    assert report["valid"] is True, report["entries"]
    assert report["version"]


def test_eval_rag_validate_accepts_expectations_only_mode() -> None:
    """kb-golden-gate validates the KB golden set without observations."""

    assert eval_rag.main(["validate", str(KB_GOLDEN)]) == 0


def test_eval_rag_validate_still_rejects_malformed_expectations(tmp_path: Path) -> None:
    broken = tmp_path / "broken.jsonl"
    broken.write_text('{"case_id":"x","track":"retrieval_only"}\n', encoding="utf-8")
    assert eval_rag.main(["validate", str(broken)]) == 1


def test_eval_rag_track_filter_joins_observation_subsets() -> None:
    """A retrieval-only recording must gate against the full expectations file."""

    assert (
        eval_rag.main(
            [
                "gate",
                str(REGRESSION_GOLDEN),
                "--observations",
                str(REGRESSION_OBSERVATIONS),
                "--track",
                "retrieval_only",
                "--min-recall",
                "0",
                "--min-mrr",
                "0",
                "--min-ndcg",
                "0",
                "--min-total-samples",
                "1",
                "--min-track-samples",
                "1",
                "--output",
                "tmp/eval-e1/rag-track-filter-test.json",
            ]
        )
        == 0
    )
