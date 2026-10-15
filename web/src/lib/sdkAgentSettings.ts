import type { ServiceDetail } from "../types/gateway";

export interface SdkAgentSettings {
  model: string;
  instructions: string;
  max_turns: number;
}
export interface SavedSdkAgentSettings {
  serviceId: string;
  framework: "openai_agents" | "claude_agent_sdk";
  settings: SdkAgentSettings;
  metadata: Record<string, unknown>;
  keyConfigured: boolean;
}
const bytes = (value: string) => new TextEncoder().encode(value).length;
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

export function validSdkAgentSettings(value: unknown): value is SdkAgentSettings {
  if (!object(value)) return false;
  return typeof value.model === "string" && Boolean(value.model.trim()) && bytes(value.model) <= 160 &&
    typeof value.instructions === "string" && bytes(value.instructions) <= 32768 &&
    typeof value.max_turns === "number" && Number.isInteger(value.max_turns) && value.max_turns >= 1 && value.max_turns <= 32;
}

/** Project only the public SDK configuration. Never retain the connector/credential dictionary. */
export function readSdkAgentSettings(service: ServiceDetail, serviceId: string): SavedSdkAgentSettings {
  const metadata = service.metadata;
  const connector = service.connector_config;
  if (object(connector) && ("sdk_provider_api_key" in connector || "sdk_provider_api_key_encrypted" in connector)) {
    throw new Error("sdk_write_only_projection_required");
  }
  if (service.service_id !== serviceId || !object(metadata) ||
    (metadata.adapter_type !== "openai_agents" && metadata.adapter_type !== "claude_agent_sdk") ||
    !validSdkAgentSettings(metadata.sdk_agent)) throw new Error("sdk_settings_unavailable");
  const { sdk_provider_key_configured: configured, ...storedMetadata } = metadata;
  return {
    serviceId, framework: metadata.adapter_type,
    settings: { model: metadata.sdk_agent.model, instructions: metadata.sdk_agent.instructions, max_turns: metadata.sdk_agent.max_turns },
    metadata: structuredClone(storedMetadata), keyConfigured: configured === true,
  };
}

export function buildSdkAgentSettingsPatch(saved: SavedSdkAgentSettings, draft: SdkAgentSettings, replacementKey: string): Record<string, unknown> {
  const settings = { ...draft, model: draft.model.trim() };
  if (!validSdkAgentSettings(settings)) throw new Error("invalid_sdk_settings");
  // Empty means unchanged. Non-empty whitespace is an invalid replacement, never a deletion.
  if (replacementKey && (bytes(replacementKey) < 16 || bytes(replacementKey) > 4096 ||
    replacementKey.startsWith("enc:") || new TextEncoder().encode(replacementKey).some(byte => byte <= 32))) throw new Error("invalid_sdk_key");
  return {
    metadata: { ...structuredClone(saved.metadata), adapter_type: saved.framework, sdk_agent: settings },
    ...(replacementKey ? { connector_config: { sdk_provider_api_key: replacementKey } } : {}),
  };
}

export function sdkAgentSettingsMatch(saved: SavedSdkAgentSettings, draft: SdkAgentSettings): boolean {
  const instructions = draft.instructions.trim() ? draft.instructions : "You are a helpful assistant.";
  return saved.settings.model === draft.model.trim() && saved.settings.instructions === instructions &&
    saved.settings.max_turns === draft.max_turns;
}
