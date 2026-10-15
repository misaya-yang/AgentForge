#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "$script_dir/../.." && pwd -P)"
original_args=("$@")
artifact="all"
dry_run=false

usage() {
    cat <<'EOF'
Usage: scripts/rust/build-update.sh [--artifact runtime|worker|app-server|all] [--dry-run]

Runs the existing Docker-contained Rust image build entrypoints under the shared
rust-build lock. Cargo executes only inside Docker multi-stage builders. This
command does not publish or claim multi-arch images.
EOF
}

while (($#)); do
    case "$1" in
        --artifact)
            [[ $# -ge 2 ]] || { echo "ERROR: --artifact requires a value" >&2; exit 2; }
            artifact="$2"
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

case "$artifact" in
    runtime|worker|app-server|all) ;;
    *)
        echo "ERROR: unsupported artifact: $artifact" >&2
        exit 2
        ;;
esac

lock_timeout="${AI_PLATFORM_RUST_BUILD_LOCK_TIMEOUT_SECONDS:-7200}"
if [[ ! "$lock_timeout" =~ ^[1-9][0-9]*$ ]]; then
    echo "ERROR: AI_PLATFORM_RUST_BUILD_LOCK_TIMEOUT_SECONDS must be a positive integer" >&2
    exit 2
fi

if [[ "${AI_PLATFORM_RUST_BUILD_LOCK_HELD:-}" != "1" ]]; then
    exec "$script_dir/locks.sh" run \
        --resource rust-build \
        --timeout-seconds "$lock_timeout" \
        --heartbeat-seconds 10 \
        --expected-end-condition "local-only $artifact build/update finishes without publication" \
        -- env \
        AI_PLATFORM_RUST_BUILD_LOCK_HELD=1 \
        CARGO_BUILD_JOBS=1 \
        AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS=1 \
        AI_PLATFORM_CAPABILITY_WORKER_CARGO_JOBS=1 \
        "$script_dir/build-update.sh" "${original_args[@]}"
fi

export CARGO_BUILD_JOBS=1
export AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS=1
export AI_PLATFORM_CAPABILITY_WORKER_CARGO_JOBS=1

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

positive_integer() {
    [[ "$1" =~ ^[1-9][0-9]*$ ]]
}

available_memory_mb() {
    if [[ -r /proc/meminfo ]]; then
        awk '/^MemAvailable:/ {print int($2 / 1024); found=1; exit} END {if (!found) exit 1}' \
            /proc/meminfo
        return
    fi
    if command -v memory_pressure >/dev/null 2>&1 && command -v sysctl >/dev/null 2>&1; then
        local total_bytes free_percent
        total_bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
        free_percent="$(memory_pressure 2>/dev/null | awk '
            /System-wide memory free percentage:/ {
                value=$NF; gsub(/%/, "", value); print value; exit
            }')"
        if [[ "$total_bytes" =~ ^[0-9]+$ && "$free_percent" =~ ^[0-9]+$ ]]; then
            echo $((total_bytes / 1024 / 1024 * free_percent / 100))
            return
        fi
    fi
    return 1
}

free_disk_mb() {
    df -Pk "$repo_root" | awk 'NR == 2 {print int($4 / 1024); found=1} END {if (!found) exit 1}'
}

minimum_memory_mb="${AI_PLATFORM_RUST_MIN_AVAILABLE_MEMORY_MB:-4096}"
minimum_disk_mb="${AI_PLATFORM_RUST_MIN_FREE_DISK_MB:-20480}"
positive_integer "$minimum_memory_mb" || fail "minimum available memory must be a positive integer"
positive_integer "$minimum_disk_mb" || fail "minimum free disk must be a positive integer"

available_mb="$(available_memory_mb)" || fail "available memory cannot be measured"
disk_mb="$(free_disk_mb)" || fail "free disk cannot be measured"
((available_mb >= minimum_memory_mb)) || fail \
    "available memory ${available_mb}MB is below required ${minimum_memory_mb}MB"
((disk_mb >= minimum_disk_mb)) || fail \
    "free disk ${disk_mb}MB is below required ${minimum_disk_mb}MB"

git_root="$(git -C "$repo_root" rev-parse --show-toplevel 2>/dev/null)" || fail \
    "repository root is not a Git checkout"
[[ "$(cd "$git_root" && pwd -P)" == "$repo_root" ]] || fail \
    "script path and Git repository root disagree"
allowed_platform_wip_path() {
    case "$1" in
        docs/*|reports/*) return 0 ;;
        deploy/runbooks/*/README.md) return 0 ;;
        deploy/runbooks/*/HANDOFF.md) return 0 ;;
        deploy/runbooks/*/loop-state.json) return 0 ;;
        deploy/runbooks/*/work-packages.yml) return 0 ;;
        deploy/runbooks/*/receipts/*) return 0 ;;
        *) return 1 ;;
    esac
}

declare -a blocked_dirty_paths=()
record_dirty_path() {
    local path="$1"
    if ! allowed_platform_wip_path "$path"; then
        blocked_dirty_paths+=("$path")
    fi
}
while IFS= read -r -d '' path; do
    record_dirty_path "$path"
done < <(git -C "$repo_root" diff --name-only --no-renames -z HEAD)
while IFS= read -r -d '' path; do
    record_dirty_path "$path"
done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
if ((${#blocked_dirty_paths[@]})); then
    printf 'ERROR: platform checkout has dirty non-documentation/build-input paths:\n' >&2
    printf '  %s\n' "${blocked_dirty_paths[@]}" >&2
    fail "commit or revert Rust/build inputs and non-receipt code before artifact identity/build"
fi

source_root="${AI_PLATFORM_AGENT_RUNTIME_SOURCE:-}"
[[ -n "$source_root" ]] || fail "AI_PLATFORM_AGENT_RUNTIME_SOURCE is required"
git -C "$source_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 || fail \
    "AI_PLATFORM_AGENT_RUNTIME_SOURCE must be a Git checkout"
[[ -z "$(git -C "$source_root" status --porcelain)" ]] || fail \
    "controlled Runtime source must be clean"

identity_script="$repo_root/scripts/harness/agent_runtime_supply_chain.py"
lock_file="$repo_root/deploy/agent-runtime-source/lock.json"
[[ -f "$identity_script" && -f "$lock_file" ]] || fail \
    "Agent Runtime identity authority is missing"

declare -a build_scripts=()
declare -a dockerfiles=()
declare -a identity_artifacts=()
case "$artifact" in
    runtime)
        build_scripts+=("$repo_root/scripts/harness/build_agent_runtime_image.sh")
        dockerfiles+=("$repo_root/deploy/agent-runtime-source/Dockerfile.runtime")
        identity_artifacts+=("agent_runtime")
        ;;
    worker)
        build_scripts+=("$repo_root/scripts/harness/build_agent_capability_worker_image.sh")
        dockerfiles+=("$repo_root/deploy/agent-runtime-source/Dockerfile.capability-worker")
        identity_artifacts+=("capability_worker")
        ;;
    app-server)
        build_scripts+=("$repo_root/scripts/harness/build_agent_runtime_source_image.sh")
        dockerfiles+=("$repo_root/deploy/agent-runtime-source/Dockerfile")
        identity_artifacts+=("app_server")
        ;;
    all)
        build_scripts+=(
            "$repo_root/scripts/harness/build_agent_runtime_image.sh"
            "$repo_root/scripts/harness/build_agent_capability_worker_image.sh"
        )
        dockerfiles+=(
            "$repo_root/deploy/agent-runtime-source/Dockerfile.runtime"
            "$repo_root/deploy/agent-runtime-source/Dockerfile.capability-worker"
        )
        identity_artifacts+=("agent_runtime" "capability_worker")
        ;;
esac

for path in "${build_scripts[@]}" "${dockerfiles[@]}"; do
    [[ -f "$path" ]] || fail "required build path is missing: $path"
done
for dockerfile in "${dockerfiles[@]}"; do
    grep -Eq 'cargo[[:space:]]+build[[:space:]]+--locked' "$dockerfile" || fail \
        "Rust build entrypoint is not Cargo --locked: $dockerfile"
done

print_command() {
    printf '  '
    printf '%q ' "$@"
    printf '\n'
}

run_command() {
    if [[ "$dry_run" == "true" ]]; then
        print_command "$@"
    else
        "$@"
    fi
}

echo "Docker-contained Rust build preflight: artifact=$artifact jobs=$CARGO_BUILD_JOBS cargo_mode=--locked"
echo "Resources: available_memory=${available_mb}MB free_disk=${disk_mb}MB"
echo "LOCAL-ONLY: this command does not publish or claim multi-arch artifacts."

# A freshly refreshed source lock intentionally has no runnable image digest.
# Validate its source/build identity first, let each Docker builder record its
# immutable local image, then require the selected runnable artifacts below.
run_command python3 "$identity_script" validate \
    --repo-root "$repo_root" \
    --lock "$lock_file"
for build_script in "${build_scripts[@]}"; do
    run_command bash "$build_script"
done
for identity_artifact in "${identity_artifacts[@]}"; do
    run_command python3 "$identity_script" validate \
        --repo-root "$repo_root" \
        --lock "$lock_file" \
        --require-artifact "$identity_artifact"
done

if [[ "$artifact" == "all" ]]; then
    run_command python3 "$identity_script" verify-local-images \
        --repo-root "$repo_root" \
        --lock "$lock_file"
fi

if [[ "$dry_run" == "true" ]]; then
    echo "DRY RUN: preflight passed; no identity or build command was executed."
else
    echo "Docker-contained Rust build completed; publication and multi-arch evidence remain separate."
fi
