import type { RequestQuery, RequestBody } from "@/api/contracts";
import type { ApiRequest } from "../../../sdk/typescript/generated";
import { api } from "@/lib/api";

export type KnowledgeKeyScope = "knowledge:read" | "knowledge:write";
export interface KnowledgeKeyMetadata {
  key_id: string;
  key_prefix: string | null;
  name: string;
  user_id: string | null;
  scopes: string[];
  roles: string[];
  tier: string | null;
  is_active: boolean;
  created_at: string;
  expires_at: string | null;
  last_used_at: string | null;
  use_count: number;
}
/** Issuance UI requires an expiry and narrows the generated server scope list. */
export type KnowledgeKeyInput = Required<Pick<ApiRequest<"POST /api/v1/api-keys">["body"], "name" | "scopes" | "expires_at">> & {
  scopes: KnowledgeKeyScope[]; expires_at: Extract<ApiRequest<"POST /api/v1/api-keys">["body"]["expires_at"], string>;
};
const base = "/api/v1/api-keys";

// Project metadata explicitly: never retain an unexpected secret in list state.
export function keyMetadata(value: unknown): KnowledgeKeyMetadata {
  if (!value || typeof value !== "object") throw new Error("Invalid key metadata");
  const row = value as Record<string, unknown>;
  if (typeof row.key_id !== "string" || !row.key_id || typeof row.name !== "string"
      || typeof row.is_active !== "boolean" || typeof row.created_at !== "string"
      || (row.scopes != null && (!Array.isArray(row.scopes) || !row.scopes.every(scope => typeof scope === "string")))) {
    throw new Error("Invalid key metadata");
  }
  const optional = (name: string) => typeof row[name] === "string" ? row[name] as string : null;
  return { key_id: row.key_id, name: row.name, is_active: row.is_active,
    created_at: row.created_at, scopes: Array.isArray(row.scopes) ? [...row.scopes] as string[] : [],
    roles: Array.isArray(row.roles) ? row.roles.filter((role): role is string => typeof role === "string") : [],
    tier: optional("tier"), key_prefix: optional("key_prefix"), user_id: optional("user_id"),
    expires_at: optional("expires_at"), last_used_at: optional("last_used_at"),
    use_count: typeof row.use_count === "number" && Number.isFinite(row.use_count) ? row.use_count : 0 };
}
export async function listKnowledgeKeys(signal?: AbortSignal): Promise<KnowledgeKeyMetadata[]> {
  const { data } = await api.get<unknown>(base, { params: ({ include_inactive: true } satisfies RequestQuery<"GET /api/v1/api-keys">), signal });
  if (!Array.isArray(data)) throw new Error("Invalid key list");
  return data.map(keyMetadata);
}
export async function issueKnowledgeKey(input: KnowledgeKeyInput): Promise<{ metadata: KnowledgeKeyMetadata; secret: string }> {
  // Direct request, no mutation/query cache, storage, telemetry or automatic retry.
  const { data } = await api.post<Record<string, unknown>>(base, (input satisfies RequestBody<"POST /api/v1/api-keys">));
  const metadata = keyMetadata(data);
  if (metadata.name !== input.name || metadata.scopes.slice().sort().join(",") !== input.scopes.slice().sort().join(",")
      || Date.parse(metadata.expires_at ?? "") !== Date.parse(input.expires_at)) throw new Error("Mismatched issuance receipt");
  if (typeof data.api_key !== "string" || !data.api_key) throw new Error("Incomplete issuance receipt");
  return { metadata, secret: data.api_key };
}
export async function revokeKnowledgeKey(id: string): Promise<void> {
  const { data } = await api.post<{ key_id?: unknown }>(`${base}/${encodeURIComponent(id)}/revoke`);
  if (data?.key_id !== id) throw new Error("Invalid revocation receipt");
}
