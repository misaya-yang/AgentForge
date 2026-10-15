from __future__ import annotations

import json
import re
from pathlib import Path

from scripts import agent_studio_regression

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/fixtures/agent-studio/regression_manifest.json"
PHASE_FILES = sorted((ROOT / "deploy/runbooks/agent-studio-prd").glob("phase-0[0-8]-*.md"))


def _phase_contract(path: Path) -> dict[str, object]:
    match = re.search(r"```json\s*(\{.*?\})\s*```", path.read_text(encoding="utf-8"), re.DOTALL)
    assert match is not None, f"missing machine contract in {path}"
    return json.loads(match.group(1))


def _required_contract_gates() -> list[tuple[str, str, str, str]]:
    gates: list[tuple[str, str, str, str]] = []
    for path in PHASE_FILES:
        contract = _phase_contract(path)
        phase = contract["phase"]
        validation = contract["validation"]
        assert isinstance(phase, dict) and isinstance(validation, dict)
        phase_id = str(phase["id"])
        commands = validation["commands"]
        assert isinstance(commands, list)
        for command in commands:
            assert isinstance(command, dict)
            if command.get("required") is True:
                gates.append(
                    (phase_id, str(command["id"]), str(command["cwd"]), str(command["command"]))
                )
    return gates


def _manifest() -> dict[str, object]:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


def test_manifest_exactly_covers_every_required_phase_gate() -> None:
    manifest = _manifest()
    assert manifest["schema_version"] == "agent-studio-regression/v1"
    entries = manifest["gates"]
    assert isinstance(entries, list)
    actual = [
        (entry["phase"], entry["id"], entry["cwd"], entry["command"])
        for entry in entries
        if isinstance(entry, dict) and entry.get("required") is True
    ]
    assert actual == _required_contract_gates()
    assert len(actual) == len({(phase, gate_id) for phase, gate_id, _, _ in actual})


def test_manifest_keeps_named_critical_and_assistant_gates() -> None:
    entries = {
        f"{entry['phase']}:{entry['id']}": entry["command"] for entry in _manifest()["gates"]
    }
    assert "test_internal_agent_capabilities.py" in entries["AS-00:capability-tests"]
    assert "test_control_plane.py" in entries["AS-00:capability-tests"]
    assert "test_agent_runtime_envelope.py" in entries["AS-02:gateway-envelope"]
    assert "test_internal_mcp_broker.py" in entries["AS-03:mcp-api-runtime"]
    assert "test_mcp_gateway_broker.py" in entries["AS-03:mcp-api-runtime"]
    assert "test_skill_entrypoint_policy.py" in entries["AS-04:skill-api-isolation"]
    assert "test_capability_plane.py" in entries["AS-04:knowledge-binding"]
    assert "test_internal_agent_capabilities.py" in entries["AS-04:knowledge-binding"]
    assert "test_agent_publication_atomicity.py" in entries["AS-06:publish-api-atomicity"]
    assert "make verify-eval-dev" in entries["AS-06:agent-eval"]
    assert "--built-image" in entries["AS-07:built-nginx-header-smoke"]
    all_commands = "\n".join(entries.values())
    assert "make test-isolation" in all_commands
    assert "make verify-assistant-runtime-dev" in all_commands


def test_manifest_has_no_optional_or_shell_placeholder_entries() -> None:
    entries = _manifest()["gates"]
    assert isinstance(entries, list)
    assert all(entry.get("required") is True for entry in entries)
    assert all(entry.get("cwd") == "." for entry in entries)
    assert all("TODO" not in entry.get("command", "") for entry in entries)


def test_workspace_hash_excludes_generated_report_diffs(monkeypatch) -> None:
    calls: list[tuple[str, ...]] = []

    def fake_git_bytes(*args: str) -> bytes:
        calls.append(args)
        if args[0] == "ls-files":
            return b""
        return b"fixture"

    monkeypatch.setattr(agent_studio_regression, "_git_bytes", fake_git_bytes)
    agent_studio_regression._workspace_hash()

    diff_call = next(call for call in calls if call[0] == "diff")
    assert ":(exclude)reports/**" in diff_call
    assert ":(exclude)web/playwright-report/**" in diff_call
    assert ":(exclude)web/test-results/**" in diff_call


def test_required_gate_rejects_zero_exit_with_skipped_tests(monkeypatch, tmp_path: Path) -> None:
    gate = {
        "phase": "AS-09",
        "id": "fixture",
        "cwd": ".",
        "command": "ignored",
    }

    class FakeProcess:
        stdout = iter(["4 passed, 2 skipped in 0.01s\n"])

        @staticmethod
        def wait() -> int:
            return 0

    def fake_popen(*_args, **_kwargs) -> FakeProcess:
        return FakeProcess()

    monkeypatch.setattr(agent_studio_regression.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(agent_studio_regression, "ROOT", tmp_path)
    result = agent_studio_regression._run_gate(gate, tmp_path)

    assert result["status"] == "failed"
    assert result["exit_code"] == 0
    assert result["skipped_count"] == 2
    assert result["failure_reason"] == "required gate reported 2 skipped test(s)"
