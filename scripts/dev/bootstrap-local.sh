#!/usr/bin/env bash
# Install locked developer dependencies without starting services or changing globals.
set -euo pipefail
BASELINE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$BASELINE_ROOT"
TOOLS="$BASELINE_ROOT/.tools"
NODE_VERSION="$(cat .node-version)"
PYTHON_VERSION="$(cat .python-version)"
UV_VERSION="$(awk '$1 == "uv:" {gsub(/"/, "", $2); print $2; exit}' harness.yml)"
PNPM_VERSION="$(python3 -c 'import json; print(json.load(open("web/package.json"))["packageManager"].split("@")[1])')"
mkdir -p "$TOOLS/bin"
if command -v brew >/dev/null 2>&1; then
    for dependency in pkgconf cmake; do
        case "$dependency" in pkgconf) executable=pkg-config ;; *) executable="$dependency" ;; esac
        if ! command -v "$executable" >/dev/null 2>&1; then
            HOMEBREW_NO_AUTO_UPDATE=1 brew install "$dependency"
        fi
    done
fi

if [ ! -x "$TOOLS/bin/uv" ] || [ "$("$TOOLS/bin/uv" --version | awk '{print $2}')" != "$UV_VERSION" ]; then
    UV_TOOL_DIR="$TOOLS/uv" UV_TOOL_BIN_DIR="$TOOLS/bin" \
        uv tool install "uv==$UV_VERSION" --python "$PYTHON_VERSION"
fi
export PATH="$TOOLS/bin:$TOOLS/node/bin:$PATH"
uv python install "$PYTHON_VERSION"

if [ ! -x "$TOOLS/node/bin/node" ] || [ "$("$TOOLS/node/bin/node" --version)" != "v$NODE_VERSION" ]; then
    case "$(uname -s)" in
        Darwin) node_os=darwin ;;
        Linux) node_os=linux ;;
        *) printf 'Unsupported Node bootstrap OS\n' >&2; exit 1 ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) node_arch=arm64 ;;
        x86_64) node_arch=x64 ;;
        *) printf 'Unsupported Node bootstrap architecture\n' >&2; exit 1 ;;
    esac
    archive="node-v$NODE_VERSION-$node_os-$node_arch.tar.gz"
    stage="$(mktemp -d "$TOOLS/node-download.XXXXXX")"
    trap 'rm -rf "$stage"' EXIT
    curl --fail --silent --show-error --location \
        "https://nodejs.org/dist/v$NODE_VERSION/$archive" -o "$stage/$archive"
    curl --fail --silent --show-error --location \
        "https://nodejs.org/dist/v$NODE_VERSION/SHASUMS256.txt" -o "$stage/SHASUMS256.txt"
    python3 - "$stage" "$archive" <<'PY'
import hashlib
import pathlib
import sys

stage, archive = pathlib.Path(sys.argv[1]), sys.argv[2]
expected = dict(line.split()[::-1] for line in (stage / "SHASUMS256.txt").read_text().splitlines())
if hashlib.sha256((stage / archive).read_bytes()).hexdigest() != expected[archive]:
    raise SystemExit("Node archive checksum mismatch")
PY
    mkdir -p "$TOOLS/node"
    tar -xzf "$stage/$archive" --strip-components=1 -C "$TOOLS/node"
    rm -rf "$stage"
    trap - EXIT
fi

if [ ! -x "$TOOLS/bin/pnpm" ] || [ "$("$TOOLS/bin/pnpm" --version)" != "$PNPM_VERSION" ]; then
    npm install --global --prefix "$TOOLS" "pnpm@$PNPM_VERSION" --allow-scripts=pnpm
fi

export UV_PYTHON="$PYTHON_VERSION" UV_FROZEN=1
uv sync --frozen --all-packages --extra dev --extra test
pnpm -C web install --frozen-lockfile
# The SDK Worker owns a separate environment and lock, outside the uv workspace.
uv sync --frozen --project apps/sdk-agent-worker
native_rust="$(awk '$1 == "channel" {gsub(/"/, "", $3); print $3}' rust/platform-native/rust-toolchain.toml)"
runtime_rust="$(python3 -c 'import json; print(json.load(open("deploy/agent-runtime-source/lock.json"))["build"]["rustc"].split()[1])')"
for rust_version in "$native_rust" "$runtime_rust"; do
    if ! rustup run "$rust_version" rustc --version >/dev/null 2>&1; then
        rustup toolchain install "$rust_version" --profile minimal --component rustfmt --component clippy
    else
        for component in rustfmt clippy; do
            if ! rustup component list --toolchain "$rust_version" --installed | grep -q "^$component-"; then
                rustup component add --toolchain "$rust_version" "$component"
            fi
        done
    fi
done
bash scripts/dev/with-local-toolchain.sh uv run --no-sync python scripts/dev/prepare-tokenizer.py
printf 'Toolchain and frozen dependencies installed. Run make local-baseline.\n'
