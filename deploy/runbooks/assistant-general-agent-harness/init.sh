#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

.venv/bin/python -m pytest tests/services/assistant/test_thinking_policy.py tests/services/assistant/test_tool_selector.py tests/services/assistant/test_streaming_prompt_contract.py tests/services/assistant/test_responses_api.py tests/services/assistant/test_model_registry_provider_boundaries.py --no-cov -q --tb=line
