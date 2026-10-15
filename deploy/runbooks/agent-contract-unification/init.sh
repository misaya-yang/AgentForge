#!/usr/bin/env bash
# Baseline check for the agent-contract-unification program.
# Read-only: runs the contracts this program must never break.
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

uv run --all-packages --extra test pytest -q --no-cov --tb=line \
  tests/api/test_agents_api.py \
  tests/contract/test_agent_studio_regression_manifest.py \
  tests/integration/test_assistant_isolation_contract.py \
  tests/integration/test_assistant_openapi_contract.py \
  tests/services/assistant/test_subagent_manager.py \
  tests/services/assistant/test_tool_selector.py
