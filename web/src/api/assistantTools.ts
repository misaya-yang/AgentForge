import { api } from "@/lib/api";

export interface AssistantToolEntry {
  name: string;
  description?: string;
  tenant_visible?: boolean;
  effect?: string;
  approval?: string;
  required_inputs: string[];
  device_required: boolean;
}

export interface AssistantSessionTools {
  platform_catalog: AssistantToolEntry[];
  next_turn_estimate: { status: "available_now" | "unavailable"; tools: AssistantToolEntry[] };
  last_run_pinned: { run_id: string; status: string; tools: AssistantToolEntry[] } | null;
  note: string;
}

export async function getAssistantSessionTools(sessionId: string, signal?: AbortSignal): Promise<AssistantSessionTools> {
  const { data } = await api.get<AssistantSessionTools>(`/api/v1/assistant/sessions/${sessionId}/tools`, { signal });
  return data;
}
