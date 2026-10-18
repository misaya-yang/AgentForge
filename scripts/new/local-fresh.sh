#!/bin/sh
# Only the std-only operations launcher uses the approved host bootstrap exception.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
cache="$HOME/.cache/agentforge-native-launcher"
mkdir -p "$cache"
launcher="$cache/local-fresh-$$"
trap 'rm -f "$launcher"' EXIT HUP INT TERM
compiler=$(rustup which --toolchain 1.99.0 rustc)
"$compiler" --edition=2024 -C debuginfo=0 -C opt-level=0 "$root/scripts/new/local-fresh.rs" -o "$launcher"
case "$*" in
  check*) "$launcher" --root "$root" "$@" ;;
  *)
    controller_file="$cache/lock-controller.path"
    test -f "$controller_file" || { echo "retained native resource controller required" >&2; exit 1; }
    controller=$(cat "$controller_file")
    test -x "$controller" || { echo "retained native resource controller unavailable" >&2; exit 1; }
    cd "$root"
    "$controller" resource-lock run --repo-root "$root" --resource integration-runtime \
      --timeout-seconds 7200 --heartbeat-seconds 10 \
      --expected-end-condition "Local fresh operation exits; private generation and original data retained" \
      -- "$launcher" --root "$root" "$@"
    ;;
esac
