import type { RequestQuery } from "@/api/contracts";
import { api } from "@/lib/api";

export type ConfigAuditFilters = Required<Pick<RequestQuery<"GET /api/v1/admin/tenant-policies/config-audit-log">, "tenant_id" | "user_id" | "resource_type" | "resource_id" | "action">>;
export interface ConfigAuditEntry {
  id: string;
  tenant_id: string | null;
  user_id: string | null;
  resource_type: string | null;
  resource_id: string | null;
  action: string;
  status: string;
  created_at: string;
}
export interface ConfigAuditPage {
  items: ConfigAuditEntry[];
  has_more: boolean;
  limit: number;
  offset: number;
  max_offset: number;
}
export async function getConfigAuditLog(filters: ConfigAuditFilters, offset: number, signal?: AbortSignal): Promise<ConfigAuditPage> {
  const { data } = await api.get<ConfigAuditPage>("/api/v1/admin/tenant-policies/config-audit-log", {
    params: ({ ...filters, offset, limit: 25 } satisfies RequestQuery<"GET /api/v1/admin/tenant-policies/config-audit-log">), signal,
  });
  return data;
}
