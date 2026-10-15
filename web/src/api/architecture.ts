import { api } from "@/lib/api";

export interface ArchitectureDependency {
  service_id: string;
  required: boolean;
  status: string;
}

export interface ArchitectureServiceStatus {
  service_id: string;
  display_name: string;
  bounded_context: string;
  responsibility: string;
  lifecycle: string;
  exposure: string;
  status: string;
  version: string;
  state_owner: string;
  scale_support: string;
  replicas: number;
  active_in_mode: boolean;
  dependencies: ArchitectureDependency[];
  degraded_reasons: string[];
  last_check: string | null;
}

export interface ArchitectureGroupStatus {
  group_id: string;
  display_name: string;
  services: ArchitectureServiceStatus[];
}

export interface ArchitectureStatus {
  schema_version: "ai-gateway/architecture-status/v1";
  topology_revision: string;
  mode: "compact" | "full" | "scale";
  mode_configuration_valid: boolean;
  last_check: string;
  groups: ArchitectureGroupStatus[];
}

export async function getArchitectureStatus(): Promise<ArchitectureStatus> {
  const { data } = await api.get<ArchitectureStatus>("/api/v1/admin/architecture-status");
  return data;
}
