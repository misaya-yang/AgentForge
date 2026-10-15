from __future__ import annotations

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock


def _load_runtime_regression_module():
    script_path = Path(__file__).resolve().parents[2] / "scripts" / "assistant_runtime_regression.py"
    spec = importlib.util.spec_from_file_location("assistant_runtime_regression_script", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load assistant runtime regression script: {script_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runtime_regression = _load_runtime_regression_module()


def _passing_gate_result() -> dict[str, object]:
    return {
        "schema_version": "assistant-runtime-regression-gate/v1",
        "timestamp": "2026-07-10T00:00:00+00:00",
        "status": "pass",
        "groups": [
            {
                "id": "ahr-test",
                "phase": "AHR-04",
                "label": "Test Group",
                "passed": True,
                "exit_code": 0,
                "elapsed_seconds": 0.1,
                "summary_line": "1 passed",
            }
        ],
        "phases": {"AHR-04": True},
        "summary": {
            "total_groups": 1,
            "passed": 1,
            "failed": 0,
            "total_elapsed_seconds": 0.1,
        },
        "no_go_thresholds": {
            "all_groups_must_pass": True,
            "critical_phases": ["AHR-04"],
        },
    }


def test_gate_no_write_leaves_default_reports_untouched(
    tmp_path: Path,
    monkeypatch,
    capsys,
) -> None:
    report_dir = tmp_path / "reports" / "assistant-runtime-regression"
    report_dir.mkdir(parents=True)
    json_report = report_dir / "latest.json"
    markdown_report = report_dir / "latest.md"
    json_report.write_text("json sentinel\n", encoding="utf-8")
    markdown_report.write_text("markdown sentinel\n", encoding="utf-8")

    write_reports = Mock()
    monkeypatch.setattr(runtime_regression, "run_gate", lambda _repo_root: _passing_gate_result())
    monkeypatch.setattr(runtime_regression, "write_reports", write_reports)
    monkeypatch.chdir(tmp_path)

    assert runtime_regression.main(["gate", "--no-write"]) == 0

    write_reports.assert_not_called()
    assert json_report.read_text(encoding="utf-8") == "json sentinel\n"
    assert markdown_report.read_text(encoding="utf-8") == "markdown sentinel\n"
    assert json.loads(capsys.readouterr().out)["status"] == "pass"


def test_eval_golden_uses_recorded_observations_and_temporary_reports(
    tmp_path: Path,
    monkeypatch,
) -> None:
    calls: list[list[str]] = []
    generated_paths: list[Path] = []

    def fake_run(cmd: list[str], **_kwargs):
        calls.append(cmd)
        if "validate" in cmd:
            return SimpleNamespace(returncode=0, stdout='{"valid": true}', stderr="")

        output_path = Path(cmd[cmd.index("--output") + 1])
        markdown_path = Path(cmd[cmd.index("--markdown") + 1])
        generated_paths.extend([output_path, markdown_path])
        assert output_path.parent == markdown_path.parent
        assert output_path.parent.is_dir()
        assert tmp_path / "reports" not in output_path.parents
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(
                {
                    "gate": {
                        "status": "pass",
                        "metrics": {
                            "pass_rate": 1.0,
                            "critical_pass_rate": 1.0,
                            "trajectory_pass_rate": 1.0,
                        },
                    }
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(runtime_regression.subprocess, "run", fake_run)
    golden_group = next(
        group for group in runtime_regression.TEST_GROUPS if group["runner"] == "eval_golden"
    )

    result = runtime_regression._run_eval_golden(golden_group, tmp_path)

    assert result["passed"] is True
    gate_cmd = next(cmd for cmd in calls if "gate" in cmd)
    assert gate_cmd[gate_cmd.index("--observations") + 1] == (
        "tests/fixtures/eval/observations/assistant_regression_v1.jsonl"
    )
    assert all("reports/eval-regression" not in str(path) for path in generated_paths)
    assert generated_paths
    assert not generated_paths[0].parent.exists()


def test_markdown_report_has_exactly_one_final_newline(tmp_path: Path) -> None:
    json_report = tmp_path / "latest.json"
    markdown_report = tmp_path / "latest.md"

    runtime_regression.write_reports(_passing_gate_result(), json_report, markdown_report)

    markdown = markdown_report.read_text(encoding="utf-8")
    assert markdown.endswith("\n")
    assert not markdown.endswith("\n\n")
