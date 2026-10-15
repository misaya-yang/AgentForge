#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
fork_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
build_memory="${AI_PLATFORM_AGENT_RUNTIME_BUILD_MEMORY:-4g}"
build_cpu_quota="${AI_PLATFORM_AGENT_RUNTIME_BUILD_CPU_QUOTA:-200000}"
cargo_jobs="${AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"
minimum_host_free_percent="${AI_PLATFORM_AGENT_RUNTIME_MIN_HOST_FREE_PERCENT:-20}"

if [[ -z "$fork_root" || ! -e "$fork_root/.git" ]]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the controlled runtime source" >&2
    exit 1
fi
if [[ -n "$(git -C "$fork_root" status --porcelain)" ]]; then
    echo "ERROR: Agent Runtime source must be clean before a Runtime image build" >&2
    exit 1
fi
receipt_path="$repo_root/deploy/agent-runtime-source/source-receipt.json"
receipt_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["fork_sha"])' "$receipt_path")"
schema_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["schema_bundle"]["sha256"])' "$receipt_path")"
upstream_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["upstream_sha"])' "$receipt_path")"
if ! git -C "$fork_root" cat-file -e "$upstream_sha^{commit}"; then
    echo "ERROR: source checkout does not contain the pinned upstream revision" >&2
    exit 1
fi

overlay_root="$repo_root/rust/agent-runtime-overlay"
python3 - "$overlay_root" "$receipt_sha" "$upstream_sha" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "manifest.json").read_text())
digest = hashlib.sha256()
files = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "manifest.json"):
    rel = path.relative_to(root).as_posix()
    payload = path.read_bytes()
    files.append(rel)
    digest.update(rel.encode())
    digest.update(b"\0")
    digest.update(payload)
    digest.update(b"\0")
if manifest.get("source_revision") != sys.argv[2] or manifest.get("upstream_sha") != sys.argv[3]:
    raise SystemExit("ERROR: Agent Runtime overlay source identity does not match the receipt")
if manifest.get("file_count") != len(files) or manifest.get("sha256") != digest.hexdigest():
    raise SystemExit("ERROR: Agent Runtime overlay manifest does not match its files")
PY

overlay_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256"])' "$overlay_root/manifest.json")"
source_revision="${upstream_sha}+${overlay_sha:0:12}"
image_tag="ai-gateway-agent-runtime:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
build_context="$(mktemp -d /tmp/ai-platform-agent-runtime-build.XXXXXX)"

cleanup() {
    rm -rf -- "$build_context"
}
trap cleanup EXIT

python3 - "$repo_root" "$fork_root" "$upstream_sha" <<'PY_VERIFY_UPSTREAM'
import sys
from pathlib import Path

from scripts.harness.rust_gate_identity import verify_overlay_upstream_base

verify_overlay_upstream_base(
    repo_root=Path(sys.argv[1]), source=Path(sys.argv[2]), upstream_sha=sys.argv[3]
)
PY_VERIFY_UPSTREAM

git -C "$fork_root" archive "$upstream_sha" | tar -x -C "$build_context"
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
            echo "ERROR: host memory free percentage fell to ${free_percent}%; cancelling Docker build" >&2
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
    --file "$repo_root/deploy/agent-runtime-source/Dockerfile.runtime" \
    --build-arg "AI_PLATFORM_AGENT_RUNTIME_FORK_SHA=$source_revision" \
    --build-arg "AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256=$schema_sha" \
    --build-arg "CARGO_BUILD_JOBS=$cargo_jobs" \
    --tag "$image_tag" \
    "$build_context" &
build_pid="$!"
monitor_build_memory "$build_pid"

image_revision="$(docker image inspect "$image_tag" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')"
image_schema_sha="$(docker image inspect "$image_tag" --format '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.schema-sha256"}}')"
image_artifact="$(docker image inspect "$image_tag" --format '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.artifact"}}')"
image_binary="$(docker image inspect "$image_tag" --format '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.binary"}}')"
if [[ "$image_revision" != "$source_revision" \
    || "$image_schema_sha" != "$schema_sha" \
    || "$image_artifact" != "agent_runtime" \
    || "$image_binary" != "ai-platform-agent-runtime" ]]; then
    echo "ERROR: Runtime image labels do not match the source receipt and artifact identity" >&2
    exit 1
fi

image_id="$(docker image inspect "$image_tag" --format '{{.Id}}')"
python3 "$repo_root/scripts/harness/agent_runtime_supply_chain.py" record-local-image \
    --repo-root "$repo_root" \
    --lock "$repo_root/deploy/agent-runtime-source/lock.json" \
    --artifact agent_runtime \
    --image "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_IMAGE_TAG=%s\n' "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_IMAGE_DIGEST=%s\n' "$image_id"
printf 'AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256=%s\n' "$image_schema_sha"
