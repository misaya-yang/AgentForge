import { api } from "@/lib/api";

export interface APIKeyListItem {
  key_id: string;
  key_prefix?: string | null;
  name: string;
  description?: string | null;
  user_id?: string | null;
  derived_user_id?: string | null;
  tier?: string | null;
  is_active: boolean;
  created_at: string;
  last_used_at?: string | null;
  use_count: number;
}

export async function listApiKeys(includeInactive = false): Promise<APIKeyListItem[]> {
  const response = await api.get<APIKeyListItem[]>("/api/v1/api-keys", {
    params: {
      include_inactive: includeInactive,
    },
  });
  return response.data || [];
}
