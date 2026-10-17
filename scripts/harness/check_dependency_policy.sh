#!/usr/bin/env bash
# Read-only, locked dependency admission; no compilation or lock regeneration.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
version="$(cargo deny --version)"
if [[ "$version" != 'cargo-deny 0.20.2' ]]; then
  echo 'Dependency gate requires the pinned cargo-deny 0.20.2' >&2
  exit 2
fi
cargo deny --manifest-path "$root/rust/platform-native/Cargo.toml" --locked \
  check --config "$root/deploy/security/deny.toml" advisories bans licenses sources
# all-features is fixed in deny.toml and repeated for native metadata admission.
cargo metadata --manifest-path "$root/rust/platform-native/Cargo.toml" \
  --locked --all-features --format-version 1 | \
  platform-tools dependency-policy metadata --repo-root "$root" --metadata -
