#!/usr/bin/env bash
# Baseline check for the agent-contract-unification program.
# Read-only: runs the contracts this program must never break.
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

# Native contract counterparts. Live Studio database cases remain explicitly ignored here.
make native-build-path-guard
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_control_plane --test runtime_v2_turn_contract --test agent_studio_agents_api --test native_openapi_serving
