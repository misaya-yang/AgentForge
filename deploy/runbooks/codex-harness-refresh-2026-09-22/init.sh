#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

git status --short --branch
python3 - <<'PY'
import json
from pathlib import Path

root = Path("deploy/runbooks/codex-harness-refresh-2026-09-22")
state = json.loads((root / "loop-state.json").read_text())
lock = json.loads(Path("deploy/agent-runtime-source/lock.json").read_text())
print("Program:", state["lifecycle"])
print("Current upstream pin:", lock["source"]["upstream_sha"])
print("Planned target:", state["target_upstream_sha"])
print("Read-only preflight; no implementation or runtime action was started.")
PY
