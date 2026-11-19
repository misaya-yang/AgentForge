import type { RequestBody, RequestQuery } from "@/api/contracts";
import { api } from "@/lib/api";
import { sameToolPolicy, type ToolPolicyPayload } from "@/lib/tenantGovernance";

export interface TenantToolPolicy extends ToolPolicyPayload {
  id: string;
  max_calls_per_session: number | null;
  created_at: string | null;
  updated_at: string | null;
}

/** Deliberately render metadata only; raw input/error fields are not part of this view. */
export interface ToolAuditEntry {
  id: number | string;
  tenant_id: string;
  user_id: string;
  tool_type: string;
  tool_name: string;
  output_status: string;
  request_id: string | null;
  session_id: string | null;
  latency_ms: number | null;
  created_at: string | null;
}
export type ToolAuditFilters = Required<Pick<RequestQuery<"GET /api/v1/admin/tenant-policies/audit-log">, "tenant_id" | "user_id" | "tool_name">>;
export interface ToolAuditPage { items: ToolAuditEntry[]; total: number }
const base = "/api/v1/admin/tenant-policies";

export async function listTenantToolPolicies(signal?: AbortSignal): Promise<TenantToolPolicy[]> {
  const { data } = await api.get<{ policies: TenantToolPolicy[] }>(`${base}/tool-policies`, { signal });
  return data.policies;
}

/** No automatic retries: a failed invalidation/readback can follow a committed write. */
export async function saveTenantToolPolicy(payload: ToolPolicyPayload): Promise<TenantToolPolicy> {
  await api.put(`${base}/tool-policies`, (payload satisfies RequestBody<"PUT /api/v1/admin/tenant-policies/tool-policies">));
  const { data } = await api.get<TenantToolPolicy>(`${base}/tool-policies/${encodeURIComponent(payload.tenant_id)}`);
  if (!sameToolPolicy(data, payload)) throw new Error("tenant_policy_readback_mismatch");
  return data;
}

export async function getToolAuditLog(filters: ToolAuditFilters, offset: number, signal?: AbortSignal): Promise<ToolAuditPage> {
  const { data } = await api.get<ToolAuditPage>(`${base}/audit-log`, {
    params: ({ tenant_id: filters.tenant_id || undefined, user_id: filters.user_id || undefined, tool_name: filters.tool_name || undefined, offset, limit: 25 } satisfies RequestQuery<"GET /api/v1/admin/tenant-policies/audit-log">),
    signal,
  });
  return data;
}
