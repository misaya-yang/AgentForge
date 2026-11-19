#!/bin/sh
# Offline control-flow test of the shipped rotator. The mock is deliberately not
# TLS evidence: crypto/identity verification belongs to native importer tests.
set -eu
repository=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
source_script="$repository/deploy/helm/ai-gateway/files/workload-import.sh"
root=$(mktemp -d "${TMPDIR:-/tmp}/agentforge-rotator.XXXXXXXX")
trap 'rm -rf "$root"' EXIT HUP INT TERM
mkdir -p "$root/input/g1" "$root/input/g2" "$root/state"
ln -s g1 "$root/input/..data"
cat > "$root/native-import" <<'MOCK'
#!/bin/sh
set -eu
[ ! -e "$FIXTURE_ROOT/fail" ] || exit 42
out= generation=
while [ "$#" -gt 0 ]; do
  case "$1" in --out) out=$2; shift 2;; --generation) generation=$2; shift 2;; *) shift;; esac
done
mkdir -m 700 "$out/$generation"
printf 'synthetic\n' > "$out/$generation/key.pem"
chmod 600 "$out/$generation/key.pem"
rm -f "$out/current"
ln -s "$generation" "$out/current"
printf 'import\n' >> "$FIXTURE_ROOT/calls"
if [ -e "$FIXTURE_ROOT/race" ]; then
  rm "$FIXTURE_ROOT/race"
  rm "$FIXTURE_ROOT/input/..data"
  ln -s g2 "$FIXTURE_ROOT/input/..data"
fi
MOCK
chmod 700 "$root/native-import"
# Rewrite only constant fixture paths; invoke the exact production control flow.
sed -e "s|root=/run/workload|root=$root/state|" -e "s|/input|$root/input|g" \
  -e "s|/usr/local/bin/workload-credentials|$root/native-import|g" "$source_script" > "$root/import.sh"
export FIXTURE_ROOT="$root" WORKLOAD_SPIFFE_ID=spiffe://fixture.test/service/gateway WORKLOAD_ROTATION_SECONDS=5
sh "$root/import.sh" once
sh "$root/import.sh" ready
[ "$(cat "$root/state/accepted-generation")" = g1 ]
# Stable projection does not import another credential generation.
sh "$root/import.sh" once
[ "$(wc -l < "$root/calls" | tr -d ' ')" = 1 ]
rm "$root/input/..data"; ln -s g2 "$root/input/..data"
touch "$root/fail"
if sh "$root/import.sh" once 2>/dev/null; then exit 1; fi
if sh "$root/import.sh" ready; then exit 1; fi
[ "$(cat "$root/state/accepted-generation")" = g1 ]
rm "$root/fail"
sh "$root/import.sh" once
sh "$root/import.sh" ready
[ "$(cat "$root/state/accepted-generation")" = g2 ]
# A projection change during import is never falsely recorded as accepted.
rm "$root/state/accepted-generation"
rm "$root/input/..data"; ln -s g1 "$root/input/..data"
touch "$root/race"
sh "$root/import.sh" once
sh "$root/import.sh" ready
[ "$(cat "$root/state/accepted-generation")" = g2 ]
printf '%s\n' 'PASS: initial import, stable dedupe, failed rotation readiness, recovery, projection race'
