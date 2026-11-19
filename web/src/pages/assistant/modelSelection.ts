import type { ModelInfo } from "@/api/assistant";

// UI/cache identity only. API model_id stays the provider's original model ID.
export function assistantModelKey(model: Pick<ModelInfo, "id" | "provider">): string {
  return JSON.stringify([model.provider, model.id]);
}

export function resolveAssistantModel(models: ModelInfo[], selection: string): ModelInfo | undefined {
  // A legacy bare ID is accepted only when it has one possible provider.
  const matches = models.filter((model) =>
    assistantModelKey(model) === selection || model.id === selection,
  );
  return matches.length === 1 ? matches[0] : undefined;
}

export function savedAssistantModelSelection(config?: {
  selected_model?: string;
  selected_provider?: string;
} | null): string {
  if (!config?.selected_model) return "";
  return config.selected_provider
    ? assistantModelKey({ id: config.selected_model, provider: config.selected_provider })
    : config.selected_model;
}

export const PRIMARY_ASSISTANT_MODEL_ID = "qwen3.8-flash";

// The Assistant catalog already excludes disabled models. A model is usable
// in the composer only when its provider is configured for this tenant too.
export function activeAssistantModels(
  models: ModelInfo[],
  availableProviders: string[],
): ModelInfo[] {
  const activeProviders = new Set(availableProviders);
  return models.filter((model) => activeProviders.has(model.provider));
}

export function preferredAssistantModelId(
  models: ModelInfo[],
  serverDefaultId: string,
  userChoice = "",
): string {
  const preferred = resolveAssistantModel(models, userChoice)
    ?? models.find((model) => model.id === PRIMARY_ASSISTANT_MODEL_ID)
    ?? resolveAssistantModel(models, serverDefaultId)
    ?? models[0];
  return preferred ? assistantModelKey(preferred) : "";
}
