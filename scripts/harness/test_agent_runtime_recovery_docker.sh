#!/usr/bin/env bash
# Scoped local Rust evidence; no host Cargo or repository target directories.
set -euo pipefail
source "$(dirname "$0")/../new/common.sh"
require_docker
assert_compose_owner
cd "$PROJECT_ROOT"
source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:?Set the clean pinned upstream checkout}"
if [[ -n "$(git -C "$source_root" status --porcelain)" ]]; then
    log_error "Pinned upstream checkout must be clean"
    exit 1
fi
python3 scripts/harness/agent_runtime_supply_chain.py validate --repo-root . --lock deploy/agent-runtime-source/lock.json --require-artifact agent_runtime
upstream_sha="$(python3 -c 'import json; print(json.load(open("rust/agent-runtime-overlay/manifest.json"))["upstream_sha"])')"
mkdir -p tmp/runtime-durable-recovery
stage="$(mktemp -d "$PROJECT_ROOT/tmp/runtime-durable-recovery/tests.XXXXXX")"
git -C "$source_root" archive "$upstream_sha" | tar -x -C "$stage"
cp -Rp rust/agent-runtime-overlay/kernel-rs/. "$stage/codex-rs/"
docker build --target recovery-tests --resource "memory=3g" --resource "cpu-quota=200000" \
    --build-arg CARGO_BUILD_JOBS=1 --file deploy/agent-runtime-source/Dockerfile.runtime "$stage" &
build_pid=$!

minimum_host_free_percent=20
monitor_build_memory() {
    local build_pid="$1"
    if ! command -v memory_pressure >/dev/null 2>&1; then
        wait "$build_pid"
        return
    fi
    while kill -0 "$build_pid" >/dev/null 2>&1; do
        local free_percent
        free_percent="$(memory_pressure | awk '/System-wide memory free percentage:/ {gsub(/%/, "", $5); print $5}')"
        if [[ "$free_percent" =~ ^[0-9]+$ ]] && (( free_percent < minimum_host_free_percent )); then
            echo "ERROR: host memory free percentage fell to ${free_percent}%; cancelling Docker build" >&2
            kill "$build_pid" >/dev/null 2>&1 || true
            wait "$build_pid" || true
            return 1
        fi
        sleep 10
    done
    wait "$build_pid"
}

monitor_build_memory "$build_pid"
