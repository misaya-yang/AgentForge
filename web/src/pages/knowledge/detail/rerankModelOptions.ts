export interface RerankPresetModelSource {
  config?: {
    rerank?: {
      enabled?: unknown;
      model?: unknown;
      provider?: unknown;
    };
  };
}

function modelName(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const model = value.trim();
  return model.length > 0 && model.length <= 256 ? model : null;
}

export function getRerankModelOptions(
  configuredModel: unknown,
  configuredProvider: unknown,
  presets: readonly RerankPresetModelSource[]
): string[] {
  const models = new Set<string>();
  const configured = modelName(configuredModel);
  const provider = modelName(configuredProvider);
  if (configured) models.add(configured);
  for (const preset of presets) {
    const presetProvider = modelName(preset.config?.rerank?.provider);
    if (!provider || !presetProvider || presetProvider !== provider) continue;
    const model = modelName(preset.config?.rerank?.model);
    if (model) models.add(model);
  }
  return [...models];
}

export function getDefaultRerankModel(
  configuredModel: unknown,
  configuredProvider: unknown,
  presets: readonly RerankPresetModelSource[]
): string {
  const configured = modelName(configuredModel);
  if (configured) return configured;
  const provider = modelName(configuredProvider);
  if (!provider) return "";

  const matchingPresets = presets.filter((preset) => preset.config?.rerank?.provider === provider);
  const enabledPreset = matchingPresets.find((preset) => preset.config?.rerank?.enabled === true);
  const enabledModel = modelName(enabledPreset?.config?.rerank?.model);
  if (enabledModel) return enabledModel;

  return matchingPresets.map((preset) => modelName(preset.config?.rerank?.model))
    .find((model): model is string => model !== null) ?? "";
}
