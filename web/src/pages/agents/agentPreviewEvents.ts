import type { AgentStreamEvent } from "@/types/agents";

export function agentPreviewEventData(
  event: AgentStreamEvent,
): Record<string, unknown> {
  return typeof event.data === "object" && event.data !== null ? event.data : {};
}

export function agentPreviewEventText(event: AgentStreamEvent): string {
  if (typeof event.data === "string") return event.data;
  const data = agentPreviewEventData(event);
  return String(
    event.content || data.content || event.message || data.message || "",
  );
}

export function agentPreviewToolActivityId(
  event: AgentStreamEvent,
): string | null {
  const data = agentPreviewEventData(event);
  const rawId =
    event.tool_call_id ||
    event.call_id ||
    data.tool_call_id ||
    data.call_id ||
    data.id;
  return typeof rawId === "string" && rawId ? `tool:${rawId}` : null;
}
