import type { RequestBody, Defined } from "@/api/contracts";
/** Mirrors tenant_policies_api authorization; platform_admin alone is not sufficient. */
export function canAdministerTenantPolicies(user: { roles: string[]; tier: string } | null | undefined): boolean {
  return Boolean(user && (user.roles.includes("admin") || user.tier.toLowerCase() === "admin"));
}

export type ToolPolicyPayload = Defined<RequestBody<"PUT /api/v1/admin/tenant-policies/tool-policies">, "tenant_id" | "allowed_tools" | "blocked_tools" | "allowed_categories" | "max_calls_per_minute">;

export interface ToolPolicyDraft {
  tenant_id: string;
  allowed_tools: string;
  blocked_tools: string;
  allowed_categories: string;
  max_calls_per_minute: string;
}

export function policyDraft(policy?: ToolPolicyPayload): ToolPolicyDraft {
  return {
    tenant_id: policy?.tenant_id ?? "",
    allowed_tools: JSON.stringify(policy?.allowed_tools ?? null),
    blocked_tools: JSON.stringify(policy?.blocked_tools ?? null),
    allowed_categories: JSON.stringify(policy?.allowed_categories ?? null),
    max_calls_per_minute: String(policy?.max_calls_per_minute ?? 20),
  };
}

export type PolicyValidation = { payload: ToolPolicyPayload; error?: never } | {
  error: "tenant" | "list" | "rate"; payload?: never;
};

/** Preserve null and empty arrays instead of silently broadening a persisted policy. */
export function validatePolicyDraft(draft: ToolPolicyDraft): PolicyValidation {
  if (!draft.tenant_id || [...draft.tenant_id].length > 64) return { error: "tenant" };
  if (!/^\d+$/.test(draft.max_calls_per_minute)) return { error: "rate" };
  const rate = Number(draft.max_calls_per_minute);
  if (!Number.isInteger(rate) || rate < 1 || rate > 10_000) return { error: "rate" };
  try {
    const fields = ["allowed_tools", "blocked_tools", "allowed_categories"] as const;
    const lists = fields.map(field => {
      const value: unknown = JSON.parse(draft[field]);
      if (value !== null && (!Array.isArray(value) || !value.every(item => typeof item === "string"))) throw new Error("list");
      return value as string[] | null;
    });
    return { payload: { tenant_id: draft.tenant_id, allowed_tools: lists[0], blocked_tools: lists[1], allowed_categories: lists[2], max_calls_per_minute: rate } };
  } catch {
    return { error: "list" };
  }
}

export function sameToolPolicy(actual: ToolPolicyPayload, expected: ToolPolicyPayload): boolean {
  return actual.tenant_id === expected.tenant_id && actual.max_calls_per_minute === expected.max_calls_per_minute &&
    (["allowed_tools", "blocked_tools", "allowed_categories"] as const).every(field => JSON.stringify(actual[field]) === JSON.stringify(expected[field]));
}
