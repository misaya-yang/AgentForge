#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

uv run --all-packages --extra test pytest tests/api tests/services/quiz tests/services/assistant/tools/test_quiz_tool_schema.py --no-cov -q --tb=line
