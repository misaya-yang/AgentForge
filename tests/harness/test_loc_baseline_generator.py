"""Git-object regression tests for the formal LOC baseline generator."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "inventory"))

import loc_baseline  # noqa: E402


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, str]:
    root = tmp_path / "repo"
    (root / "src").mkdir(parents=True)
    (root / "web" / "src").mkdir(parents=True)
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "loc-generator@example.invalid")
    _git(root, "config", "user.name", "LOC Generator Test")
    (root / "src" / "big.py").write_text("line\n" * 801, encoding="utf-8")
    (root / "web" / "src" / "big.ts").write_text("line\n" * 501, encoding="utf-8")
    _git(root, "add", ".")
    _git(root, "commit", "-q", "-m", "source")
    return root, _git(root, "rev-parse", "HEAD")


def test_generator_counts_git_blobs_not_dirty_worktree(tmp_path: Path) -> None:
    root, source_sha = _repo(tmp_path)
    (root / "src" / "big.py").write_text("dirty\n" * 900, encoding="utf-8")
    (root / "src" / "untracked_big.py").write_text(
        "untracked\n" * 900,
        encoding="utf-8",
    )

    baseline = loc_baseline.build(root, source_revision=source_sha)

    assert baseline["base_git_sha"] == source_sha
    assert baseline["oversized_python"]["files"] == [
        {
            "file": "src/big.py",
            "lines": 801,
            "unit": "gateway",
            "is_test": False,
        }
    ]
    assert baseline["oversized_typescript"]["files"][0]["lines"] == 501
    assert not any(
        row["file"] == "src/untracked_big.py"
        for row in baseline["oversized_python"]["files"]
    )


def test_generator_rejects_unknown_source_object(tmp_path: Path) -> None:
    root, _source_sha = _repo(tmp_path)

    with pytest.raises(loc_baseline.LocGenerationError, match="cannot resolve"):
        loc_baseline.build(root, source_revision="0" * 40)
