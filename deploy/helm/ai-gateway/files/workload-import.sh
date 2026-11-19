#!/bin/sh
# The native importer pins one projected ..data generation, verifies the SVID,
# copies regular private files, and atomically activates a generation. No key
# bytes enter argv, environment, shell variables or logs.
set -eu
umask 077
root=/run/workload
ready() {
  test -L /input/..data && test -r "$root/accepted-generation" &&
    test "$(readlink /input/..data)" = "$(cat "$root/accepted-generation")" &&
    test -L "$root/credentials/current"
}
if [ "${1:-}" = ready ]; then ready; exit; fi
mkdir -p "$root/credentials"
chmod 700 "$root/credentials"
counter=0
while :; do
  if ! ready; then
    generation=$(readlink /input/..data)
    counter=$((counter + 1))
    /usr/local/bin/workload-credentials import-svid \
      --source-root /input --out "$root/credentials" \
      --generation "svid-$(date +%s)-$$-$counter" \
      --spiffe-id "$WORKLOAD_SPIFFE_ID"
    # If projection rotated during import, readiness remains false until the next
    # import. Do not certify a generation merely because import returned success.
    test "$generation" = "$(readlink /input/..data)" || continue
    printf '%s\n' "$generation" > "$root/.accepted-generation"
    mv "$root/.accepted-generation" "$root/accepted-generation"
  fi
  [ "${1:-}" = once ] && exit 0
  sleep "$WORKLOAD_ROTATION_SECONDS"
done
