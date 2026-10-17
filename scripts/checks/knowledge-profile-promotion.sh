#!/usr/bin/env bash
# Offline measurement gate. Base policy must already exist in the trusted Git base.
set -euo pipefail
if [[ $# != 1 || ! "$1" =~ ^[0-9a-f]{40}$ ]]; then
  echo 'usage: knowledge-profile-promotion.sh BASE_SHA' >&2
  exit 2
fi
base_sha=$1
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
if ! git diff --name-only "$base_sha" HEAD -- \
  rust/platform-native/crates/platform-knowledge-domain/src/migration_knowledge_ingest \
  rust/platform-native/crates/platform-knowledge-domain/src/migration_knowledge_query \
  rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest \
  rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query \
  rust/platform-native/crates/platform-knowledge-service/src/parsing_eval.rs \
  rust/platform-native/crates/platform-knowledge-service/src/settings.rs \
  rust/platform-native/crates/platform-knowledge-service/src/process.rs \
  reports/knowledge/quality | rg -q .; then
  echo 'No Knowledge profile/evaluator changes; promotion not requested.'
  exit 0
fi
policy_temp=$(mktemp "${TMPDIR:-/tmp}/knowledge-policy.XXXXXX")
trap 'rm -f "$policy_temp"' EXIT
if ! git show "$base_sha:reports/knowledge/quality/promotion-policy.json" > "$policy_temp"; then
  echo 'Knowledge quality baseline has not been established on the base revision; promotion blocked.' >&2
  exit 1
fi
candidate=reports/knowledge/quality/candidate.json
if [[ ! -f "$candidate" ]]; then
  echo 'Missing measured Knowledge candidate receipt; fixture/replay results cannot authorize promotion.' >&2
  exit 1
fi
source_sha=$(jq -er '.source_sha' "$candidate")
if [[ ! "$source_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo 'Invalid measurement source SHA.' >&2
  exit 1
fi
git merge-base --is-ancestor "$source_sha" HEAD
# Receipt-only commits may follow the measurement source. Code/config changes may not.
if ! git diff --exit-code "$source_sha" HEAD -- rust/platform-native/crates/platform-knowledge-domain \
  rust/platform-native/crates/platform-knowledge-service rust/platform-native/crates/platform-knowledge-ops \
  rust/platform-native/crates/platform-gateway-foundation rust/platform-native/Cargo.toml rust/platform-native/Cargo.lock; then
  echo 'Measured Knowledge source differs from this revision.' >&2
  exit 1
fi
knowledge-eval-gate "$repo_root" "$policy_temp" "$candidate" "$source_sha"
