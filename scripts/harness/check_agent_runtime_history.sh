#!/usr/bin/env bash
# Release-blocking, offline native decoder gate. Never builds or mutates data.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
manifest="${AI_PLATFORM_HISTORY_CORPUS:-$repo_root/tests/fixtures/agent-runtime/history/corpus.json}"
lock="${AI_PLATFORM_HISTORY_SOURCE_LOCK:-$repo_root/deploy/agent-runtime-source/lock.json}"
report_dir="${AI_PLATFORM_HISTORY_REPORT_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/agentforge-upgrade/evidence/history-compatibility}"
direction=forward
decoder="${AI_PLATFORM_HISTORY_DECODER:-}"
if [[ "${1:-}" == --rollback && "$#" == 1 ]]; then
    direction=rollback
    decoder="${AI_PLATFORM_HISTORY_ROLLBACK_DECODER:-}"
elif [[ "$#" != 0 ]]; then
    echo 'ERROR: history gate accepts only optional --rollback' >&2
    exit 2
fi
if [[ "$report_dir" != /* || "$report_dir" == "$repo_root" || "$report_dir" == "$repo_root/"* ]]; then
    echo 'ERROR: history reports require an absolute external directory' >&2
    exit 2
fi
mkdir -p "$report_dir"
report_dir="$(cd "$report_dir" && pwd -P)"
if [[ "$report_dir" == "$repo_root" || "$report_dir" == "$repo_root/"* ]]; then
    echo 'ERROR: history reports resolve into repository' >&2
    exit 2
fi
# Invalidate any previous success before a missing executable/input can fail.
printf '{"schema":"ai-platform/history-gate/v1","result":"BLOCKED","reason":"decoder_or_input_not_admitted"}\n' > "$report_dir/$direction.json"
if [[ -z "$decoder" || ! -x "$decoder" || ! -f "$manifest" || ! -f "$lock" ]]; then
    echo 'ERROR: authenticated corpus and explicitly built native history decoder are required; no skip or synthetic fallback' >&2
    exit 2
fi
"$decoder" --manifest "$manifest" --source-lock "$lock" --direction "$direction" --receipt "$report_dir/$direction.json"
