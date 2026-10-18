#!/usr/bin/env bash
# Generate complete notices from the locked, installed dependency sources.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ $# -ne 1 ]]; then
  echo 'usage: check_third_party_notices.sh OUTPUT_DIRECTORY' >&2
  exit 2
fi
output="$1"
if [[ -e "$output" ]]; then
  echo 'Notice output directory must not already exist' >&2
  exit 2
fi
metadata="$(mktemp "${TMPDIR:-/tmp}/agentforge-notice-metadata.XXXXXXXX")"
trap 'rm -f "$metadata"' EXIT
cargo metadata --manifest-path "$root/rust/platform-native/Cargo.toml" \
  --locked --all-features --format-version 1 > "$metadata"
platform-tools third-party-notices --repo-root "$root" \
  --cargo-metadata "$metadata" --check-all --require-zero --output "$output"
