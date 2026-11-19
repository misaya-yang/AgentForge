#!/usr/bin/env bash
# Scoped local Rust evidence; no host Cargo or repository target directories.
set -euo pipefail
source "$(dirname "$0")/../new/common.sh"
require_docker
# This target only builds/tests an isolated source context. It never operates on
# application containers; unrelated Compose projects do not own this builder.
cd "$PROJECT_ROOT"
source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:?Set the clean pinned upstream checkout}"
if [[ -n "$(git -C "$source_root" status --porcelain)" ]]; then
    log_error "Pinned upstream checkout must be clean"
    exit 1
fi
platform-tools source-lock validate --repo-root . --lock deploy/agent-runtime-source/lock.json --require-artifact agent_runtime
upstream_sha="$(platform-tools json-get rust/agent-runtime-overlay/manifest.json upstream_sha)"
platform-tools source-lock verify-overlay --repo-root "$PROJECT_ROOT" --source "$source_root" --upstream-sha "$upstream_sha"
stage="$(mktemp -d "${TMPDIR:-/tmp}/ai-platform-runtime-tests.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
git -C "$source_root" archive "$upstream_sha" | tar -x -C "$stage"
cp -Rp rust/agent-runtime-overlay/kernel-rs/. "$stage/codex-rs/"
resource_builder="$(platform-tools buildx-resource-builder --repo-root "$PROJECT_ROOT" --memory "${AI_PLATFORM_AGENT_RUNTIME_TEST_BUILD_MEMORY:-3g}" --cpu-quota "${AI_PLATFORM_AGENT_RUNTIME_BUILD_CPU_QUOTA:-200000}")"
docker buildx build --builder "$resource_builder" --target recovery-tests \
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
