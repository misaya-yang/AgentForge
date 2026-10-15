#!/bin/bash
set -euo pipefail

PROJECT_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
ENV_FILE=${ENV_FILE:-"$PROJECT_ROOT/.env"}
AI_PLATFORM_AGENT_RUNTIME_SOURCE=${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}

if [ -z "$AI_PLATFORM_AGENT_RUNTIME_SOURCE" ]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the controlled Agent fork" >&2
    exit 2
fi
if [ ! -f "$AI_PLATFORM_AGENT_RUNTIME_SOURCE/codex-rs/Cargo.toml" ]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE does not contain codex-rs/Cargo.toml" >&2
    exit 2
fi
if [ ! -f "$ENV_FILE" ]; then
    echo "ERROR: ENV_FILE is required for the isolated PostgreSQL contract" >&2
    exit 2
fi

set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

cd "$PROJECT_ROOT"
uv run --all-packages --extra test pytest -q --no-cov \
    tests/database/test_agent_runtime_thread_store_migration.py

export AI_PLATFORM_RUNTIME_MIGRATION_PATH="$PROJECT_ROOT/database/migrations/089_agent_runtime_thread_store.sql"

# The controlled checkout is the pristine upstream source.  Compose the
# repository-owned Runtime overlay into an ephemeral tree before running its
# PostgreSQL contract; testing the checkout directly omits the platform crate.
overlay_root="$PROJECT_ROOT/rust/agent-runtime-overlay"
upstream_sha=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["upstream_sha"])' \
    "$overlay_root/manifest.json")
runtime_context=$(mktemp -d /tmp/ai-platform-agent-thread-store-contract.XXXXXX)

cleanup() {
    rm -rf -- "$runtime_context"
}
trap cleanup EXIT

git -C "$AI_PLATFORM_AGENT_RUNTIME_SOURCE" archive "$upstream_sha" | tar -x -C "$runtime_context"
cp -R "$overlay_root/kernel-rs/." "$runtime_context/codex-rs/"

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$AI_PLATFORM_AGENT_RUNTIME_SOURCE/codex-rs/target"}
cd "$runtime_context/codex-rs"
cargo test -p ai-platform-agent-runtime --lib -- --include-ignored
