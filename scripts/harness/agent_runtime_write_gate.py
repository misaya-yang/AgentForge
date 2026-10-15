#!/usr/bin/env python3
"""Run the CHR-04 tool side-effect lifecycle contract tests.

The gate is deliberately source-level: it runs the small platform lifecycle
ledger in the controlled Agent fork and does not call a provider or mutate the
developer's Compose stack.  The Docker/runtime acceptance gate is separate.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


class GateError(RuntimeError):
    pass


def _fork_path(value: str | None) -> Path:
    path = Path(value or "../AI--Platfform-agent-runtime-source").expanduser().resolve()
    if not (path / "justfile").is_file() or not (path / "codex-rs").is_dir():
        raise GateError(f"controlled Agent Runtime source is not a source checkout: {path}")
    return path


def run_gate(fork: Path) -> int:
    repo_root = Path(__file__).resolve().parents[2]
    overlay = repo_root / "rust" / "agent-runtime-overlay"
    manifest = json.loads((overlay / "manifest.json").read_text())
    upstream_sha = str(manifest["upstream_sha"])
    env = os.environ.copy()
    env.setdefault("CARGO_TERM_COLOR", "never")
    env.setdefault(
        "CARGO_TARGET_DIR",
        str(Path(tempfile.gettempdir()) / "ai-platform-agent-runtime-write-gate-target"),
    )
    with tempfile.TemporaryDirectory(prefix="ai-platform-agent-runtime-write-gate-") as temp:
        archive_path = Path(temp) / "source.tar"
        with archive_path.open("wb") as archive:
            subprocess.run(
                ["git", "-C", str(fork), "archive", upstream_sha],
                stdout=archive,
                check=True,
            )
        with tarfile.open(archive_path) as archive:
            archive.extractall(temp, filter="data")
        shutil.copytree(
            overlay / "kernel-rs",
            Path(temp) / "codex-rs",
            dirs_exist_ok=True,
        )
        completed = subprocess.run(
            ["cargo", "test", "-p", "ai-platform-agent-runtime", "--lib"],
            cwd=Path(temp) / "codex-rs",
            env=env,
            check=False,
        )
    if completed.returncode:
        raise GateError(f"tool lifecycle contract failed with exit {completed.returncode}")
    print("AI_PLATFORM_AGENT_RUNTIME_WRITE_GATE_OK")
    print("invariants=approval,dispatch_fence,idempotency,recovery,pairing")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fork", default=os.environ.get("AI_PLATFORM_AGENT_RUNTIME_SOURCE"))
    args = parser.parse_args()
    try:
        return run_gate(_fork_path(args.fork))
    except (GateError, OSError) as error:
        print(f"agent-runtime-write-gate: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
