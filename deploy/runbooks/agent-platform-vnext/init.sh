#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

make doctor
if [[ "${1:-}" == "--start" ]]; then
  exec make deploy ARGS="--no-migrate"
fi
