#!/usr/bin/env bash
set -euo pipefail
if (($#)); then
    echo "ERROR: use scripts/rust/build-update.sh for artifact selection or --dry-run" >&2
    exit 2
fi

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
if [[ "${AI_PLATFORM_RUST_BUILD_LOCK_HELD:-}" != "1" ]]; then
    exec bash "$repo_root/scripts/rust/build-update.sh" --artifact runtime
fi
fork_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
build_memory="${AI_PLATFORM_AGENT_RUNTIME_BUILD_MEMORY:-4g}"
build_cpu_quota="${AI_PLATFORM_AGENT_RUNTIME_BUILD_CPU_QUOTA:-200000}"
cargo_jobs="${AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"
minimum_host_free_percent="${AI_PLATFORM_AGENT_RUNTIME_MIN_HOST_FREE_PERCENT:-20}"
minimum_disk_mb="${AI_PLATFORM_RUST_MIN_FREE_DISK_MB:-20480}"
if [[ "$cargo_jobs" != "1" || ! "$minimum_host_free_percent" =~ ^[1-9][0-9]?$ || ! "$minimum_disk_mb" =~ ^[1-9][0-9]*$ ]]; then
    echo 'ERROR: controlled builds require one Cargo job and explicit valid memory/disk headroom thresholds' >&2
    exit 2
fi
platform_assert_build_headroom "$repo_root" "$minimum_host_free_percent" "$minimum_disk_mb"
build_platform="${AI_PLATFORM_AGENT_RUNTIME_BUILD_PLATFORM:-linux/arm64}"
case "$build_platform" in
    linux/amd64|linux/arm64) ;;
    *) echo "ERROR: Runtime build platform must be linux/amd64 or linux/arm64" >&2; exit 2 ;;
esac

if [[ -z "$fork_root" || ! -e "$fork_root/.git" ]]; then
    echo "ERROR: AI_PLATFORM_AGENT_RUNTIME_SOURCE must point to the controlled runtime source" >&2
    exit 1
fi
if [[ -n "$(git -C "$fork_root" status --porcelain)" ]]; then
    echo "ERROR: Agent Runtime source must be clean before a Runtime image build" >&2
    exit 1
fi
receipt_path="$repo_root/deploy/agent-runtime-source/source-receipt.json"
receipt_sha="$(platform-tools json-get "$receipt_path" source.fork_sha)"
schema_sha="$(platform-tools json-get "$receipt_path" schema_bundle.sha256)"
upstream_sha="$(platform-tools json-get "$receipt_path" source.upstream_sha)"
if ! git -C "$fork_root" cat-file -e "$upstream_sha^{commit}"; then
    echo "ERROR: source checkout does not contain the pinned upstream revision" >&2
    exit 1
fi

overlay_root="$repo_root/rust/agent-runtime-overlay"
platform-tools source-lock verify-overlay --repo-root "$repo_root" --overlay-dir "$overlay_root" --source-revision "$receipt_sha" --upstream-sha "$upstream_sha"

overlay_sha="$(platform-tools json-get "$overlay_root/manifest.json" sha256)"
source_revision="${upstream_sha}+${overlay_sha:0:12}"
image_tag="ai-gateway-agent-runtime:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
build_context="$(mktemp -d /tmp/ai-platform-agent-runtime-build.XXXXXX)"

cleanup() {
    rm -rf -- "$build_context"
}
trap cleanup EXIT

platform-tools source-lock verify-overlay --repo-root "$repo_root" --source "$fork_root" --upstream-sha "$upstream_sha"

git -C "$fork_root" archive "$upstream_sha" | tar -x -C "$build_context"
cp -Rp "$overlay_root/kernel-rs/." "$build_context/codex-rs/"

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
image_platform="$(docker image inspect "$image_tag" --format '{{.Os}}/{{.Architecture}}')"
if [[ "$image_platform" != "$build_platform" \
    || "$image_revision" != "$source_revision" \
    || "$image_schema_sha" != "$schema_sha" \
    || "$image_artifact" != "agent_runtime" \
    || "$image_binary" != "ai-platform-agent-runtime" ]]; then
    echo "ERROR: Runtime image labels do not match the source receipt and artifact identity" >&2
    exit 1
fi

image_id="$(docker image inspect "$image_tag" --format '{{.Id}}')"
platform-tools source-lock record-local-image \
    --repo-root "$repo_root" \
    --lock "$repo_root/deploy/agent-runtime-source/lock.json" \
    --artifact agent_runtime \
    --image "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_IMAGE_TAG=%s\n' "$image_tag"
printf 'AI_PLATFORM_AGENT_RUNTIME_IMAGE_DIGEST=%s\n' "$image_id"
printf 'AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256=%s\n' "$image_schema_sha"
