import { api } from "@/lib/api";
export type StageName = "injection" | "sensitive_data" | "content_safety";
export type Direction = "input" | "output";
export type Detector = "email" | "us_ssn" | "payment_card" | "private_key" | "cloud_access_key" | "credential_assignment";
export interface GuardrailRule { id: string; all: string[]; any: string[]; min_any: number; score: number }
export interface GuardrailStage { stage: StageName; mode: "enforce" | "observe"; directions: Direction[]; threshold: number; rules: GuardrailRule[]; detectors: Detector[] }
export interface GuardrailPolicy { schema: "agentforge.guardrails/v1"; version: string; enabled: boolean; budget_ms: number; max_input_bytes: number; max_output_bytes: number; stages: GuardrailStage[] }
export interface PolicySnapshot { policy: GuardrailPolicy | null; policy_pin: string | null }
export interface DecisionReport {
  schema: string; policy_pin: string; policy_version: string; direction: Direction;
  decision: string; reason: string; elapsed_us: number; audit_state: string; audit_us: number | null; holdback_us: number;
  stages: { stage: StageName; mode: string; score: number; triggered: boolean; elapsed_us: number; findings: { rule: string; location: string }[] }[];
}
export interface DecisionLog { request_id: string; decisions: { audit_id: number; decision: DecisionReport; created_at: string }[] }
const base = "/api/v1/guardrails";
export async function getGuardrailPolicy(signal?: AbortSignal): Promise<PolicySnapshot> {
  const { data } = await api.get<PolicySnapshot>(`${base}/policy`, { signal });
  return data;
}
/** A failed response may follow a commit: never retry a write automatically. */
export async function putGuardrailPolicy(input: { policy: GuardrailPolicy; expected_pin: string | null }): Promise<PolicySnapshot> {
  const { data } = await api.put<{ policy_pin: string }>(`${base}/policy`, input);
  const current = await getGuardrailPolicy();
  if (current.policy_pin !== data.policy_pin) throw new Error("guardrails_readback_unconfirmed");
  return current;
}
export async function getGuardrailDecisions(requestId: string, signal?: AbortSignal): Promise<DecisionLog> {
  if (!requestId.trim() || requestId.length > 255 || Array.from(requestId).some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)) throw new Error("guardrails_request_invalid");
  const { data } = await api.get<DecisionLog>(`${base}/decisions/${encodeURIComponent(requestId)}`, { signal });
  return data;
}
