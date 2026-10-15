import { api } from "@/lib/api";

export interface ConfigAuditFilters {
  tenant_id: string;
  user_id: string;
  resource_type: string;
  resource_id: string;
  action: string;
}
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
    params: { ...filters, offset, limit: 25 }, signal,
  });
  return data;
}
