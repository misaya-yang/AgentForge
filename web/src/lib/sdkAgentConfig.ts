import { validateSdkCapabilitySelection, type SdkCapabilityOption } from "@/lib/sdkCapabilitySelection";
import { validateSdkKnowledgeSelection, type SdkKnowledgeOption } from "@/lib/sdkKnowledgeSelection";

/** Public registration only. Worker routing and credentials stay Gateway-side. */
export type AgentSdk = "openai_agents" | "claude_agent_sdk";

export function isNativeAgentSdk(adapter: unknown): adapter is AgentSdk {
  return adapter === "openai_agents" || adapter === "claude_agent_sdk";
}

export interface SdkAgentFormValues {
  sdk: AgentSdk;
  serviceId: string;
  name: string;
  model: string;
  instructions: string;
  maxTurns: number;
  apiKey: string;
  knowledgeDatasetIds?: string[];
  capabilityIds?: string[];
}

export function buildSdkAgentDefinition(values: SdkAgentFormValues, visibleDatasets: readonly SdkKnowledgeOption[] = [], visibleCapabilities: readonly SdkCapabilityOption[] = []): Record<string, unknown> {
  const capabilityIds = validateSdkCapabilitySelection(values.capabilityIds ?? [], visibleCapabilities);
  const knowledgeIds = validateSdkKnowledgeSelection(values.knowledgeDatasetIds ?? [], visibleDatasets);
  if (!isNativeAgentSdk(values.sdk) ||
      !/^[a-z0-9][a-z0-9_-]{0,127}$/.test(values.serviceId.trim()) ||
      !values.name.trim() || !values.model.trim() || !values.apiKey.trim() ||
      !Number.isInteger(values.maxTurns) || values.maxTurns < 1 || values.maxTurns > 32) {
    throw new Error("invalid_sdk_agent_configuration");
  }
  return {
    service_id: values.serviceId.trim(),
    name: values.name.trim(),
    service_type: "custom",
    supported_modes: capabilityIds.length ? ["stream"] : ["sync", "stream"],
    status: "active",
    connector_type: "http",
    connector_config: { sdk_provider_api_key: values.apiKey.trim() },
    accepted_content_types: ["text"],
    output_content_types: ["text"],
    session_enabled: true,
    max_retries: 0,
    metadata: {
      adapter_type: values.sdk,
      ...(knowledgeIds.length || capabilityIds.length ? { sdk_tools: {
        ...(knowledgeIds.length ? { knowledge_dataset_ids: knowledgeIds } : {}),
        ...(capabilityIds.length ? { capability_ids: capabilityIds } : {}),
      } } : {}),
      sdk_agent: {
        model: values.model.trim(),
        instructions: values.instructions.trim(),
        max_turns: values.maxTurns,
      },
    },
  };
}
