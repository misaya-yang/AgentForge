from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/eval_general_agent_failure_recovery.py"
SUITE = ROOT / "src/services/eval/fixtures/general_agent_failure_suite.v1.json"


def _load_script() -> ModuleType:
    spec = importlib.util.spec_from_file_location("failure_recovery_harness", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_failure_suite_declares_real_business_faults_and_binary_acceptance() -> None:
    suite = json.loads(SUITE.read_text(encoding="utf-8"))

    assert suite["schema_version"] == "general-agent-failure-suite/v1"
    assert "no weighted score" in suite["acceptance"]
    assert {case["domain"] for case in suite["cases"]} == {
        "financial operations",
        "legal and regulatory analysis",
        "agent orchestration",
        "legal discovery",
    }
    assert all(case["critical"] is True for case in suite["cases"])
    assert all(case["repetitions"] >= 3 for case in suite["cases"])
    assert sum(len(case["required_gates"]) for case in suite["cases"]) == 19


def test_loader_rejects_duplicate_json_keys(tmp_path: Path) -> None:
    module = _load_script()
    malformed = tmp_path / "duplicate.json"
    malformed.write_text(
        '{"schema_version":"general-agent-failure-suite/v1",'
        '"schema_version":"general-agent-failure-suite/v1",'
        '"suite_id":"x","acceptance":"x","cases":[]}',
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="duplicate JSON key"):
        module._load_suite(malformed)


def test_failure_recovery_harness_runs_production_paths_end_to_end(tmp_path: Path) -> None:
    report_path = tmp_path / "failure-report.json"
    environment = dict(os.environ)
    for name in (
        "DEEPSEEK_API_KEY",
        "GENERAL_AGENT_JUDGE_API_KEY",
        "OPENAI_API_KEY",
        "DASHSCOPE_API_KEY",
        "DASHSCOPE_CHAT_API_KEY",
    ):
        environment.pop(name, None)

    completed = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--suite",
            str(SUITE),
            "--output",
            str(report_path),
            "--offline-fixture",
        ],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )

    assert completed.returncode == 0, completed.stdout + completed.stderr
    summary = json.loads(completed.stdout)
    report = json.loads(report_path.read_text(encoding="utf-8"))
    assert summary["passed"] is True
    assert report["evaluation_kind"] == "deterministic_binary_safety_gate"
    assert report["uses_external_provider"] is False
    assert report["uses_api_key"] is False
    assert report["passed"] is True
    assert report["case_count"] == 4
    assert report["trial_count"] == 12
    assert all(case["observed_passes"] == case["required_passes"] == 3 for case in report["cases"])
    assert all(
        gate is True
        for case in report["cases"]
        for trial in case["trials"]
        for gate in trial["gates"].values()
    )
    assert report_path.stat().st_mode & 0o777 == 0o600


def test_script_never_accepts_or_reads_key_arguments() -> None:
    source = SCRIPT.read_text(encoding="utf-8")

    assert "--api-key" not in source
    assert "DEEPSEEK_API_KEY" not in source
    assert "GENERAL_AGENT_JUDGE_API_KEY" not in source
    assert "os.environ" not in source
    assert 'evaluation_kind": "deterministic_binary_safety_gate' in source
