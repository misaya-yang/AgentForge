#!/usr/bin/env bash
# Synthetic fixture tests of rejection behavior; never compiler/image acceptance.
set -euo pipefail
self=$(cd -- "$(dirname -- "$0")" && pwd -P)
base=${1:?new external test directory required}
[[ $base == /* && ! -e $base ]] || exit 2
mkdir -m 0700 "$base"
fixture=$base/fixture
mkdir -p "$fixture/export/source-package/evidence" "$fixture/control"
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
evidence=$fixture/export/source-package/evidence
binding='{"target_commit":"test-commit","target_tree":"test-tree","candidate_source_sha256":"test-candidate","resolved_lock_sha256":"test-lock"}'
names='["ai-platform-agent-runtime","ai-platform-history-gate","ai-platform-capability-worker","ai-platform-capability-worker-health","ai-platform-plugin-packs","ai-platform-python-supervisor","ai-platform-python-sandbox","ai-platform-python-sandbox-health","ai-platform-sandbox-pool","ai-platform-sandbox-attestor","ai-platform-sandbox-measurement"]'
jq -n --argjson names "$names" --argjson binding "$binding" '{schema:"agentforge/source-bound-built-binaries/v1",source_binding:$binding,binaries:[$names[]|{name:.,sha256:("a"*64),bytes:100}],images_built:false,services_started:false}' > "$evidence/built-binaries.json"
jq -n '{steps:[range(22)|{name:(if .==21 then "resolved-metadata" else "step-"+tostring end)}]}' > "$fixture/control/execution-plan.json"
for i in {0..21}; do
 name=step-$i; receipt_stem=$name
 if [[ $i == 21 ]]; then name=resolved-metadata; receipt_stem=resolved-metadata-command; fi
 : > "$evidence/$name.stdout"; : > "$evidence/$name.stderr"
 jq -n --argjson binding "$binding" --arg hash "$(hash "$evidence/$name.stdout")" '{exit_code:0,stop_reason:null,stdout_sha256:$hash,stderr_sha256:$hash,source_binding:$binding}' > "$evidence/$receipt_stem.json"
done
files='{}'
for file in "$evidence"/*.json; do
 files=$(jq -n --argjson files "$files" --arg key "evidence/$(basename "$file")" --arg hash "$(hash "$file")" '$files+{($key):$hash}')
done
unit=$fixture/export/source-package/source-unit.json
jq -n --argjson evidence "$files" '{schema:"ai-platform/runtime-source-unit/v1",candidate_start_allowed:false,generated_release_identity:false,release_lock_refresh_ready:false,target:"test-commit",target_tree:"test-tree",input_candidate_manifest_sha256:"test-candidate",resolved_lock_sha256:"test-lock",evidence:$evidence}' > "$unit"
for step in metadata-command schema-command; do
 : > "$fixture/export/$step.stdout"; : > "$fixture/export/$step.stderr"
 jq -n --arg unit "$(hash "$unit")" --arg hash "$(hash "$fixture/export/$step.stdout")" '{exit_code:0,stop_reason:null,stdout_sha256:$hash,stderr_sha256:$hash,source_binding:{source_unit_sha256:$unit,resolved_lock_sha256:"test-lock",target_commit:"test-commit",target_tree:"test-tree"}}' > "$fixture/export/$step.json"
done
printf '{}\n' > "$fixture/export/source-inventory-before.json"
cp "$fixture/export/source-inventory-before.json" "$fixture/export/source-inventory-after.json"
jq -n --arg unit "$(hash "$unit")" '{success:true,source_unit_sha256:$unit}' > "$fixture/export/run-status.json"
"$self/verify-evidence.sh" "$fixture" "$base/accepted-fixture" > "$base/positive.log" 2>&1
reject() {
 local label=$1
 if "$self/verify-evidence.sh" "$fixture" "$base/rejected-$label" > "$base/$label.log" 2>&1; then echo "FAILED: accepted $label" >&2; exit 1; fi
}
cp "$fixture/export/run-status.json" "$base/status.original"
printf '{"success":false}\n' > "$fixture/export/run-status.json"; reject failed-run
cp "$base/status.original" "$fixture/export/run-status.json"
printf 'corruption\n' >> "$unit"; reject changed-unit
# Remove exactly the deliberate fixture mutation, never real evidence.
sed '$d' "$unit" > "$base/unit.original"; cp "$base/unit.original" "$unit"
printf 'changed\n' > "$evidence/step-12.stderr"; reject changed-command-stream
: > "$evidence/step-12.stderr"
printf '{"changed":true}\n' > "$fixture/export/source-inventory-after.json"; reject source-mutated
cp "$fixture/export/source-inventory-before.json" "$fixture/export/source-inventory-after.json"
mv "$evidence/built-binaries.json" "$base/bins.original"
ln -s "$base/bins.original" "$evidence/built-binaries.json"; reject binary-receipt-symlink
# A source-unit-consistent duplicate binary must also fail the exact set gate.
unlink "$evidence/built-binaries.json"
jq '.binaries[10]=.binaries[0]' "$base/bins.original" > "$evidence/built-binaries.json"
jq --arg hash "$(hash "$evidence/built-binaries.json")" '.evidence["evidence/built-binaries.json"]=$hash' "$unit" > "$base/updated-unit"
cp "$base/updated-unit" "$unit"
jq --arg hash "$(hash "$unit")" '.source_unit_sha256=$hash' "$base/status.original" > "$fixture/export/run-status.json"
reject duplicate-role-binary
printf 'PASS: synthetic positive fixture + 6 fail-closed negative fixtures; no real build/image result\n'
