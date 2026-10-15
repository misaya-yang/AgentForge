import { isAxiosError, type AxiosRequestTransformer } from "axios";
import { api } from "@/lib/api";
import type { RoleResponse } from "./users";

export interface RoleCreateInput {
  role_name: string;
  description: string;
  permissions: string[];
}

/** Exact-name observation only; absence never authorizes replay of a submitted create. */
export async function readRole(name: string): Promise<RoleResponse | null> {
  try {
    const { data } = await api.get<RoleResponse>(`/api/v1/roles/${encodeURIComponent(name)}`);
    if (data.role_name !== name || (data.description !== null && typeof data.description !== "string")
      || !Array.isArray(data.permissions) || !data.permissions.every(code => typeof code === "string")
      || typeof data.is_system !== "boolean") throw new Error("role_receipt_invalid");
    return data;
  } catch (error) {
    if (isAxiosError(error) && error.response?.status === 404) return null;
    throw error;
  }
}

/** One transport attempt, no automatic retry or alternate registration route. */
export async function createRole(input: RoleCreateInput, mayDispatch: () => boolean): Promise<void> {
  const defaults = api.defaults.transformRequest;
  const guard: AxiosRequestTransformer = data => {
    // Runs after async authentication interceptors, immediately before transport.
    // The callback is required, contains no serialized credentials and also fences unmount.
    if (!mayDispatch()) throw new Error("role_creation_authority_changed");
    return data;
  };
  await api.post("/api/v1/roles", input, {
    transformRequest: [guard, ...(Array.isArray(defaults) ? defaults : defaults ? [defaults] : [])],
  });
}

export function roleMatches(actual: RoleResponse, input: RoleCreateInput): boolean {
  return actual.role_name === input.role_name && actual.description === input.description
    && !actual.is_system
    && JSON.stringify([...new Set(actual.permissions)].sort()) === JSON.stringify([...new Set(input.permissions)].sort());
}
