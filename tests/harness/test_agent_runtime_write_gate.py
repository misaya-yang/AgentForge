from __future__ import annotations

from pathlib import Path

import pytest

from scripts.harness import agent_runtime_write_gate as write_gate


def test_fork_path_requires_controlled_source_checkout(tmp_path: Path) -> None:
    with pytest.raises(write_gate.GateError, match="controlled Agent Runtime source"):
        write_gate._fork_path(str(tmp_path))


def test_gate_runs_only_the_lifecycle_contract(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    (tmp_path / "justfile").write_text("", encoding="utf-8")
    (tmp_path / "codex-rs").mkdir()
    build_root = tmp_path / "build"
    build_root.mkdir()
    calls: list[tuple[list[str], dict[str, object]]] = []

    class Completed:
        returncode = 0

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return Completed()

    class _TemporaryDirectory:
        def __init__(self, *_args, **_kwargs):
            pass

        def __enter__(self):
            return str(build_root)

        def __exit__(self, *_args):
            return None

    class _Archive:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def extractall(self, *_args, **_kwargs):
            return None

    monkeypatch.setattr(write_gate.subprocess, "run", fake_run)
    monkeypatch.setattr(write_gate.tempfile, "TemporaryDirectory", _TemporaryDirectory)
    monkeypatch.setattr(write_gate.tarfile, "open", lambda *_args, **_kwargs: _Archive())
    monkeypatch.setattr(write_gate.shutil, "copytree", lambda *_args, **_kwargs: None)
    assert write_gate.run_gate(tmp_path) == 0
    assert calls[-1][0] == [
        "cargo",
        "test",
        "-p",
        "ai-platform-agent-runtime",
        "--lib",
    ]
    assert calls[-1][1]["cwd"] == build_root / "codex-rs"
