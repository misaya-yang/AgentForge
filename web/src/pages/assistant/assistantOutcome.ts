import type { ChatMessage } from "./types";

export type AssistantOutcome =
  | "running"
  | "succeeded"
  | "empty"
  | "failed"
  | "cancelled"
  | "restart_interrupted"
  | "source_revoked"
  | "unknown";

const RUN_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const IMAGE_TASK_ID = /^imt_[0-9a-f]{20}$/i;

export function imageFailureReceipt(
  error: unknown,
  providerDispatched: boolean,
): { uncertain: boolean; diagnosticId?: string } {
  const candidate = error as { response?: { data?: { detail?: unknown } }; error_code?: unknown; task_id?: unknown } | null;
  const detail = candidate?.response?.data?.detail ?? candidate;
  const fields = detail && typeof detail === "object" ? detail as Record<string, unknown> : {};
  const diagnosticId = typeof fields.task_id === "string" && IMAGE_TASK_ID.test(fields.task_id)
    ? fields.task_id : undefined;
  return {
    uncertain: fields.error_code === "outcome_unknown" ||
      (providerDispatched && typeof fields.error_code !== "string"),
    ...(diagnosticId ? { diagnosticId } : {}),
  };
}

export function assistantOutcome(message: ChatMessage): {
  kind: AssistantOutcome;
  diagnosticId?: string;
} {
  const runId = message.processSummary?.runId;
  const diagnosticId = typeof runId === "string" && RUN_ID.test(runId)
    ? runId
    : typeof message.diagnosticId === "string" && IMAGE_TASK_ID.test(message.diagnosticId)
      ? message.diagnosticId : undefined;
  const status = message.processSummary?.status ?? message.status;
  if (message.isGeneratingImage) return { kind: "running" };
  if (message.outcomeUncertain) return { kind: "unknown", diagnosticId };
  if (status === "failed" || message.status === "failed") return { kind: "failed", diagnosticId };
  if (status === "cancelled" || message.status === "cancelled") {
    return {
      kind: message.processSummary?.terminalReason === "runtime_restart_interrupted"
        ? "restart_interrupted" : "cancelled",
      diagnosticId,
    };
  }
  if (message.isStreaming || status === "running" || status === "blocked" || status === "streaming") {
    return { kind: "running" };
  }
  if (message.sourceAccessRevoked) {
    return { kind: status === "succeeded" || status === "completed" ? "succeeded" : "source_revoked" };
  }
  return { kind: message.content.trim() ? "succeeded" : "empty" };
}
