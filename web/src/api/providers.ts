import type { RequestBody } from "@/api/contracts";
import type { ApiRequest } from "../../../sdk/typescript/generated";
/**
 * LLM Providers API client.
 *
 * Provides CRUD operations for managing LLM providers.
 */

import { api } from "@/lib/api";
import { request, readOptions, readKey } from "./generatedTransport";

// =============================================================================
// Types
// =============================================================================

export interface Provider {
  provider_id: string;
  tenant_id: string;
  display_name: string;
  api_type: string;
  base_url?: string | null;
  metadata?: Record<string, unknown>;
  has_api_key: boolean;
  allow_environment_credentials?: boolean;
  is_enabled: boolean;
  created_at: string;
  updated_at: string;
}

export type ProviderCreate = ApiRequest<"POST /api/v1/providers">["body"];

export interface ProviderTemplateCredentialField {
  name: string;
  label: string;
  field_type: "password" | "text" | string;
  required: boolean;
  placeholder?: string | null;
}

export interface ProviderTemplateModel {
  model_id: string;
  display_name: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
  input_price_per_1k: number;
  output_price_per_1k: number;
  access_level: string;
  sort_order: number;
  catalog_capabilities?: import("./models").ModelCapabilityProfile;
}

export interface ProviderTemplate {
  template_id: string;
  display_name: string;
  description: string;
  default_provider_id: string;
  api_type: string;
  default_base_url: string;
  credential_fields: ProviderTemplateCredentialField[];
  discovery_strategy: string;
  default_models: ProviderTemplateModel[];
  advanced: boolean;
}

export type ProviderFromTemplateCreate = ApiRequest<"POST /api/v1/providers/from-template">["body"];

export type ProviderUpdate = ApiRequest<"PUT /api/v1/providers/{provider_id}">["body"];

export interface ProviderTestResult {
  success: boolean;
  message: string;
  latency_ms?: number;
}

export interface ProviderModelSyncItem {
  model_id: string;
  provider_id: string;
  display_name: string;
  is_enabled: boolean;
}

export interface ProviderModelSyncSkipped {
  model_id: string;
  reason: string;
}

export interface ProviderModelSyncResult {
  provider_id: string;
  template_id?: string | null;
  created_models: ProviderModelSyncItem[];
  updated_models: ProviderModelSyncItem[];
  skipped_models: ProviderModelSyncSkipped[];
  discovery_warnings: string[];
}

export const providerQueryKeys = {
  all: ["providers"] as const,
  list: (scope: string, includeDisabled = false) => ["providers", ...readKey(scope, "GET /api/v1/providers", { query: { include_disabled: includeDisabled } })] as const,
  templates: ["provider-templates"] as const,
};

// =============================================================================
// API Functions
// =============================================================================

/**
 * List all providers.
 */
export async function listProviders(includeDisabled = false): Promise<Provider[]> {
  return request("GET /api/v1/providers", { query: { include_disabled: includeDisabled } });
}

/**
 * List guided provider templates.
 */
export async function listProviderTemplates(): Promise<ProviderTemplate[]> {
  const { data } = await api.get<ProviderTemplate[]>("/api/v1/provider-templates");
  return data;
}


/**
 * Create a new provider.
 */
export async function createProvider(provider: ProviderCreate): Promise<Provider> {
  return request("POST /api/v1/providers", { body: provider });
}

/**
 * Create a provider from a guided template.
 */
export async function createProviderFromTemplate(
  provider: ProviderFromTemplateCreate
): Promise<Provider> {
  return request("POST /api/v1/providers/from-template", { body: provider });
}

/**
 * Update a provider.
 */
export async function updateProvider(
  providerId: string,
  updates: ProviderUpdate
): Promise<Provider> {
  return request("PUT /api/v1/providers/{provider_id}", { path: { provider_id: providerId }, body: updates });
}

/**
 * Delete a provider.
 */
export async function deleteProvider(providerId: string): Promise<void> {
  await request("DELETE /api/v1/providers/{provider_id}", { path: { provider_id: providerId } });
}

/**
 * Test provider API connection.
 */
export async function testProviderConnection(providerId: string): Promise<ProviderTestResult> {
  const { data } = await api.post<ProviderTestResult>(
    `/api/v1/providers/${providerId}/test`
  );
  return data;
}

/**
 * Sync provider-supported models from the Gateway catalog/discovery layer.
 */
export async function syncProviderModels(providerId: string): Promise<ProviderModelSyncResult> {
  const { data } = await api.post<ProviderModelSyncResult>(
    `/api/v1/providers/${providerId}/models/sync`
  );
  return data;
}

// =============================================================================
// Helper Functions
// =============================================================================

/**
 * Get API type display name.
 */
export function getApiTypeDisplayName(apiType: string): string {
  const names: Record<string, string> = {
    openai: "OpenAI Compatible",
    dashscope: "DashScope",
    anthropic: "Anthropic",
    google: "Google Gemini",
    "google-vertex": "Google Vertex AI",
  };
  return names[apiType] || apiType;
}

/**
 * Get default base URL for API type.
 */
export function getDefaultBaseUrl(apiType: string): string {
  const urls: Record<string, string> = {
    openai: "https://api.openai.com",
    dashscope: "https://dashscope.aliyuncs.com/compatible-mode",
    anthropic: "https://api.anthropic.com",
    google: "https://generativelanguage.googleapis.com",
    "google-vertex": "https://aiplatform.googleapis.com",
  };
  return urls[apiType] || "";
}

/** Native-only reviewed sync. A 404/503 must never fall back to legacy sync. */
export async function previewProviderModels(providerId: string) {
  const { data } = await api.post<import("@/components/llm/catalogReviewState").CatalogPreview>(`/api/v1/providers/${encodeURIComponent(providerId)}/models/sync/preview`);
  return data;
}
export async function applyProviderModelReview(providerId: string, body: RequestBody<"POST /api/v1/providers/{provider_id}/models/sync/apply">) {
  const { data } = await api.post<import("@/components/llm/catalogReviewState").CatalogReadback>(`/api/v1/providers/${encodeURIComponent(providerId)}/models/sync/apply`, (body satisfies RequestBody<"POST /api/v1/providers/{provider_id}/models/sync/apply">));
  return data;
}
export async function readProviderModelReview(providerId: string, reviewId: string) {
  const { data } = await api.get<import("@/components/llm/catalogReviewState").CatalogReadback>(`/api/v1/providers/${encodeURIComponent(providerId)}/models/sync/reviews/${encodeURIComponent(reviewId)}`);
  return data;
}

/** Scope is the current tenant/user/auth generation; never reuse across sessions. */
export function providersOptions(scope: string, includeDisabled = false) {
  return { ...readOptions(scope, "GET /api/v1/providers", { query: { include_disabled: includeDisabled } }), queryKey: providerQueryKeys.list(scope, includeDisabled) };
}
