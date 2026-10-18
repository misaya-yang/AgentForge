import type { RequestBody } from "@/api/contracts";
import type { ApiRequest } from "../../../sdk/typescript/generated";
import { api } from "@/lib/api";

export type PluginAvailability = "available" | "partially_available" | "policy_blocked";
export interface PluginCapability {
  id: string;
  name: string;
  version: string;
  schema_hash: string;
  category: string;
  risk_level: string;
  read_only: boolean;
  required_permissions: string[];
  declared_permissions: string[];
  available: boolean;
}
export interface PluginVersion {
  version: string;
  availability: PluginAvailability;
  required_permissions: string[];
  declared_permissions: string[];
  capabilities: PluginCapability[];
}
export interface PluginLifecycle {
  managed: boolean;
  enabled: boolean;
  selected_version: string | null;
  revision: string;
  selected_catalog_revision: string | null;
  selection_status: "deployment_default" | "current" | "version_unavailable" | "catalog_changed";
  updated_at: string | null;
}
export interface PluginRegistryEntry {
  id: string;
  versions: PluginVersion[];
  lifecycle: PluginLifecycle;
}
export interface PluginRegistry {
  schema_version: "ai-platform-plugin-registry/v1";
  catalog_revision: number;
  catalog_revision_token: string;
  tenant_id: string;
  can_manage: boolean;
  package_installation_supported: false;
  plugins: PluginRegistryEntry[];
}

/** Deployment attribution plus tenant-local admission preferences, never an executable installer. */
export async function getPluginRegistry(): Promise<PluginRegistry> {
  const { data } = await api.get<PluginRegistry>("/api/v1/assistant/plugins");
  return data;
}

export type UpdatePluginLifecycle = ApiRequest<"PUT /api/v1/assistant/plugins/{plugin_id}/lifecycle">["body"];
export async function updatePluginLifecycle(pluginId: string, payload: UpdatePluginLifecycle): Promise<{ plugin_id: string; tenant_id: string; lifecycle: PluginLifecycle }> {
  const { data } = await api.put<{ plugin_id: string; tenant_id: string; lifecycle: PluginLifecycle }>(
    `/api/v1/assistant/plugins/${encodeURIComponent(pluginId)}/lifecycle`, (payload satisfies RequestBody<"PUT /api/v1/assistant/plugins/{plugin_id}/lifecycle">),
  );
  return data;
}
