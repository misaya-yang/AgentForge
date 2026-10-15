#!/usr/bin/env bash
# Restore only the immutable upstream source; do not apply isolated candidates.
set -euo pipefail
BASELINE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
read -r upstream_sha upstream_tree upstream_url < <(python3 - "$BASELINE_ROOT" <<'PY'
import json
import pathlib
import sys

source = json.loads((pathlib.Path(sys.argv[1]) / "deploy/agent-runtime-source/lock.json").read_text())["source"]
print(source["upstream_sha"], source["git_tree_sha"], source["upstream_url"])
PY
)
source_dir="${AGENTFORGE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/agentforge}/upstream/codex-$upstream_sha"
if [ ! -d "$source_dir/.git" ]; then
    mkdir -p "$source_dir"
    git -C "$source_dir" init -q
    git -C "$source_dir" remote add origin "$upstream_url"
fi
if ! git -C "$source_dir" cat-file -e "$upstream_sha^{commit}" 2>/dev/null; then
    git -C "$source_dir" fetch --depth=1 origin "$upstream_sha"
fi
[ -z "$(git -C "$source_dir" status --porcelain)" ] || { printf 'Upstream checkout is dirty\n' >&2; exit 1; }
git -C "$source_dir" checkout --detach "$upstream_sha"
[ "$(git -C "$source_dir" rev-parse HEAD)" = "$upstream_sha" ]
[ "$(git -C "$source_dir" rev-parse 'HEAD^{tree}')" = "$upstream_tree" ]
printf 'Verified immutable Runtime source: %s\n' "$source_dir"
