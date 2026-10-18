#!/usr/bin/env bash
# Source-bound development packaging only. Does not promote release admission.
set -euo pipefail
execution=${1:?usage: verify-evidence.sh EXECUTION_PACKAGE OUTPUT_CONTEXT}
context=${2:?new external output context required}
fail() { echo "STOP: $*" >&2; exit 3; }
regular() { [[ -f "$1" && ! -L "$1" ]] || fail 'required regular public evidence absent'; }
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
status=$execution/export/run-status.json
unit=$execution/export/source-package/source-unit.json
bins=$execution/export/source-package/evidence/built-binaries.json
regular "$status"
jq -e '.success == true' "$status" >/dev/null || fail 'actual complete compiler/test/export run has not succeeded'
regular "$unit"; regular "$bins"
[[ $(jq -r .source_unit_sha256 "$status") == "$(hash "$unit")" ]] || fail 'source-unit identity mismatch'
jq -e '(.schema == "ai-platform/runtime-source-unit/v1" or .schema == "ai-platform/runtime-source-unit/v2") and .candidate_start_allowed == false and .generated_release_identity == false and .release_lock_refresh_ready == false' "$unit" >/dev/null || fail 'source receipt is not the non-release source unit'
[[ $(jq -r '.evidence["evidence/built-binaries.json"]' "$unit") == "$(hash "$bins")" ]] || fail 'binary receipt is not in actual source unit'
jq -e --slurpfile unit "$unit" '
 .schema == "agentforge/source-bound-built-binaries/v1" and .images_built == false and .services_started == false
 and .source_binding.target_commit == $unit[0].target
 and .source_binding.target_tree == $unit[0].target_tree
 and .source_binding.candidate_source_sha256 == $unit[0].input_candidate_manifest_sha256
 and .source_binding.resolved_lock_sha256 == $unit[0].resolved_lock_sha256
 and ([.binaries[].name]|sort) == (["ai-platform-agent-runtime","ai-platform-history-gate","ai-platform-capability-worker","ai-platform-capability-worker-health","ai-platform-plugin-packs","ai-platform-python-supervisor","ai-platform-python-sandbox","ai-platform-python-sandbox-health","ai-platform-sandbox-pool","ai-platform-sandbox-attestor","ai-platform-sandbox-measurement"]|sort)
 and all(.binaries[]; (.sha256|test("^[a-f0-9]{64}$")) and (.bytes|type == "number" and . > 0 and . == floor))' "$bins" >/dev/null || fail 'binary set, source binding, hash or length invalid'
# Reconcile every command against the actual plan. No inspect-as-verify shortcut.
regular "$execution/control/execution-plan.json"
schema=$(jq -r .schema "$unit")
expected_steps=22
if [[ $schema == ai-platform/runtime-source-unit/v2 ]]; then
 expected_steps=6
 regular "$execution/batch-admission.json"
 jq -e --arg unit "$(hash "$unit")" --slurpfile binary "$bins" '.schema == "agentforge/source-batch-admission/v1" and .success == true and .source_unit_sha256 == $unit and .resolved_lock_sha256 == $binary[0].source_binding.resolved_lock_sha256 and .candidate_source_sha256 == $binary[0].source_binding.candidate_source_sha256 and .actual_cargo_invocations == 8 and .required_normal_elfs == 11 and .derived_source_evidence.result == "pass" and .failed == 0 and .filtered_out == 0 and .production_admitted == false and .generated_release_identity == false' "$execution/batch-admission.json" >/dev/null || fail 'new batch admission missing or not source-bound; old v1 does not admit v2'
 [[ $(jq -r .schema "$execution/control/execution-plan.json") == agentforge/controlled-execution-plan/v2 ]] || fail 'batch execution plan version mismatch'
 [[ $(jq -c '[.steps[].name]|sort' "$execution/control/execution-plan.json") == '["batch-bins","batch-check","batch-docs","batch-list","batch-metadata","batch-tests"]' ]] || fail 'batch command coverage changed'
fi
[[ $(jq '.steps|length' "$execution/control/execution-plan.json") == "$expected_steps" ]] || fail 'complete real-command count mismatch'
jq -e --argjson count "$expected_steps" '(.steps|map(.name)|unique|length)==$count' "$execution/control/execution-plan.json" >/dev/null || fail 'duplicate command plan entries'
while IFS= read -r step; do
 receipt_stem=$step; [[ $step != resolved-metadata ]] || receipt_stem=resolved-metadata-command
 receipt=$execution/export/source-package/evidence/$receipt_stem.json
 regular "$receipt"
 [[ $(jq -r --arg p "evidence/$receipt_stem.json" '.evidence[$p]' "$unit") == "$(hash "$receipt")" ]] || fail 'command receipt hash mismatch'
 jq -e --arg step "$step" --slurpfile plan "$execution/control/execution-plan.json" --slurpfile binary "$bins" '.command == ($plan[0].steps[]|select(.name==$step)|.command) and .exit_code == 0 and .stop_reason == null and .source_binding == $binary[0].source_binding' "$receipt" >/dev/null || fail 'actual command did not succeed with same source identity'
 for stream in stdout stderr; do
  file=$execution/export/source-package/evidence/$step.$stream; regular "$file"
  [[ $(jq -r --arg k "${stream}_sha256" '.[$k]' "$receipt") == "$(hash "$file")" ]] || fail 'actual command stream hash mismatch'
 done
done < <(jq -r '.steps[].name' "$execution/control/execution-plan.json")
for step in metadata-command schema-command; do
 receipt=$execution/export/$step.json; regular "$receipt"
 jq -e --arg unit "$(hash "$unit")" --slurpfile binary "$bins" '.exit_code == 0 and .stop_reason == null and .source_binding.source_unit_sha256 == $unit and .source_binding.resolved_lock_sha256 == $binary[0].source_binding.resolved_lock_sha256 and .source_binding.target_commit == $binary[0].source_binding.target_commit and .source_binding.target_tree == $binary[0].source_binding.target_tree' "$receipt" >/dev/null || fail 'final schema/metadata command not bound to actual unit'
 for stream in stdout stderr; do
  file=$execution/export/$step.$stream; regular "$file"
  [[ $(jq -r --arg k "${stream}_sha256" '.[$k]' "$receipt") == "$(hash "$file")" ]] || fail 'final stream hash mismatch'
 done
done
regular "$execution/export/source-inventory-before.json"; regular "$execution/export/source-inventory-after.json"
cmp -s "$execution/export/source-inventory-before.json" "$execution/export/source-inventory-after.json" || fail 'source changed during compilation'
# A new context contains selected public files only, never the whole repo/env.
[[ ! -e "$context" && ! -L "$context" ]] || fail 'output exists; preserve it and choose a new path'
mkdir -m 0700 "$context"
cp "$unit" "$context/source-unit.json"
cp "$bins" "$context/built-binaries.json"
jq -r '.binaries[]|"\(.sha256)  /cargo-target/debug/\(.name)"' "$bins" > "$context/cache-binaries.sha256"
jq -r '.binaries[]|"\(.bytes) \(.name)"' "$bins" > "$context/cache-binaries.bytes"
jq -n --arg unit "$(hash "$unit")" --arg bins "$(hash "$bins")" --arg status "$(hash "$status")" --slurpfile binaries "$bins" '{schema:"agentforge/local-dev-execution-artifacts/v1",source_unit_sha256:$unit,built_binary_receipt_sha256:$bins,successful_run_status_sha256:$status,source_binding:$binaries[0].source_binding,binaries:$binaries[0].binaries,production_admitted:false,generated_release_identity:false,release_gates_unchanged:true,services_started:false,images_built:false}' > "$context/dev-identity.json"
