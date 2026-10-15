from __future__ import annotations

import importlib.util
import json
from pathlib import Path


def _load_script():
    script_path = Path(__file__).resolve().parents[2] / "scripts" / "assistant_capability_live.py"
    spec = importlib.util.spec_from_file_location("assistant_capability_live_script", script_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


script = _load_script()


def _write_jsonl(path: Path, rows: list[dict]) -> None:
    path.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")


def test_cli_gates_bounded_result_receipts(tmp_path: Path, capsys) -> None:
    manifest = tmp_path / "manifest.jsonl"
    receipts = tmp_path / "receipts.jsonl"
    _write_jsonl(
        manifest,
        [
            {
                "case_id": "assistant.multi_turn.exact_marker",
                "evidence_tier": "real_provider",
                "repetitions": 3,
                "minimum_passes": 2,
                "critical": False,
            }
        ],
    )
    _write_jsonl(
        receipts,
        [
            {
                "case_id": "assistant.multi_turn.exact_marker",
                "evidence_tier": "real_provider",
                "trial": trial,
                "checks": [
                    {
                        "name": "exact_answer",
                        "kind": "outcome",
                        "passed": trial != 3,
                        "evidence": f"trial-{trial} response hash matched",
                    }
                ],
            }
            for trial in range(1, 4)
        ],
    )

    assert script.main([str(manifest), str(receipts)]) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "pass"
    assert result["evidence_tiers"]["real_provider"] == {
        "cases": 1,
        "passed": 1,
        "trials": 3,
    }


def test_cli_rejects_proxy_only_success(tmp_path: Path, capsys) -> None:
    manifest = tmp_path / "manifest.jsonl"
    receipts = tmp_path / "receipts.jsonl"
    _write_jsonl(
        manifest,
        [
            {
                "case_id": "assistant.code.executes",
                "evidence_tier": "local_live",
                "repetitions": 1,
                "minimum_passes": 1,
                "critical": False,
            }
        ],
    )
    _write_jsonl(
        receipts,
        [
            {
                "case_id": "assistant.code.executes",
                "evidence_tier": "local_live",
                "trial": 1,
                "checks": [
                    {
                        "name": "non_empty_output",
                        "kind": "proxy",
                        "passed": True,
                        "evidence": "response length was greater than zero",
                    }
                ],
            }
        ],
    )

    assert script.main([str(manifest), str(receipts)]) == 1
    assert json.loads(capsys.readouterr().out)["status"] == "fail"


def test_cli_fails_closed_on_malformed_input(tmp_path: Path, capsys) -> None:
    manifest = tmp_path / "manifest.jsonl"
    receipts = tmp_path / "receipts.jsonl"
    manifest.write_text("not-json\n", encoding="utf-8")
    receipts.write_text("", encoding="utf-8")

    assert script.main([str(manifest), str(receipts)]) == 1
    assert "invalid JSONL" in capsys.readouterr().err
