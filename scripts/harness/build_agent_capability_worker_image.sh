#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
overlay_root="$repo_root/rust/agent-runtime-overlay"
build_memory="${AI_PLATFORM_CAPABILITY_WORKER_BUILD_MEMORY:-2g}"
build_cpu_quota="${AI_PLATFORM_CAPABILITY_WORKER_BUILD_CPU_QUOTA:-100000}"
cargo_jobs="${AI_PLATFORM_CAPABILITY_WORKER_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"
minimum_host_free_percent="${AI_PLATFORM_CAPABILITY_WORKER_MIN_HOST_FREE_PERCENT:-20}"

if [[ -z "$source_root" || ! -e "$source_root/.git" ]]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the controlled runtime source" >&2
    exit 1
fi
if [[ -n "$(git -C "$source_root" status --porcelain)" ]]; then
    echo "ERROR: controlled runtime source must be clean before a worker image build" >&2
    exit 1
fi

manifest="$overlay_root/manifest.json"
upstream_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["upstream_sha"])' "$manifest")"
overlay_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256"])' "$manifest")"
if ! git -C "$source_root" cat-file -e "$upstream_sha^{commit}"; then
    echo "ERROR: controlled runtime source does not contain the pinned revision" >&2
    exit 1
fi

python3 - "$overlay_root" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "manifest.json").read_text())
digest = hashlib.sha256()
files = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "manifest.json"):
    relative = path.relative_to(root).as_posix()
    payload = path.read_bytes()
    files.append(relative)
    digest.update(relative.encode())
    digest.update(b"\0")
    digest.update(payload)
    digest.update(b"\0")
if manifest.get("file_count") != len(files) or manifest.get("sha256") != digest.hexdigest():
    raise SystemExit("ERROR: Agent Runtime overlay manifest does not match its files")
PY

schema_sha="$(shasum -a 256 "$repo_root/database/migrations/096_agent_capability_executions.sql" | awk '{print $1}')"
source_revision="${upstream_sha}+${overlay_sha:0:12}"
image="ai-gateway-agent-capability-worker:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
build_context="$(mktemp -d /tmp/ai-platform-capability-worker-build.XXXXXX)"

cleanup() {
    rm -rf -- "$build_context"
}
trap cleanup EXIT

python3 - "$repo_root" "$source_root" "$upstream_sha" <<'PY_VERIFY_UPSTREAM'
import sys
from pathlib import Path

from scripts.harness.rust_gate_identity import verify_overlay_upstream_base

verify_overlay_upstream_base(
    repo_root=Path(sys.argv[1]), source=Path(sys.argv[2]), upstream_sha=sys.argv[3]
)
PY_VERIFY_UPSTREAM

git -C "$source_root" archive "$upstream_sha" | tar -x -C "$build_context"
cp -Rp "$overlay_root/kernel-rs/." "$build_context/codex-rs/"

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
            echo "ERROR: host memory guard stopped the capability worker build" >&2
            kill "$build_pid" >/dev/null 2>&1 || true
            wait "$build_pid" || true
            return 1
        fi
        sleep 10
    done
    wait "$build_pid"
}

docker build \
    --build-arg HTTP_PROXY --build-arg HTTPS_PROXY --build-arg NO_PROXY \
    --resource "memory=$build_memory" \
    --resource "cpu-quota=$build_cpu_quota" \
    --file "$repo_root/deploy/agent-runtime-source/Dockerfile.capability-worker" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_SOURCE_REVISION=$source_revision" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_OVERLAY_SHA256=$overlay_sha" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_SCHEMA_SHA256=$schema_sha" \
    --build-arg "CARGO_BUILD_JOBS=$cargo_jobs" \
    --tag "$image" \
    "$build_context" &
build_pid="$!"
monitor_build_memory "$build_pid"

image_revision="$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')"
image_overlay="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.overlay-sha256"}}')"
image_schema="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.schema-sha256"}}')"
image_binary="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.binary"}}')"
if [[ "$image_revision" != "$source_revision" \
    || "$image_overlay" != "$overlay_sha" \
    || "$image_schema" != "$schema_sha" \
    || "$image_binary" != "ai-platform-capability-worker" ]]; then
    echo "ERROR: capability worker image labels do not match the source unit" >&2
    exit 1
fi

python3 "$repo_root/scripts/harness/agent_runtime_supply_chain.py" record-local-image \
    --repo-root "$repo_root" \
    --lock "$repo_root/deploy/agent-runtime-source/lock.json" \
    --artifact capability_worker \
    --image "$image"

printf 'AGENT_CAPABILITY_WORKER_IMAGE_TAG=%s\n' "$image"
printf 'AGENT_CAPABILITY_WORKER_IMAGE_DIGEST=%s\n' \
    "$(docker image inspect "$image" --format '{{.Id}}')"
