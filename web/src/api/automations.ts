import { api } from "@/lib/api";

export type AutomationTrigger = { kind: "cron"; expression: string; timezone: string } | { kind: "webhook" } | { kind: "space_page_changed"; item_id: string } | { kind: "connector_event"; entry_id: string };
export type AutomationAction = { kind: "run_agent"; prompt: string } | { kind: "run_skill"; skill_id: string; prompt: string } | { kind: "write_space_page"; item_id: string; markdown: string } | { kind: "notify"; message: string };
export interface AutomationDefinition { title: string; model_id: string; trigger: AutomationTrigger; actions: AutomationAction[] }
export interface AutomationRule { automation_id: string; title: string; definition: AutomationDefinition; status: "draft" | "active" | "paused" | "cancelled"; revision: number; next_run_at: string | null }
export interface AutomationExecution { delivery_id: string; event_key: string; action_index: number; status: string; result: Record<string, unknown> | null; created_at: string }
function rule(value: unknown): AutomationRule {
  if (!value || typeof value !== "object" || !("automation_id" in value) || !("definition" in value) || !("revision" in value) || !("status" in value)
    || typeof value.automation_id !== "string" || !Number.isSafeInteger(value.revision) || !["draft", "active", "paused", "cancelled"].includes(String(value.status))) throw new Error("Unconfirmed automation receipt");
  return value as AutomationRule;
}
export async function listAutomations(signal?: AbortSignal): Promise<AutomationRule[]> {
  const data = (await api.get<{ automations: unknown[] }>("/api/v1/automations", { signal })).data;
  if (!Array.isArray(data.automations)) throw new Error("Invalid automation list");
  return data.automations.map(rule);
}
export async function draftAutomation(prompt: string): Promise<AutomationRule> {
  return rule((await api.post<{ automation: unknown }>("/api/v1/automations/draft", { prompt })).data.automation);
}
export async function createAutomation(definition: AutomationDefinition): Promise<AutomationRule> {
  return rule((await api.post<{ automation: unknown }>("/api/v1/automations", definition)).data.automation);
}
export async function confirmAutomation(value: AutomationRule): Promise<{ automation: AutomationRule; webhook_secret?: string | null }> {
  const data = (await api.post<{ automation: unknown; webhook_secret?: string | null }>(`/api/v1/automations/${encodeURIComponent(value.automation_id)}/confirm`, { confirm: true }, { headers: { "If-Match": `"${value.revision}"` } })).data;
  return { automation: rule(data.automation), webhook_secret: data.webhook_secret };
}
export async function changeAutomation(value: AutomationRule, action: "pause" | "resume" | "cancel"): Promise<AutomationRule> {
  return rule((await api.post<{ automation: unknown }>(`/api/v1/automations/${encodeURIComponent(value.automation_id)}/${action}`, {}, { headers: { "If-Match": `"${value.revision}"` } })).data.automation);
}
export async function automationExecutions(id: string, signal?: AbortSignal): Promise<AutomationExecution[]> {
  return (await api.get<{ executions: AutomationExecution[] }>(`/api/v1/automations/${encodeURIComponent(id)}/executions`, { signal })).data.executions;
}

export interface AutomationNotification { notification_id: string; message: string; created_at: string }
export async function automationNotifications(signal?: AbortSignal): Promise<AutomationNotification[]> {
  const data = (await api.get<{ notifications: AutomationNotification[] }>("/api/v1/automations/notifications", { signal })).data;
  if (!Array.isArray(data.notifications)) throw new Error("Invalid automation notifications");
  return data.notifications;
}
