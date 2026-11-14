#!/usr/bin/env bash
set -euo pipefail
if (($#)); then
    echo "ERROR: use scripts/rust/build-update.sh for artifact selection or --dry-run" >&2
    exit 2
fi

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
# Direct callers enter the same clean-tree/resource-lock preflight as make/all.
if [[ "${AI_PLATFORM_RUST_BUILD_LOCK_HELD:-}" != "1" ]]; then
    exec bash "$repo_root/scripts/rust/build-update.sh" --artifact worker
fi
source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
overlay_root="$repo_root/rust/agent-runtime-overlay"
build_memory="${AI_PLATFORM_CAPABILITY_WORKER_BUILD_MEMORY:-2g}"
build_cpu_quota="${AI_PLATFORM_CAPABILITY_WORKER_BUILD_CPU_QUOTA:-100000}"
cargo_jobs="${AI_PLATFORM_CAPABILITY_WORKER_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"
minimum_host_free_percent="${AI_PLATFORM_CAPABILITY_WORKER_MIN_HOST_FREE_PERCENT:-20}"
minimum_disk_mb="${AI_PLATFORM_RUST_MIN_FREE_DISK_MB:-20480}"
build_platform="${AI_PLATFORM_CAPABILITY_WORKER_BUILD_PLATFORM:-linux/arm64}"
case "$build_platform" in
    linux/amd64|linux/arm64) ;;
    *) echo "ERROR: Worker build platform must be linux/amd64 or linux/arm64" >&2; exit 2 ;;
esac

# Public base-image inputs must be measured/selected by the integration owner.
# Mutable family tags in Dockerfiles are documentation, not release pins.
: "${CAPABILITY_RUST_BUILD_IMAGE:?Select the verified Rust builder image by digest}"
: "${CAPABILITY_WORKER_RUNTIME_IMAGE:?Select the verified control runtime image by digest}"
for base_image in "$CAPABILITY_RUST_BUILD_IMAGE" "$CAPABILITY_WORKER_RUNTIME_IMAGE"; do
    if [[ ! "$base_image" =~ ^[^[:space:]]+@sha256:[0-9a-f]{64}$ ]]; then
        echo "ERROR: capability base images require exact sha256 digest references" >&2
        exit 2
    fi
done
if [[ "$cargo_jobs" != "1" || ! "$minimum_host_free_percent" =~ ^[1-9][0-9]?$ || ! "$minimum_disk_mb" =~ ^[1-9][0-9]*$ ]]; then
    echo "ERROR: controlled builds require one Cargo job and a host-free threshold from 1 to 99 percent" >&2
    exit 2
fi
platform_assert_build_headroom "$repo_root" "$minimum_host_free_percent" "$minimum_disk_mb"

if [[ -z "$source_root" || ! -e "$source_root/.git" ]]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the controlled runtime source" >&2
    exit 1
fi
if [[ -n "$(git -C "$source_root" status --porcelain)" ]]; then
    echo "ERROR: controlled runtime source must be clean before a worker image build" >&2
    exit 1
fi

manifest="$overlay_root/manifest.json"
upstream_sha="$(platform-tools json-get "$manifest" upstream_sha)"
overlay_sha="$(platform-tools json-get "$manifest" sha256)"
if ! git -C "$source_root" cat-file -e "$upstream_sha^{commit}"; then
    echo "ERROR: controlled runtime source does not contain the pinned revision" >&2
    exit 1
fi

platform-tools source-lock verify-overlay --repo-root "$repo_root" --overlay-dir "$overlay_root"

schema_sha="$(shasum -a 256 "$repo_root/database/migrations/096_agent_capability_executions.sql" | awk '{print $1}')"
source_revision="${upstream_sha}+${overlay_sha:0:12}"
image="ai-gateway-agent-capability-worker:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
build_context="$(mktemp -d /tmp/ai-platform-capability-worker-build.XXXXXX)"

cleanup() {
    rm -rf -- "$build_context"
}
trap cleanup EXIT

platform-tools source-lock verify-overlay --repo-root "$repo_root" --source "$source_root" --upstream-sha "$upstream_sha"

git -C "$source_root" archive "$upstream_sha" | tar -x -C "$build_context"
cp -Rp "$overlay_root/kernel-rs/." "$build_context/codex-rs/"
# Keep a complete shared context; only the separate sandbox Dockerfile copies
# these public lock/catalog inputs into its wheels/package stages.
cp -Rp "$repo_root/deploy/agent-runtime-source/python" "$build_context/python"

monitor_build_memory() {
    platform_monitor_build_headroom "$1" "$repo_root" "$minimum_host_free_percent" "$minimum_disk_mb"
}

resource_builder="$(platform-tools buildx-resource-builder --repo-root "$repo_root" --memory "$build_memory" --cpu-quota "$build_cpu_quota")"
platform_configure_build_proxy_args
# A generic public CA bundle is an ephemeral BuildKit trust input, never an
# image layer or application credential. Omitted inputs keep system trust.
build_ca_args=()
if [[ -n "${AI_PLATFORM_BUILD_CA_BUNDLE:-}" ]]; then
    ca_bundle="$AI_PLATFORM_BUILD_CA_BUNDLE"
    if [[ "$ca_bundle" != /* || ! -f "$ca_bundle" || -L "$ca_bundle" \
        || "$ca_bundle" == *,* || "$ca_bundle" == *$'\n'* \
        || "$ca_bundle" =~ (^|/)\.env[^/]*(/|$) ]]; then
        echo 'ERROR: build CA requires an absolute regular public certificate bundle path' >&2
        exit 2
    fi
    if grep -Eq -- '-----BEGIN .*PRIVATE KEY-----' "$ca_bundle" \
        || ! openssl crl2pkcs7 -nocrl -certfile "$ca_bundle" -outform DER >/dev/null 2>&1; then
        echo 'ERROR: build CA must contain only parseable public certificates, never private keys' >&2
        exit 2
    fi
    build_ca_args+=(--secret "id=agentforge_build_ca,src=$ca_bundle")
fi
build_mirror_args=()
for mirror_name in DEBIAN_MIRROR DEBIAN_SECURITY_MIRROR; do
    mirror_input="AI_PLATFORM_BUILD_${mirror_name}"
    mirror_value="${!mirror_input:-}"
    if [[ -n "$mirror_value" ]]; then
        if [[ ! "$mirror_value" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?(/[^[:space:]@?#]*)?$ ]]; then
            echo 'ERROR: Debian build mirrors require HTTPS public repository URLs without credentials' >&2
            exit 2
        fi
        build_mirror_args+=(--build-arg "${mirror_name}=${mirror_value}")
    fi
done
docker buildx build --builder "$resource_builder" --platform "$build_platform" --load \
    ${platform_build_proxy_args[@]+"${platform_build_proxy_args[@]}"} \
    ${build_ca_args[@]+"${build_ca_args[@]}"} \
    ${build_mirror_args[@]+"${build_mirror_args[@]}"} \
    --file "$repo_root/deploy/agent-runtime-source/Dockerfile.capability-worker" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_SOURCE_REVISION=$source_revision" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_OVERLAY_SHA256=$overlay_sha" \
    --build-arg "AI_PLATFORM_CAPABILITY_WORKER_SCHEMA_SHA256=$schema_sha" \
    --build-arg "CARGO_BUILD_JOBS=$cargo_jobs" \
    --build-arg "CAPABILITY_RUST_BUILD_IMAGE=$CAPABILITY_RUST_BUILD_IMAGE" \
    --build-arg "CAPABILITY_WORKER_RUNTIME_IMAGE=$CAPABILITY_WORKER_RUNTIME_IMAGE" \
    --label "com.misaya.ai-platform.capability-worker.rust-build-image=$CAPABILITY_RUST_BUILD_IMAGE" \
    --label "com.misaya.ai-platform.capability-worker.runtime-base-image=$CAPABILITY_WORKER_RUNTIME_IMAGE" \
    --tag "$image" \
    "$build_context" &
build_pid="$!"
monitor_build_memory "$build_pid"

image_revision="$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')"
image_overlay="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.overlay-sha256"}}')"
image_schema="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.schema-sha256"}}')"
image_artifact="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.artifact"}}')"
image_binary="$(docker image inspect "$image" --format '{{index .Config.Labels "com.misaya.ai-platform.capability-worker.binary"}}')"
image_platform="$(docker image inspect "$image" --format '{{.Os}}/{{.Architecture}}')"
if [[ "$image_platform" != "$build_platform" \
    || "$image_revision" != "$source_revision" \
    || "$image_overlay" != "$overlay_sha" \
    || "$image_schema" != "$schema_sha" \
    || "$image_artifact" != "capability_worker" \
    || "$image_binary" != "ai-platform-capability-worker" ]]; then
    echo "ERROR: capability worker image labels do not match the source unit" >&2
    exit 1
fi

platform-tools source-lock record-local-image \
    --repo-root "$repo_root" \
    --lock "$repo_root/deploy/agent-runtime-source/lock.json" \
    --artifact capability_worker \
    --image "$image"

printf 'AGENT_CAPABILITY_WORKER_IMAGE_TAG=%s\n' "$image"
printf 'AGENT_CAPABILITY_WORKER_IMAGE_DIGEST=%s\n' \
    "$(docker image inspect "$image" --format '{{.Id}}')"
