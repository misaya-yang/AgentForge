"""Offline tests for the golden-set Postgres import path (PRD T0-#2 close-out).

Unit teeth for scripts/import_kb_eval_golden.py and the pure row-mapping half
of knowledge_service.persistence.kb_eval_golden_store.  No database here: the
live import/upsert/promotion behaviour is covered tier-b in
tests/database/test_kb_eval_golden_migration.py.  What must hold offline:

* the committed golden JSONL imports under its own metadata version and lands
  entirely in the growth split (frozen promotion is review-gated, never an
  import side effect);
* the importer is fail-closed: contract-invalid rows and version conflicts
  exit non-zero without ever reaching a connection;
* case_to_row/row_to_case round-trip a case losslessly through the split
  promoted into metadata.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from knowledge_service.persistence.kb_eval_golden_store import (
    GOLDEN_SPLITS,
    GoldenStoreError,
    case_to_row,
    row_to_case,
)

import scripts.import_kb_eval_golden as importer
from scripts.seed_kb_golden_set import SEED_VERSION, build_seed_rows

REPO_ROOT = Path(__file__).resolve().parents[2]
KB_GOLDEN = REPO_ROOT / "tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl"


def _valid_case(**overrides: object) -> dict:
    case = {
        "case_id": "kb.demo.retrieval.en",
        "track": "retrieval_only",
        "query": "how do refunds work",
        "relevance": {"seg-a": 3, "seg-b": 1},
        "metadata": {"version": "v-test", "provenance": "hand-written"},
    }
    case.update(overrides)
    return case


def _plan(stdout: str) -> dict:
    return json.loads(stdout.strip().splitlines()[-1])


def test_dry_run_of_committed_golden_set_succeeds(capsys: pytest.CaptureFixture[str]) -> None:
    code = importer.main([str(KB_GOLDEN), "--dry-run"])
    assert code == 0
    plan = _plan(capsys.readouterr().out)
    assert plan["version"] == SEED_VERSION
    assert plan["cases"] == len(build_seed_rows())
    # Nothing is promoted to frozen by an import alone.
    assert plan["frozen"] == 0
    assert plan["growth"] == plan["cases"]
    assert plan["mode"] == "dry-run"


def test_invalid_json_is_refused_without_a_database(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    bad = tmp_path / "bad.jsonl"
    case = _valid_case(track="not_a_track")
    bad.write_text(json.dumps(case) + "\n", encoding="utf-8")
    assert importer.main([str(bad), "--dry-run"]) == 2
    assert "invalid golden case" in capsys.readouterr().err


def test_unparseable_line_is_refused(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    bad = tmp_path / "broken.jsonl"
    bad.write_text("{not json}\n", encoding="utf-8")
    assert importer.main([str(bad), "--dry-run"]) == 2
    assert "golden import refused" in capsys.readouterr().err


def test_version_conflict_between_rows_and_flag_is_fatal(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "mixed.jsonl"
    a = _valid_case(case_id="kb.a")
    b = _valid_case(case_id="kb.b", metadata={"version": "other"})
    path.write_text(json.dumps(a) + "\n" + json.dumps(b) + "\n", encoding="utf-8")

    assert importer.main([str(path), "--dry-run"]) == 2
    assert "distinct metadata versions" in capsys.readouterr().err

    capsys.readouterr()
    assert importer.main([str(path), "--dry-run", "--version", "v-test"]) == 2
    assert "contradicts row metadata versions" in capsys.readouterr().err


def test_explicit_version_wins_for_uniform_rows(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "one.jsonl"
    path.write_text(json.dumps(_valid_case()) + "\n", encoding="utf-8")
    assert importer.main([str(path), "--dry-run", "--version", "v-test"]) == 0
    assert _plan(capsys.readouterr().out)["version"] == "v-test"


def test_case_to_row_promotes_split_and_provenance() -> None:
    row = case_to_row(_valid_case(), version="v1", default_split="growth")
    assert row["version"] == "v1"
    assert row["split"] == "growth"
    assert row["provenance"] == "hand-written"
    assert row["relevance"] == {"seg-a": 3, "seg-b": 1}
    assert row["reference_answer"] is None

    # metadata.split overrides the default; the explicit provenance argument
    # overrides metadata.
    override = case_to_row(
        _valid_case(metadata={"version": "v1", "split": "frozen"}),
        version="v1",
        default_split="growth",
        provenance="reviewed-2026-08-28",
    )
    assert override["split"] == "frozen"
    assert override["provenance"] == "reviewed-2026-08-28"


def test_case_to_row_rejects_contract_violations() -> None:
    with pytest.raises(ValueError, match="track"):
        case_to_row(_valid_case(track="bogus"), version="v1")
    with pytest.raises(ValueError, match="query"):
        case_to_row(_valid_case(query="   "), version="v1")
    with pytest.raises(ValueError, match="relevance"):
        case_to_row(_valid_case(relevance={}), version="v1")
    with pytest.raises(ValueError, match="relevance grades"):
        case_to_row(_valid_case(relevance={"seg-a": "high"}), version="v1")
    with pytest.raises(ValueError, match="split"):
        case_to_row(_valid_case(metadata={"split": "eternal"}), version="v1")
    with pytest.raises(ValueError, match="version"):
        case_to_row(_valid_case(), version="  ")


def test_row_to_case_round_trips_through_the_promoted_columns() -> None:
    case = _valid_case()
    row = case_to_row(case, version="v1", default_split="growth")
    # Simulated asyncpg record: JSONB comes back as text.
    record = {
        **row,
        "relevance": json.dumps(row["relevance"]),
        "metadata": json.dumps(row["metadata"]),
    }
    restored = row_to_case(record)
    assert restored["case_id"] == case["case_id"]
    assert restored["track"] == case["track"]
    assert restored["query"] == case["query"]
    assert restored["relevance"] == case["relevance"]
    assert restored["metadata"]["split"] == "growth"
    assert restored["metadata"]["provenance"] == "hand-written"
    # The restored case revalidates against the same top-level allow-list.
    assert set(restored) <= {"case_id", "track", "query", "relevance", "reference_answer", "metadata"}


def test_answer_aware_reference_answer_round_trips() -> None:
    case = _valid_case(track="answer_aware", reference_answer="Within the window.")
    row = case_to_row(case, version="v1")
    assert row["reference_answer"] == "Within the window."
    assert row_to_case(row)["reference_answer"] == "Within the window."


def test_store_inputs_are_validated_before_any_connection(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    # set_split on an empty id list and unknown splits fail in pure Python.
    with pytest.raises(ValueError, match="split"):
        case_to_row(_valid_case(), version="v1", default_split="nope")
    assert frozenset({"frozen", "growth"}) == GOLDEN_SPLITS
    assert GoldenStoreError.__mro__[1] is RuntimeError


def test_missing_file_is_refused(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert importer.main([str(tmp_path / "gone.jsonl"), "--dry-run"]) == 2
    assert "golden import refused" in capsys.readouterr().err
