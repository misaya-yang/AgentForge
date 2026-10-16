#!/usr/bin/env bash
set -euo pipefail

harness_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$harness_dir" rev-parse --show-toplevel)"
cd "$repo_root"

git status --short --branch
printf 'Program: '; platform-tools json-get deploy/runbooks/codex-harness-refresh-2026-09-22/loop-state.json lifecycle
printf 'Current upstream pin: '; platform-tools json-get deploy/agent-runtime-source/lock.json source.upstream_sha
printf 'Planned target: '; platform-tools json-get deploy/runbooks/codex-harness-refresh-2026-09-22/loop-state.json target_upstream_sha
printf '%s\n' 'Read-only preflight; no implementation or runtime action was started.'
