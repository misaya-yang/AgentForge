import type { RequestBody, Defined } from "@/api/contracts";
import { api } from "@/lib/api";
export type RoutingPolicy = Defined<RequestBody<"POST /api/v1/routing/policies">, "schema" | "tenant" | "policy_id" | "version" | "cost_currency" | "strategy" | "candidates" | "required_capabilities" | "max_latency_ms" | "max_cost_nanos" | "metric_max_age_ms" | "max_attempts">;
const base = "/api/v1/routing";
export async function readRoutingPolicy(id: string, version: string, signal?: AbortSignal): Promise<RoutingPolicy> {
  return (await api.get<RoutingPolicy>(`${base}/policies/${encodeURIComponent(id)}/versions/${encodeURIComponent(version)}`, { signal })).data;
}
export async function publishRoutingPolicy(policy: RoutingPolicy): Promise<string> {
  const { data } = await api.post<{ policy_pin: string }>(`${base}/policies`, (policy satisfies RequestBody<"POST /api/v1/routing/policies">));
  if (typeof data.policy_pin !== "string" || !/^[a-f0-9]{64}$/.test(data.policy_pin)) throw new Error("routing_receipt_invalid");
  return data.policy_pin;
}
export async function activateRoutingPolicy(id: string, version: string, expectedPin: string | null): Promise<string> {
  const { data } = await api.post<{ policy_pin: string }>(`${base}/policies/${encodeURIComponent(id)}/activate`, ({ version, expected_pin: expectedPin } satisfies RequestBody<"POST /api/v1/routing/policies/{id}/activate">));
  if (typeof data.policy_pin !== "string" || !/^[a-f0-9]{64}$/.test(data.policy_pin)) throw new Error("routing_receipt_invalid");
  return data.policy_pin;
}
export async function readRoutingDecision(id: string): Promise<unknown> {
  return (await api.get<unknown>(`${base}/decisions/${encodeURIComponent(id)}`)).data;
}
