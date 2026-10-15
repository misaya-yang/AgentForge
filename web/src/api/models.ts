/**
 * LLM Models API client.
 *
 * Provides CRUD operations for managing LLM models.
 */

import { api } from "@/lib/api";
import i18n from "@/i18n";

// =============================================================================
// Types
// =============================================================================

export type ModelAccessLevel = "public" | "premium" | "admin";

export type CanonicalReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "adaptive"
  | "max"
  | "ultra";

export interface ModelReasoningOption {
  id: string;
  label: string;
  aliases: string[];
  canonical_effort?: CanonicalReasoningEffort | null;
  settings: Record<string, unknown>;
}

export interface ModelCapabilityProfile {
  schema_version: 1;
  reasoning: {
    adapter_id: string;
    default_option: string;
    options: ModelReasoningOption[];
    visibility: "none" | "summary" | "stream";
    replay_policy:
      | "discard_after_turn"
      | "preserve_during_tool_turn"
      | "preserve_session";
  };
  prompt_cache: { adapter_id: string; config: Record<string, unknown> };
  native_search: {
    adapter_id: string;
    enabled: boolean;
    config: Record<string, unknown>;
  };
  tools: {
    function_calling: boolean;
    parallel_calls: boolean;
    strict_schema: boolean;
  };
  modalities: { input: string[]; output: string[] };
  streaming: {
    text_deltas: boolean;
    reasoning_deltas: boolean;
    tool_call_deltas: boolean;
  };
}

export interface ModelCapabilityAdapter {
  id: string;
  kind: "reasoning" | "prompt_cache" | "native_search";
  label: string;
  settings_schema: Record<string, unknown>;
}

export interface LLMModel {
  model_id: string;
  tenant_id: string;
  provider_id: string;
  display_name: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
  input_price_per_1k: number;
  output_price_per_1k: number;
  access_level: ModelAccessLevel;
  is_enabled: boolean;
  sort_order: number;
  catalog_capabilities: Partial<ModelCapabilityProfile>;
  capability_overrides: Partial<ModelCapabilityProfile>;
  effective_capabilities: ModelCapabilityProfile;
  capability_revision: number;
  created_at: string;
  updated_at: string;
}

export interface ModelCreate {
  model_id: string;
  provider_id: string;
  display_name: string;
  context_window?: number;
  max_output_tokens?: number;
  supports_vision?: boolean;
  supports_tools?: boolean;
  input_price_per_1k?: number;
  output_price_per_1k?: number;
  access_level?: ModelAccessLevel;
  is_enabled?: boolean;
  sort_order?: number;
  capability_overrides?: Partial<ModelCapabilityProfile>;
}

export interface ModelUpdate {
  // Renames are allowed — the server will UPDATE the primary key when this
  // differs from the path's model_id. Passing the same value (or omitting it)
  // is a no-op.
  model_id?: string;
  display_name?: string;
  context_window?: number;
  max_output_tokens?: number;
  supports_vision?: boolean;
  supports_tools?: boolean;
  input_price_per_1k?: number;
  output_price_per_1k?: number;
  access_level?: ModelAccessLevel;
  is_enabled?: boolean;
  sort_order?: number;
  capability_overrides?: Partial<ModelCapabilityProfile>;
  expected_capability_revision?: number;
}

export const modelQueryKeys = {
  all: ["models"] as const,
  byProvider: (providerId: string, includeDisabled = false) =>
    ["models", { providerId, includeDisabled }] as const,
};

// =============================================================================
// API Functions
// =============================================================================

/**
 * List all models.
 */
export async function listModels(
  providerId?: string,
  includeDisabled = false
): Promise<LLMModel[]> {
  let url = `/api/v1/models?include_disabled=${includeDisabled}`;
  if (providerId) {
    url += `&provider_id=${providerId}`;
  }
  const { data } = await api.get<LLMModel[]>(url);
  return data;
}


/**
 * Create a new model.
 */
export async function createModel(model: ModelCreate): Promise<LLMModel> {
  const { data } = await api.post<LLMModel>("/api/v1/models", model);
  return data;
}

/**
 * Update a model.
 *
 * ``providerId`` disambiguates when the same model_id exists under multiple
 * providers (migration 055); the server falls back to the first row by
 * sort_order+provider_id when it is omitted.
 */
export async function updateModel(
  modelId: string,
  updates: ModelUpdate,
  providerId?: string
): Promise<LLMModel> {
  const { data } = await api.put<LLMModel>(
    `/api/v1/models/${encodeURIComponent(modelId)}`,
    updates,
    providerId ? { params: { provider_id: providerId } } : undefined
  );
  return data;
}

/**
 * Delete a model.
 */
export async function deleteModel(modelId: string, providerId?: string): Promise<void> {
  await api.delete(`/api/v1/models/${encodeURIComponent(modelId)}`, {
    params: providerId ? { provider_id: providerId } : undefined,
  });
}

/**
 * Toggle model enabled state.
 */
export async function toggleModel(
  modelId: string,
  isEnabled: boolean,
  providerId?: string
): Promise<LLMModel> {
  const params: Record<string, string | boolean> = { is_enabled: isEnabled };
  if (providerId) {
    params.provider_id = providerId;
  }
  const { data } = await api.patch<LLMModel>(
    `/api/v1/models/${encodeURIComponent(modelId)}/toggle`,
    undefined,
    { params }
  );
  return data;
}

export async function listCapabilityAdapters(): Promise<ModelCapabilityAdapter[]> {
  const { data } = await api.get<ModelCapabilityAdapter[]>(
    "/api/v1/model-capability-adapters"
  );
  return data;
}

export async function resetModelCapabilities(
  modelId: string,
  capabilityRevision: number,
  providerId?: string
): Promise<LLMModel> {
  const params: Record<string, string | number> = {
    expected_capability_revision: capabilityRevision,
  };
  if (providerId) {
    params.provider_id = providerId;
  }
  const { data } = await api.post<LLMModel>(
    `/api/v1/models/${encodeURIComponent(modelId)}/capabilities/reset`,
    undefined,
    { params }
  );
  return data;
}

// =============================================================================
// Helper Functions
// =============================================================================

/**
 * Get access level display name.
 */
export function getAccessLevelDisplayName(level: ModelAccessLevel): string {
  const names: Record<ModelAccessLevel, string> = {
    public: i18n.t("models.access.public"),
    premium: i18n.t("models.access.premium"),
    admin: i18n.t("models.access.admin"),
  };
  return names[level] || level;
}


/**
 * Format price for display.
 * Handles string, number, null, and undefined inputs safely.
 */
export function formatPrice(price: number | string | null | undefined): string {
  // Handle null/undefined
  if (price === null || price === undefined) return "-";

  // Convert string to number if needed (PostgreSQL DECIMAL returns as string)
  const numPrice = typeof price === "string" ? parseFloat(price) : price;

  // Handle NaN or invalid values
  if (isNaN(numPrice)) return "-";

  if (numPrice === 0) return i18n.t("models.price.free");
  if (numPrice < 0.001) return `$${numPrice.toFixed(6)}`;
  if (numPrice < 0.01) return `$${numPrice.toFixed(5)}`;
  return `$${numPrice.toFixed(4)}`;
}

/**
 * Format context window for display.
 * Handles string, number, null, and undefined inputs safely.
 */
export function formatContextWindow(tokens: number | string | null | undefined): string {
  // Handle null/undefined
  if (tokens === null || tokens === undefined) return "-";

  // Convert string to number if needed
  const numTokens = typeof tokens === "string" ? parseInt(tokens, 10) : tokens;

  // Handle NaN or invalid values
  if (isNaN(numTokens)) return "-";

  if (numTokens >= 1000000) return `${(numTokens / 1000000).toFixed(1)}M`;
  if (numTokens >= 1000) return `${(numTokens / 1000).toFixed(0)}K`;
  return numTokens.toString();
}
