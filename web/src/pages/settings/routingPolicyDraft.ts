import type { RoutingPolicy } from "@/api/routingPolicy";
export function parseRoutingPolicy(text: string, tenant: string): RoutingPolicy {
  const p = JSON.parse(text) as RoutingPolicy;
  const identifier = (value: unknown, max = 512) => typeof value === "string" && value.length > 0 && new TextEncoder().encode(value).byteLength <= max && value === value.trim() && !/\p{Cc}/u.test(value);
  const uint = (n: unknown, max = Number.MAX_SAFE_INTEGER): n is number => typeof n === "number" && Number.isSafeInteger(n) && n >= 0 && n <= max;
  if (!p || p.schema !== "agentforge.routing_policy/v1" || p.tenant !== tenant || p.cost_currency !== "USD"
    || !identifier(p.policy_id, 128) || !identifier(p.version, 128)
    || !["priority", "weighted", "lowest_latency", "lowest_cost"].includes(p.strategy)
    || !Array.isArray(p.candidates) || p.candidates.length < 1 || p.candidates.length > 128
    || !Array.isArray(p.required_capabilities) || p.required_capabilities.length > 64 || !p.required_capabilities.every(value => identifier(value))
    || !uint(p.max_attempts, 8) || p.max_attempts < 1 || !uint(p.metric_max_age_ms, 3_600_000) || p.metric_max_age_ms < 1
    || (p.max_latency_ms !== null && !uint(p.max_latency_ms)) || (p.max_cost_nanos !== null && !uint(p.max_cost_nanos))) throw new Error("routing_policy_invalid");
  const allowed = ["schema", "tenant", "policy_id", "version", "cost_currency", "strategy", "candidates", "required_capabilities", "max_latency_ms", "max_cost_nanos", "metric_max_age_ms", "max_attempts"];
  if (Object.keys(p).some(key => !allowed.includes(key)) || new Set(p.required_capabilities).size !== p.required_capabilities.length) throw new Error("routing_policy_invalid");
  const identities = new Set<string>();
  for (const candidate of p.candidates) {
    if (!candidate?.target || Object.keys(candidate).sort().join() !== "priority,target,weight" || !Object.values(candidate.target).every(value => identifier(value)) || Object.keys(candidate.target).sort().join() !== "model,provider,version"
      || !uint(candidate.priority, 4_294_967_295) || !uint(candidate.weight, 1_000_000) || candidate.weight < 1) throw new Error("routing_candidate_invalid");
    const id = JSON.stringify([candidate.target.provider, candidate.target.model, candidate.target.version]);
    if (identities.has(id)) throw new Error("routing_candidate_duplicate"); identities.add(id);
  }
  // The native BTreeSet canonicalizes capability order before pinning/readback.
  return { ...p, required_capabilities: [...p.required_capabilities].sort() };
}
export function initialRoutingPolicy(tenant: string): RoutingPolicy {
  return { schema: "agentforge.routing_policy/v1", tenant, policy_id: "", version: "", cost_currency: "USD",
    strategy: "priority", candidates: [], required_capabilities: [], max_latency_ms: null, max_cost_nanos: null, metric_max_age_ms: 60000, max_attempts: 1 };
}
/** Stable comparison only; the server's JCS pin remains the authority. */
export function routingPolicyFingerprint(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(routingPolicyFingerprint).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, item]) => `${JSON.stringify(key)}:${routingPolicyFingerprint(item)}`).join(",")}}`;
  return JSON.stringify(value);
}
