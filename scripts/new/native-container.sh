#!/bin/sh
# Bootstrap only: operational policy and Docker invocation live in std-only Rust.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
cache="${XDG_CACHE_HOME:-$HOME/.cache}/agentforge-native-launcher"
mkdir -p "$cache"
# A unique output avoids replacing another running launcher's executable.
launcher="$cache/launcher-$$"
trap 'rm -f "$launcher"' EXIT HUP INT TERM
compiler=$(rustup which --toolchain 1.99.0 rustc)
"$compiler" --edition=2024 -C debuginfo=0 -C opt-level=0 \
  "$root/scripts/new/native-container.rs" -o "$launcher"
"$launcher" --root "$root" "$@"
