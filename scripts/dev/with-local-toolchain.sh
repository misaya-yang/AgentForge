#!/usr/bin/env bash
# Execute an existing harness command with this checkout's isolated tools.
set -euo pipefail
BASELINE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="$BASELINE_ROOT/.tools/bin:$BASELINE_ROOT/.tools/node/bin:$BASELINE_ROOT/.venv/bin:$PATH"
export UV_PYTHON="$(cat "$BASELINE_ROOT/.python-version")"
export UV_FROZEN=1
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR="${AGENTFORGE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/agentforge}/targets/native-1.99.0"
export TIKTOKEN_CACHE_DIR="${AGENTFORGE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/agentforge}/tokenizers"
mkdir -p "$CARGO_TARGET_DIR"
cd "$BASELINE_ROOT"
exec "$@"
