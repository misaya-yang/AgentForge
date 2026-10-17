import type { Detector, GuardrailPolicy, StageName } from "@/api/guardrails";
export const stages: StageName[] = ["injection", "sensitive_data", "content_safety"];
export const detectors: Detector[] = ["email", "us_ssn", "payment_card", "private_key", "cloud_access_key", "credential_assignment"];
export function emptyPolicy(): GuardrailPolicy {
  return { schema: "agentforge.guardrails/v1", version: "1", enabled: false, budget_ms: 100, max_input_bytes: 2097152, max_output_bytes: 4194304,
    stages: stages.map(stage => ({ stage, mode: "observe", directions: ["input", "output"], threshold: 1, rules: [], detectors: [] })) };
}
export function validPolicy(policy: GuardrailPolicy): boolean {
  const label = (v: string) => /^[A-Za-z0-9_./:-]{1,96}$/.test(v);
  const integer = (v: number, min: number, max: number) => Number.isInteger(v) && v >= min && v <= max;
  let total = 0;
  if (!label(policy.version) || !integer(policy.budget_ms, 5, 500) || !integer(policy.max_input_bytes, 1024, 2097152) || !integer(policy.max_output_bytes, 1024, 4194304)) return false;
  if (policy.stages.length !== 3 || new Set(policy.stages.map(s => s.stage)).size !== 3) return false;
  for (const stage of policy.stages) {
    if (!stages.includes(stage.stage) || !stage.directions.length || !integer(stage.threshold, 1, 65535) || stage.rules.length > 32 || (!stage.rules.length && !stage.detectors.length) || (stage.stage !== "sensitive_data" && stage.detectors.length)) return false;
    if (new Set(stage.rules.map(r => r.id)).size !== stage.rules.length) return false;
    for (const rule of stage.rules) {
      if (!label(rule.id) || !integer(rule.score, 1, 65535) || rule.all.length > 8 || rule.any.length > 8 || (!rule.all.length && !rule.any.length) || !integer(rule.min_any, rule.any.length ? 1 : 0, rule.any.length)) return false;
      for (const pattern of [...rule.all, ...rule.any]) { const size = new TextEncoder().encode(pattern).length; if (!size || size > 2048) return false; total += size; }
    }
  }
  // Rust owns regex syntax/complexity validation; JavaScript regex is a different language.
  return total <= 32768;
}
export type GuardrailsText = (zh: string, en: string) => string;
