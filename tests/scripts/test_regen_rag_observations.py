"""Unit tests for the golden manifest / observation replay machinery (no network)."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

import pytest

import scripts.regen_rag_observations as replay
from scripts.seed_kb_golden_set import (
    SEED_VERSION,
    build_eval_import_payload,
    build_seed_rows,
    render_golden_jsonl,
)
from src.services.eval.golden_validation import validate_case
from src.services.eval.rag_regression import validate_rag_cases, validate_rag_observations

RETRIEVAL_CASE: dict[str, Any] = {
    "case_id": "kb.seed.demo.retrieval.en",
    "track": "retrieval_only",
    "query": "demo refund policy",
    "relevance": {"demo-primary": 3, "demo-window": 1},
}


def _payload(*segment_ids: str) -> dict[str, Any]:
    return {
        "results": [{"segment_id": segment_id, "score": 0.5} for segment_id in segment_ids],
        "metadata": {"strategy": "demo"},
    }


def test_build_retrieval_replay_conforms_to_validator() -> None:
    replay_obj = replay.build_retrieval_replay(RETRIEVAL_CASE, _payload("demo-primary", "demo-window"))
    assert replay_obj == {
        "status": "succeeded",
        "ranked_segment_ids": ["demo-primary", "demo-window"],
        "answer_source": "retrieval_only",
        "answer": None,
    }
    joined = {RETRIEVAL_CASE["case_id"]: replay_obj}
    assert validate_rag_observations([RETRIEVAL_CASE], joined)["valid"] is True


@pytest.mark.parametrize(
    "payload",
    [
        {"results": []},
        {"results": "not-a-list"},
        {},
        [1, 2],
        {"results": [{"document_id": "no-segment"}]},
        {"results": [{"segment_id": "  "}]},
    ],
)
def test_build_retrieval_replay_fails_closed(payload: Any) -> None:
    with pytest.raises(replay.RecordingError):
        replay.build_retrieval_replay(RETRIEVAL_CASE, payload)


def test_record_observations_preserves_expectation_order_and_shape() -> None:
    second = {**RETRIEVAL_CASE, "case_id": "kb.seed.demo.retrieval.zh", "query": "演示退款"}
    cases = [RETRIEVAL_CASE, second]
    bindings = {str(case["case_id"]): "ds-1" for case in cases}

    def fetch(dataset_id: str, query: str, top_k: int) -> dict[str, Any]:
        assert dataset_id == "ds-1" and top_k == 7
        return _payload("seg-a", "seg-b") if query == "demo refund policy" else _payload("段-丙")

    rows = replay.record_observations(cases, bindings, fetch, top_k=7)
    assert [row["case_id"] for row in rows] == [
        "kb.seed.demo.retrieval.en",
        "kb.seed.demo.retrieval.zh",
    ]
    encoded = replay.serialize_jsonl(rows)
    assert encoded.endswith(b"\n")
    lines = encoded.decode("utf-8").rstrip("\n").split("\n")
    assert lines[0] == json.dumps(rows[0], ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    assert "段-丙" in lines[1]  # ensure_ascii=False: CJK ids stay raw UTF-8


def test_resolve_dataset_bindings() -> None:
    bound = {**RETRIEVAL_CASE, "metadata": {"dataset_id": " ds-9 "}}
    assert replay.resolve_dataset_bindings([bound], override=None) == {
        "kb.seed.demo.retrieval.en": "ds-9"
    }
    assert replay.resolve_dataset_bindings([RETRIEVAL_CASE], override="ds-flag") == {
        "kb.seed.demo.retrieval.en": "ds-flag"
    }
    with pytest.raises(replay.RecordingError, match="kb.seed.demo.retrieval.en"):
        replay.resolve_dataset_bindings([RETRIEVAL_CASE], override=None)


def test_select_recordable_cases_gates_answer_track() -> None:
    answer_case = {
        "case_id": "rag.answer.demo",
        "track": "answer_aware",
        "query": "demo",
        "relevance": {"a": 3},
    }
    with pytest.raises(replay.RecordingError, match="--retrieval-only"):
        replay.select_recordable_cases([RETRIEVAL_CASE, answer_case], retrieval_only=False)
    selected = replay.select_recordable_cases(
        [RETRIEVAL_CASE, answer_case], retrieval_only=True
    )
    assert [str(case["case_id"]) for case in selected] == [RETRIEVAL_CASE["case_id"]]
    with pytest.raises(replay.RecordingError, match="zero cases"):
        replay.select_recordable_cases([answer_case], retrieval_only=True)


def _write_manifest(tmp_path: Path, files: dict[str, dict[str, str]]) -> Path:
    manifest = tmp_path / "manifest.json"
    manifest.write_text(
        json.dumps({"version": "test.1", "frozen_at": "2026-08-28", "files": files}),
        encoding="utf-8",
    )
    return manifest


def test_verify_manifest_ok_mismatch_and_missing(tmp_path: Path) -> None:
    golden = tmp_path / "golden"
    golden.mkdir()
    observed = tmp_path / "observed.jsonl"
    observed.write_text("row\n", encoding="utf-8")
    digest = hashlib.sha256(observed.read_bytes()).hexdigest()
    manifest = _write_manifest(
        tmp_path / "golden",
        {"../observed.jsonl": {"sha256": digest, "purpose": "test"}},
    )
    assert manifest == tmp_path / "golden" / "manifest.json"
    report = replay.verify_manifest(manifest)
    assert report["valid"] is True and report["entries"][0]["status"] == "ok"

    observed.write_text("row tampered\n", encoding="utf-8")
    report = replay.verify_manifest(manifest)
    assert report["valid"] is False and report["entries"][0]["status"] == "mismatch"

    observed.unlink()
    report = replay.verify_manifest(manifest)
    assert report["valid"] is False and report["entries"][0]["status"] == "missing"

    _write_manifest(tmp_path, {"observed.jsonl": {"sha256": "not-hex"}})
    with pytest.raises(replay.RecordingError, match="sha256"):
        replay.verify_manifest(tmp_path / "manifest.json")


def test_verify_manifest_passes_on_repo_golden_files() -> None:
    report = replay.verify_manifest(replay.DEFAULT_MANIFEST)
    assert report["valid"] is True, report
    listed = {str(entry["file"]) for entry in report["entries"]}
    assert listed == {
        "rag_regression_v1.jsonl",
        "../observations/rag_regression_v1.jsonl",
        "kb_golden_qa_v1.jsonl",
    }


def test_verify_cli_exit_codes(tmp_path: Path) -> None:
    observed = tmp_path / "observed.jsonl"
    observed.write_text("row\n", encoding="utf-8")
    digest = hashlib.sha256(observed.read_bytes()).hexdigest()
    manifest = _write_manifest(tmp_path, {"observed.jsonl": {"sha256": digest}})
    assert replay.main(["verify", "--manifest", str(manifest)]) == 0
    observed.write_text("row tampered\n", encoding="utf-8")
    assert replay.main(["verify", "--manifest", str(manifest)]) == 1


def test_seed_rows_pass_the_rag_validator_and_carry_provenance() -> None:
    rows = build_seed_rows()
    assert 12 <= len(rows) <= 24
    assert validate_rag_cases(rows)["valid"] is True
    case_ids = [str(row["case_id"]) for row in rows]
    assert len(case_ids) == len(set(case_ids))
    assert any(row["track"] == "answer_aware" for row in rows)
    assert any(
        any("一" <= character <= "鿿" for character in str(row["query"]))
        for row in rows
    )
    for row in rows:
        metadata = row["metadata"]
        assert metadata["provenance"] == "seed-machine-generated, human-review-pending"
        assert metadata["version"] == SEED_VERSION
        assert isinstance(metadata["cross_lingual"], bool)


def test_seed_fixture_on_disk_matches_seed_source() -> None:
    from scripts.seed_kb_golden_set import DEFAULT_OUTPUT

    assert DEFAULT_OUTPUT.read_bytes() == render_golden_jsonl(build_seed_rows())


def test_eval_import_payload_passes_assistant_case_validation() -> None:
    payload = build_eval_import_payload(build_seed_rows())
    assert payload["mode"] == "skip_duplicates"
    for example in payload["examples"]:
        assert validate_case(example) == []
