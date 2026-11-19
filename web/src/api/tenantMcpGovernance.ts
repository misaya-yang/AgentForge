import type { RequestBody } from "@/api/contracts";
import { isHttpError } from '@/lib/fetchClient';
import { useAuthStore } from "@/store/useAuthStore";
import { api } from "@/lib/api";
import { normalizeMcpConfig, saveReviewedMcpConfig, type TenantMcpConfig } from "@/lib/tenantMcpGovernance";

const base = "/api/v1/admin/tenant-policies/mcp-configs";
export async function listTenantMcpConfigs(signal?: AbortSignal): Promise<TenantMcpConfig[]> {
  const { data } = await api.get<{ configs: unknown[] }>(base, { signal });
  return data.configs.map(normalizeMcpConfig);
}
async function readTenantMcpConfig(tenant: string, signal: AbortSignal): Promise<TenantMcpConfig | null> {
  try {
    const { data } = await api.get<unknown>(`${base}/${encodeURIComponent(tenant)}`, { signal });
    return normalizeMcpConfig(data);
  } catch (error) {
    if (isHttpError(error) && error.response?.status === 404) return null;
    throw error;
  }
}
export async function saveTenantMcpConfig({ payload, baseline, identity }: { payload: TenantMcpConfig; baseline: TenantMcpConfig | null; identity: string }): Promise<TenantMcpConfig> {
  const token = useAuthStore.getState().token;
  const controller = new AbortController();
  const checkIdentity = () => {
    const state = useAuthStore.getState();
    const user = state.user;
    if (!token || state.token !== token || !user || JSON.stringify([user.user_id, user.roles, user.tier]) !== identity) controller.abort();
  };
  const unsubscribe = useAuthStore.subscribe(checkIdentity);
  const guard = () => { checkIdentity(); controller.signal.throwIfAborted(); };
  try {
    guard();
    return await saveReviewedMcpConfig(payload, baseline, async tenant => {
      guard();
      const result = await readTenantMcpConfig(tenant, controller.signal);
      guard();
      return result;
    }, async config => {
      guard();
      await api.put(base, (config satisfies RequestBody<"PUT /api/v1/admin/tenant-policies/mcp-configs">), { signal: controller.signal });
      guard();
    });
  } finally { unsubscribe(); }
}
