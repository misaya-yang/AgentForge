import type { RequestBody, Defined } from "@/api/contracts";
/**
 * Connector catalog API client.
 *
 * CRUD for admin-managed connector provider definitions (`/connectors/admin`).
 * `client_secret` is write-only: it is accepted on create/update but never
 * returned by the API.
 */

import { api } from "@/lib/api";

// =============================================================================
// Types
// =============================================================================

export type ConnectorMode = "live" | "ingest" | "both";

export interface ConnectorAuthConfig {
  client_id?: string;
  client_secret?: string | null;
  auth_url?: string;
  token_url?: string;
  scopes?: string;
  redirect_uri?: string | null;
}

export type ConnectorMcpToolInfo = NonNullable<RequestBody<"POST /api/v1/connectors/admin/configs">["mcp_tools"]>[number];

export interface ConnectorProvider {
  provider: string;
  display_name: string;
  description?: string | null;
  icon_url?: string | null;
  mode: ConnectorMode;
  enabled: boolean;
  supports_sync: boolean;
  supports_search: boolean;
  auth?: ConnectorAuthConfig | null;
  mcp_tools?: ConnectorMcpToolInfo[];
  extra_config?: Record<string, unknown>;
  tenant_id: string;
  created_at?: string | null;
  updated_at?: string | null;
}

export type ConnectorProviderCreate = Defined<RequestBody<"POST /api/v1/connectors/admin/configs">, "provider" | "display_name">;

export type ConnectorProviderUpdate = RequestBody<"PUT /api/v1/connectors/admin/configs/{provider}">;

export const connectorQueryKeys = {
  all: ["connectors"] as const,
  list: () => ["connectors", "list"] as const,
};

// =============================================================================
// API Functions
// =============================================================================

/** List catalog definitions visible to the caller's tenant. */
export async function listConnectors(): Promise<ConnectorProvider[]> {
  const { data } = await api.get<ConnectorProvider[]>("/api/v1/connectors/admin/configs");
  return data;
}

/** Create a tenant-scoped catalog definition. */
export async function createConnector(
  payload: ConnectorProviderCreate
): Promise<ConnectorProvider> {
  const { data } = await api.post<ConnectorProvider>("/api/v1/connectors/admin/configs", (payload satisfies RequestBody<"POST /api/v1/connectors/admin/configs">));
  return data;
}

/** Update a catalog definition. */
export async function updateConnector(
  provider: string,
  payload: ConnectorProviderUpdate
): Promise<ConnectorProvider> {
  const { data } = await api.put<ConnectorProvider>(
    `/api/v1/connectors/admin/configs/${encodeURIComponent(provider)}`,
    (payload satisfies RequestBody<"PUT /api/v1/connectors/admin/configs/{provider}">)
  );
  return data;
}

/** Enable or disable a provider without a full update. */
export async function toggleConnector(provider: string, enabled: boolean): Promise<ConnectorProvider> {
  const { data } = await api.patch<ConnectorProvider>(
    `/api/v1/connectors/admin/configs/${encodeURIComponent(provider)}/enabled`,
    ({ enabled } satisfies RequestBody<"PATCH /api/v1/connectors/admin/configs/{provider}/enabled">)
  );
  return data;
}

/** Delete a catalog definition; 409 while users are still connected. */
export async function deleteConnector(provider: string): Promise<void> {
  await api.delete(`/api/v1/connectors/admin/configs/${encodeURIComponent(provider)}`);
}
