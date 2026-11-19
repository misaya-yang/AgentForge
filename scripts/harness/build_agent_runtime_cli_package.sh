#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
output_dir="${AI_GATEWAY_CLI_OUTPUT_DIR:-}"
dry_run=false

usage() {
    echo "Usage: AI_PLATFORM_AGENT_RUNTIME_SOURCE=/clean/codex-harness $0 [--output DIR] [--dry-run]"
}

while (($#)); do
    case "$1" in
        --output)
            [[ $# -ge 2 ]] || { echo "ERROR: --output requires a directory" >&2; exit 2; }
            output_dir="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

[[ -n "$source_root" ]] || fail "AI_PLATFORM_AGENT_RUNTIME_SOURCE is required"
git -C "$source_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 || fail "source is not a Git checkout"
[[ -z "$(git -C "$source_root" status --porcelain)" ]] || fail "source checkout must be clean"

receipt="$repo_root/deploy/agent-runtime-source/source-receipt.json"
overlay_root="$repo_root/rust/agent-runtime-overlay"
dockerfile="$repo_root/deploy/agent-runtime-source/Dockerfile.cli"
upstream_sha="$(platform-tools json-get "$receipt" source.upstream_sha)"
overlay_sha="$(platform-tools json-get "$overlay_root/manifest.json" sha256)"

git -C "$source_root" cat-file -e "$upstream_sha^{commit}" || fail "source checkout does not contain $upstream_sha"
platform-tools source-lock verify-overlay --repo-root "$repo_root" --overlay-dir "$overlay_root" --upstream-sha "$upstream_sha" --source "$source_root"

platform-tools source-lock validate \
    --repo-root "$repo_root" --lock "$repo_root/deploy/agent-runtime-source/lock.json"

if [[ "$dry_run" == "true" ]]; then
    echo "DRY RUN: independent CLI source identity is valid"
    echo "upstream=$upstream_sha overlay=$overlay_sha output=${output_dir:-sdk/cli/vendor/linux-<docker-arch>}"
    exit 0
fi

command -v docker >/dev/null 2>&1 || fail "docker is required"
build_context="$(mktemp -d /tmp/ai-platform-agent-cli-build.XXXXXX)"
export_dir="$(mktemp -d /tmp/ai-platform-agent-cli-export.XXXXXX)"
cleanup() {
    rm -rf -- "$build_context" "$export_dir"
}
trap cleanup EXIT

git -C "$source_root" archive "$upstream_sha" | tar -x -C "$build_context"
cp -R "$overlay_root/kernel-rs/." "$build_context/codex-rs/"

image_tag="ai-gateway-independent-cli-builder:local-${upstream_sha:0:12}-${overlay_sha:0:12}"
resource_builder="$(platform-tools buildx-resource-builder --repo-root "$repo_root" --memory "${AI_PLATFORM_AGENT_RUNTIME_BUILD_MEMORY:-4g}" --cpu-quota "${AI_PLATFORM_AGENT_RUNTIME_BUILD_CPU_QUOTA:-200000}")"
docker buildx build --builder "$resource_builder" --load \
    --file "$dockerfile" \
    --target builder \
    --build-arg "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}" \
    --build-arg "AI_PLATFORM_AGENT_RUNTIME_UPSTREAM_SHA=$upstream_sha" \
    --build-arg "AI_PLATFORM_AGENT_RUNTIME_OVERLAY_SHA256=$overlay_sha" \
    --tag "$image_tag" \
    "$build_context"
docker run --rm --network none "$image_tag" /tmp/codex --version
docker_arch="$(docker version --format '{{.Server.Arch}}')"
case "$docker_arch" in
    amd64|x86_64) node_arch="x64" ;;
    arm64|aarch64) node_arch="arm64" ;;
    *) fail "unsupported Docker architecture for Node package layout: $docker_arch" ;;
esac
if [[ -z "$output_dir" ]]; then
    output_dir="$repo_root/sdk/cli/vendor/linux-$node_arch"
fi
docker buildx build --builder "$resource_builder" \
    --file "$dockerfile" \
    --target export \
    --build-arg "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}" \
    --output "type=local,dest=$export_dir" \
    "$build_context"

mkdir -p "$output_dir"
install -m 0755 "$export_dir/codex" "$output_dir/codex"
platform-tools cli-package-materials --repo-root "$repo_root" --source "$build_context" --output "$output_dir" --vendor "$repo_root/sdk/cli/vendor" --upstream-sha "$upstream_sha" --overlay-sha "$overlay_sha" --arch "$node_arch"

echo "Independent Agent CLI built at $output_dir/codex"
echo "Docker builder image: $image_tag"
