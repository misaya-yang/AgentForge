import { api } from "@/lib/api";

export interface AssistantMemoryItem {
  key: string;
  value: unknown;
  namespace: string;
  source: string;
  created_at: string | null;
  updated_at: string | null;
  expires_at: string | null;
}

export interface AssistantMemoryState {
  enabled: boolean;
  effect: string;
  items: AssistantMemoryItem[];
  has_more: boolean;
}

export async function getAssistantMemory(): Promise<AssistantMemoryState> {
  const { data } = await api.get<AssistantMemoryState>("/api/v1/assistant/memory");
  return data;
}

export async function setAssistantMemoryEnabled(enabled: boolean): Promise<boolean> {
  const { data } = await api.patch<{ enabled: boolean }>("/api/v1/assistant/memory", { enabled });
  return data.enabled;
}

export async function updateAssistantMemoryItem(key: string, value: string): Promise<void> {
  await api.put("/api/v1/assistant/memory/items", { key, value });
}

export async function deleteAssistantMemoryItem(key: string): Promise<void> {
  await api.delete("/api/v1/assistant/memory/items", { params: { key } });
}
