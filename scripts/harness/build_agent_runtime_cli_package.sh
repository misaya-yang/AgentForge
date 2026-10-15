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
upstream_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["upstream_sha"])' "$receipt")"
overlay_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256"])' "$overlay_root/manifest.json")"

git -C "$source_root" cat-file -e "$upstream_sha^{commit}" || fail "source checkout does not contain $upstream_sha"
python3 - "$overlay_root" "$upstream_sha" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
expected_upstream = sys.argv[2]
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
if manifest.get("upstream_sha") != expected_upstream:
    raise SystemExit("ERROR: overlay and source receipt disagree")
if manifest.get("file_count") != len(files) or manifest.get("sha256") != digest.hexdigest():
    raise SystemExit("ERROR: overlay manifest does not match overlay files")
PY

python3 "$repo_root/scripts/harness/agent_runtime_supply_chain.py" validate \
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
docker build \
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
docker build \
    --file "$dockerfile" \
    --target export \
    --build-arg "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}" \
    --output "type=local,dest=$export_dir" \
    "$build_context"

mkdir -p "$output_dir"
install -m 0755 "$export_dir/codex" "$output_dir/codex"
python3 - "$output_dir" "$repo_root/sdk/cli/vendor" "$upstream_sha" "$overlay_sha" "$node_arch" "$repo_root" "$build_context" <<'PY'
import hashlib
import json
import pathlib
import sys

output = pathlib.Path(sys.argv[1])
vendor = pathlib.Path(sys.argv[2])
binary = output / "codex"
repository = pathlib.Path(sys.argv[6])
source = pathlib.Path(sys.argv[7])
lock = json.loads((repository / "deploy/agent-runtime-source/lock.json").read_text())
material_paths = {
    "LICENSE": (source / "LICENSE", lock["license"]["upstream_license_sha256"]),
    "NOTICE": (source / "NOTICE", lock["license"]["upstream_notice_sha256"]),
    "NOTICE.ai-platform.md": (repository / lock["license"]["notice"], lock["license"]["notice_sha256"]),
    "sbom.cdx.json": (repository / lock["build"]["sbom"], lock["build"]["sbom_sha256"]),
    "source-receipt.json": (repository / lock["build"]["source_receipt"], lock["build"]["source_receipt_sha256"]),
}
materials = {}
for name, (path, expected) in material_paths.items():
    content = path.read_bytes()
    actual = hashlib.sha256(content).hexdigest()
    if actual != expected:
        raise SystemExit(f"ERROR: native package material does not match source lock: {name}")
    (output / name).write_bytes(content)
    materials[name] = actual
receipt = {
    "schema_version": "ai-gateway-cli/native-artifact/v1",
    "upstream_sha": sys.argv[3],
    "overlay_sha256": sys.argv[4],
    "target_system": "linux",
    "target_arch": sys.argv[5],
    "binary": "codex",
    "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "materials": materials,
}
(output / "artifact.json").write_text(json.dumps(receipt, indent=2) + "\n")
vendor.mkdir(parents=True, exist_ok=True)
(vendor / "source.json").write_text(json.dumps({
    "schema_version": "ai-gateway-cli/native-source/v1",
    "upstream_sha": sys.argv[3],
    "overlay_sha256": sys.argv[4],
}, indent=2) + "\n")
PY

echo "Independent Agent CLI built at $output_dir/codex"
echo "Docker builder image: $image_tag"
