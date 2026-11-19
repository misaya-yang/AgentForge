#!/usr/bin/env bash
# Synthetic packaging-parser fixtures only, never a compiler/admission result.
set -euo pipefail
self=$(cd -- "$(dirname -- "$0")" && pwd -P)
base=${1:?new external fixture directory required}
[[ $base == /* && ! -e $base ]] || exit 2
mkdir -m 0700 "$base"
fixture=$base/fixture; evidence=$fixture/export/source-package/evidence
mkdir -p "$evidence" "$fixture/control"
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
names='["ai-platform-agent-runtime","ai-platform-history-gate","ai-platform-capability-worker","ai-platform-capability-worker-health","ai-platform-plugin-packs","ai-platform-python-supervisor","ai-platform-python-sandbox","ai-platform-python-sandbox-health","ai-platform-sandbox-pool","ai-platform-sandbox-attestor","ai-platform-sandbox-measurement"]'
binding='{"target_commit":"fixture","target_tree":"fixture-tree","candidate_source_sha256":"fixture-candidate","resolved_lock_sha256":"fixture-lock"}'
jq -n --argjson names "$names" --argjson binding "$binding" '{schema:"agentforge/source-bound-built-binaries/v1",source_binding:$binding,binaries:[$names[]|{name:.,sha256:("a"*64),bytes:100}],images_built:false,services_started:false}' > "$evidence/built-binaries.json"
jq -n '{schema:"agentforge/controlled-execution-plan/v2",steps:["batch-metadata","batch-check","batch-list","batch-tests","batch-docs","batch-bins"]|map({name:.,command:["fixture-command",.]})}' > "$fixture/control/execution-plan.json"
while read -r name; do
 : > "$evidence/$name.stdout"; : > "$evidence/$name.stderr"
 jq -n --arg name "$name" --argjson binding "$binding" --arg hash "$(hash "$evidence/$name.stdout")" '{command:["fixture-command",$name],exit_code:0,stop_reason:null,stdout_sha256:$hash,stderr_sha256:$hash,source_binding:$binding}' > "$evidence/$name.json"
done < <(jq -r '.steps[].name' "$fixture/control/execution-plan.json")
files='{}'
for file in "$evidence"/*.json; do files=$(jq -n --argjson files "$files" --arg path "evidence/$(basename "$file")" --arg hash "$(hash "$file")" '$files+{($path):$hash}'); done
unit=$fixture/export/source-package/source-unit.json
jq -n --argjson evidence "$files" '{schema:"ai-platform/runtime-source-unit/v2",candidate_start_allowed:false,generated_release_identity:false,release_lock_refresh_ready:false,target:"fixture",target_tree:"fixture-tree",input_candidate_manifest_sha256:"fixture-candidate",resolved_lock_sha256:"fixture-lock",evidence:$evidence}' > "$unit"
for name in metadata-command schema-command; do
 : > "$fixture/export/$name.stdout"; : > "$fixture/export/$name.stderr"
 jq -n --arg unit "$(hash "$unit")" --arg hash "$(hash "$fixture/export/$name.stdout")" '{exit_code:0,stop_reason:null,stdout_sha256:$hash,stderr_sha256:$hash,source_binding:{source_unit_sha256:$unit,resolved_lock_sha256:"fixture-lock",target_commit:"fixture",target_tree:"fixture-tree"}}' > "$fixture/export/$name.json"
done
printf '{}\n' > "$fixture/export/source-inventory-before.json"; cp "$fixture/export/source-inventory-before.json" "$fixture/export/source-inventory-after.json"
jq -n --arg unit "$(hash "$unit")" '{success:true,source_unit_sha256:$unit}' > "$fixture/export/run-status.json"
jq -n --arg unit "$(hash "$unit")" '{schema:"agentforge/source-batch-admission/v1",success:true,source_unit_sha256:$unit,resolved_lock_sha256:"fixture-lock",candidate_source_sha256:"fixture-candidate",actual_cargo_invocations:8,required_normal_elfs:11,derived_source_evidence:{result:"pass"},failed:0,filtered_out:0,production_admitted:false,generated_release_identity:false}' > "$fixture/batch-admission.json"
"$self/verify-evidence.sh" "$fixture" "$base/positive-fixture" > "$base/positive.log" 2>&1
reject() { if "$self/verify-evidence.sh" "$fixture" "$base/rejected-$1" > "$base/$1.log" 2>&1; then echo "FAILED: accepted $1" >&2; exit 1; fi; }
cp "$fixture/batch-admission.json" "$base/admission.original"
jq '.success=false' "$base/admission.original" > "$fixture/batch-admission.json"; reject failed-native-admission
jq '.source_unit_sha256="stale"' "$base/admission.original" > "$fixture/batch-admission.json"; reject stale-native-admission
jq '.filtered_out=1' "$base/admission.original" > "$fixture/batch-admission.json"; reject filtered-tests
jq '.production_admitted=true' "$base/admission.original" > "$fixture/batch-admission.json"; reject production-claim
jq 'del(.derived_source_evidence)' "$base/admission.original" > "$fixture/batch-admission.json"; reject missing-original-export-gates
cp "$base/admission.original" "$fixture/batch-admission.json"
cp "$fixture/control/execution-plan.json" "$base/plan.original"
jq '.steps=.steps[1:]' "$base/plan.original" > "$fixture/control/execution-plan.json"; reject missing-actual-command
jq '.steps[5].name="batch-tests"' "$base/plan.original" > "$fixture/control/execution-plan.json"; reject duplicate-actual-command
cp "$base/plan.original" "$fixture/control/execution-plan.json"
cp "$unit" "$base/unit.original"
jq '.schema="ai-platform/runtime-source-unit/v1"' "$base/unit.original" > "$unit"
jq --arg unit "$(hash "$unit")" '.source_unit_sha256=$unit' "$fixture/export/run-status.json" > "$base/status-v1"; cp "$base/status-v1" "$fixture/export/run-status.json"
reject new-receipts-cannot-pass-old-v1
printf 'PASS: synthetic v2 packaging parser positive + 8 negatives; native compiler/admission/image acceptance unrun by this fixture\n'
