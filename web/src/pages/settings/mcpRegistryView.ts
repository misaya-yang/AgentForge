export interface RegistryServer { server_id: string; name: string; transport: string; enabled: boolean; health_status: string }
export interface RegistryConnection { connection_id: string; server_id: string; principal_type: "service_account" | "user_delegated"; enabled: boolean; revoked_at: string | null; expires_at: string | null }
export interface RegistryTool { tool_id: string; server_id: string; runtime_name: string; snapshot_id: string; schema_hash: string; schema_version: number; risk_level: string; enabled: boolean; read_only: boolean; input_schema: { properties?: Record<string, unknown>; required?: string[] } }
const object = (value: unknown): value is Record<string, unknown> => Boolean(value && typeof value === "object" && !Array.isArray(value));
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0 && !/\p{Cc}/u.test(value);
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
function rows(value: unknown, field: string): unknown[] {
  if (!object(value) || !Array.isArray(value[field])) throw new Error("MCP registry unavailable");
  return value[field];
}
export function registryServers(value: unknown): RegistryServer[] {
  return rows(value, "servers").map(row => {
    if (!object(row) || !text(row.server_id) || !text(row.name) || !text(row.transport) || !text(row.health_status) || typeof row.enabled !== "boolean") throw new Error("MCP server metadata invalid");
    return { server_id: row.server_id, name: row.name, transport: row.transport, health_status: row.health_status, enabled: row.enabled };
  });
}
export function registryConnections(value: unknown, server: string, now = Date.now()): RegistryConnection[] {
  return rows(value, "connections").map(row => {
    if (!object(row) || row.server_id !== server || !uuid(row.connection_id) || !["service_account", "user_delegated"].includes(String(row.principal_type)) || typeof row.enabled !== "boolean"
      || (row.revoked_at !== null && !text(row.revoked_at)) || (row.expires_at !== null && (!text(row.expires_at) || !Number.isFinite(Date.parse(row.expires_at))))) throw new Error("MCP connection metadata invalid");
    return { connection_id: row.connection_id, server_id: server, principal_type: row.principal_type as RegistryConnection["principal_type"], enabled: row.enabled, revoked_at: row.revoked_at as string | null, expires_at: row.expires_at as string | null };
  }).filter(row => row.enabled && !row.revoked_at && (!row.expires_at || Date.parse(row.expires_at) > now));
}
export function registryTools(value: unknown, server: string): RegistryTool[] {
  return rows(value, "tools").map(row => {
    if (!object(row) || row.server_id !== server || !text(row.tool_id) || !text(row.runtime_name) || !text(row.snapshot_id) || !/^[a-f0-9]{64}$/i.test(String(row.schema_hash))
      || typeof row.schema_version !== "number" || !Number.isSafeInteger(row.schema_version) || row.schema_version < 1 || !text(row.risk_level) || typeof row.enabled !== "boolean" || typeof row.read_only !== "boolean" || !object(row.input_schema)
      || (row.input_schema.properties !== undefined && !object(row.input_schema.properties))
      || (row.input_schema.required !== undefined && (!Array.isArray(row.input_schema.required) || !row.input_schema.required.every(text)))) throw new Error("MCP tool snapshot invalid");
    return { tool_id: row.tool_id, server_id: server, runtime_name: row.runtime_name, snapshot_id: row.snapshot_id, schema_hash: row.schema_hash as string, schema_version: row.schema_version,
      risk_level: row.risk_level, enabled: row.enabled, read_only: row.read_only, input_schema: { properties: row.input_schema.properties as Record<string, unknown> | undefined, required: row.input_schema.required as string[] | undefined } };
  });
}
