import type { RequestBody } from "@/api/contracts";
export type AgentTurnRequestOptions = Pick<RequestBody<"POST /api/v2/agent/threads/{thread}/turns">, "provider_id" | "max_tokens" | "temperature" | "kb_dataset_ids" | "kb_mode" | "kb_top_k" | "kb_score_threshold" | "web_search_enabled" | "web_search_max_results" | "file_paths" | "execution_profile" | "memory_mode" | "system_prompt" | "os_agent_enabled" | "local_node_device_id" | "local_node_grant_ids" | "resume_run_id" | "resume_approval_id">;

export function buildAgentTurnPayload(
  message: string,
  modelId?: string,
  reasoningOption?: string,
  request?: AgentTurnRequestOptions,
): RequestBody<"POST /api/v2/agent/threads/{thread}/turns"> {
  return {
    message,
    ...(modelId ? { model_id: modelId } : {}),
    ...(request?.provider_id ? { provider_id: request.provider_id } : {}),
    ...(reasoningOption ? { reasoning_option: reasoningOption } : {}),
    ...(request?.max_tokens != null ? { max_tokens: request.max_tokens } : {}),
    ...(request?.temperature != null ? { temperature: request.temperature } : {}),
    kb_dataset_ids: request?.kb_dataset_ids || [],
    kb_mode: request?.kb_mode || "off",
    kb_top_k: request?.kb_top_k || 5,
    kb_score_threshold: request?.kb_score_threshold ?? 0.4,
    web_search_enabled: request?.web_search_enabled || false,
    web_search_max_results: request?.web_search_max_results || 5,
    file_paths: request?.file_paths || [],
    ...(request?.execution_profile ? { execution_profile: request.execution_profile } : {}),
    ...(request?.memory_mode ? { memory_mode: request.memory_mode } : {}),
    ...(request?.system_prompt !== undefined ? { system_prompt: request.system_prompt } : {}),
    ...(request?.os_agent_enabled !== undefined ? { os_agent_enabled: request.os_agent_enabled } : {}),
    ...(request?.local_node_device_id !== undefined ? { local_node_device_id: request.local_node_device_id } : {}),
    ...(request?.local_node_grant_ids !== undefined ? { local_node_grant_ids: request.local_node_grant_ids } : {}),
    ...(request?.resume_run_id !== undefined ? { resume_run_id: request.resume_run_id } : {}),
    ...(request?.resume_approval_id !== undefined ? { resume_approval_id: request.resume_approval_id } : {}),
  };
}

export function buildAgentThreadPayload(sessionId?: string, modelId?: string, providerId?: string): RequestBody<"POST /api/v2/agent/threads"> {
  return {
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(modelId ? { model_id: modelId } : {}),
    ...(providerId ? { provider_id: providerId } : {}),
  };
}
