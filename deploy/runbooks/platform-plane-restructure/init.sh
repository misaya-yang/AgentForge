#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

# This stack is memory-tight; never parallelise Docker or Cargo here.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export COMPOSE_PARALLEL_LIMIT="${COMPOSE_PARALLEL_LIMIT:-1}"

make doctor
