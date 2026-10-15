export interface TenantMcpConfig {
  tenant_id: string;
  allowed_servers: string[];
  server_overrides: Record<string, unknown> | null;
  max_connections: number;
}
export interface McpConfigDraft { tenant_id: string; allowed_servers: string; max_connections: string }

/** asyncpg-compatible APIs return JSONB as text; never coerce malformed overrides to null. */
export function normalizeMcpConfig(value: unknown): TenantMcpConfig {
  if (!value || typeof value !== "object") throw new Error("mcp_config_invalid");
  const row = value as Record<string, unknown>;
  let overrides: unknown = row.server_overrides;
  if (typeof overrides === "string") overrides = JSON.parse(overrides);
  if (typeof row.tenant_id !== "string" || !Array.isArray(row.allowed_servers) || !row.allowed_servers.every(item => typeof item === "string") ||
    !Number.isInteger(row.max_connections) || (row.max_connections as number) < 1 || (row.max_connections as number) > 100 ||
    (overrides !== null && (typeof overrides !== "object" || Array.isArray(overrides)))) throw new Error("mcp_config_invalid");
  return { tenant_id: row.tenant_id, allowed_servers: [...row.allowed_servers], server_overrides: overrides as Record<string, unknown> | null, max_connections: row.max_connections as number };
}
export function mcpConfigDraft(config?: TenantMcpConfig): McpConfigDraft {
  return { tenant_id: config?.tenant_id ?? "", allowed_servers: JSON.stringify(config?.allowed_servers ?? []), max_connections: String(config?.max_connections ?? 5) };
}
export type McpValidation = { payload: TenantMcpConfig; error?: never } | { error: "tenant" | "list" | "connections"; payload?: never };
export function validateMcpConfigDraft(draft: McpConfigDraft, initial?: TenantMcpConfig): McpValidation {
  // The server owns its exact Unicode tenant-ID validation.
  if (!draft.tenant_id || [...draft.tenant_id].length > 64 || (initial && draft.tenant_id !== initial.tenant_id)) return { error: "tenant" };
  if (!/^\d+$/.test(draft.max_connections) || Number(draft.max_connections) < 1 || Number(draft.max_connections) > 100) return { error: "connections" };
  try {
    const servers: unknown = JSON.parse(draft.allowed_servers);
    if (!Array.isArray(servers) || !servers.every(server => typeof server === "string")) return { error: "list" };
    return { payload: { tenant_id: draft.tenant_id, allowed_servers: servers, max_connections: Number(draft.max_connections), server_overrides: initial?.server_overrides ?? null } };
  } catch { return { error: "list" }; }
}
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, entry]) => `${JSON.stringify(key)}:${canonical(entry)}`).join(",")}}`;
  return JSON.stringify(value);
}
export function sameMcpConfig(a: TenantMcpConfig | null, b: TenantMcpConfig | null): boolean {
  return canonical(a) === canonical(b);
}
/** Refresh before writing and verify readback; this is not atomic compare-and-swap. */
export async function saveReviewedMcpConfig(
  payload: TenantMcpConfig,
  baseline: TenantMcpConfig | null,
  read: (tenant: string) => Promise<TenantMcpConfig | null>,
  write: (payload: TenantMcpConfig) => Promise<void>,
): Promise<TenantMcpConfig> {
  if (!sameMcpConfig(await read(payload.tenant_id), baseline)) throw new Error("mcp_config_changed");
  await write(payload);
  const actual = await read(payload.tenant_id);
  // Native storage normalizes an empty override object to SQL NULL.
  const expected = { ...payload, server_overrides: payload.server_overrides && Object.keys(payload.server_overrides).length ? payload.server_overrides : null };
  if (!actual || !sameMcpConfig(actual, expected)) throw new Error("mcp_config_readback_mismatch");
  return actual;
}
