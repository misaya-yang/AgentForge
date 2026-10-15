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

if [[ -z "$fork_root" || ! -d "$fork_root/.git" ]]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the independent runtime source" >&2
    exit 1
fi

if [[ -n "$(git -C "$fork_root" status --porcelain)" ]]; then
    echo "ERROR: Agent Runtime source must be clean before an image build" >&2
    exit 1
fi

receipt_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["fork_sha"])' "$repo_root/deploy/agent-runtime-source/source-receipt.json")"
schema_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["schema_bundle"]["sha256"])' "$repo_root/deploy/agent-runtime-source/source-receipt.json")"
upstream_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["upstream_sha"])' "$repo_root/deploy/agent-runtime-source/source-receipt.json")"

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
image_tag="ai-gateway-agent-runtime-source:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
build_context="$(mktemp -d /tmp/ai-platform-agent-runtime-source-build.XXXXXX)"

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
cp -R "$overlay_root/kernel-rs/." "$build_context/codex-rs/"

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
    --file "$repo_root/deploy/agent-runtime-source/Dockerfile" \
    --build-arg "AI_PLATFORM_AGENT_RUNTIME_UPSTREAM_SHA=$source_revision" \
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
    || "$image_artifact" != "app_server" \
    || "$image_binary" != "codex-app-server" ]]; then
    echo "ERROR: image source/schema/artifact labels do not match the source receipt" >&2
    exit 1
fi

python3 - "$image_tag" <<'PY'
import json
import select
import subprocess
import sys
import time

image = sys.argv[1]
request = {
    "method": "initialize",
    "id": 0,
    "params": {"clientInfo": {"name": "ai_platform_probe", "title": "AI Platform Probe", "version": "0.1.0"}},
}
process = subprocess.Popen(
    ["docker", "run", "--rm", "-i", "--network", "none", image],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
assert process.stdin is not None
assert process.stdout is not None
assert process.stderr is not None
process.stdin.write(json.dumps(request) + "\n")
process.stdin.flush()

deadline = time.monotonic() + 30
initialize_seen = False
stdout_lines = []
while time.monotonic() < deadline:
    readable, _, _ = select.select([process.stdout], [], [], 1)
    if not readable:
        if process.poll() is not None:
            break
        continue
    line = process.stdout.readline()
    if not line:
        break
    stdout_lines.append(line)
    if not line.strip().startswith("{"):
        continue
    item = json.loads(line)
    if item.get("id") == 0 and isinstance(item.get("result"), dict):
        initialize_seen = True
        break

process.stdin.close()
try:
    process.wait(timeout=10)
except subprocess.TimeoutExpired:
    process.terminate()
    process.wait(timeout=5)
stderr = process.stderr.read()
if not initialize_seen:
    raise SystemExit(
        "image protocol probe did not return initialize result; "
        f"stdout={''.join(stdout_lines)[-1000:]!r}; stderr={stderr[-1000:]!r}"
    )
PY

image_id="$(docker image inspect "$image_tag" --format '{{.Id}}')"
python3 "$repo_root/scripts/harness/agent_runtime_supply_chain.py" record-local-image \
    --repo-root "$repo_root" \
    --lock "$repo_root/deploy/agent-runtime-source/lock.json" \
    --artifact app_server \
    --image "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_SOURCE_IMAGE_TAG=%s\n' "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_SOURCE_IMAGE_DIGEST=%s\n' "$image_id"
printf 'AI_PLATFORM_AGENT_RUNTIME_SOURCE_SCHEMA_SHA256=%s\n' "$image_schema_sha"
