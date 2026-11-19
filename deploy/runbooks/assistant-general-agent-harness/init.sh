#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

# Historical thinking/tool-selector test filenames were already absent from the shipped source.
# Current native entry/control contracts are checked here; this is not historical coverage proof.
make native-build-path-guard
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_control_plane --test runtime_capability_contract --test runtime_v2_turn_contract
