import type { TimelineStepData } from "./TimelineStep";
import type { ChatMessage } from "../types";

export function hasTerminalActivity(message: Pick<ChatMessage, "status" | "processSummary">): boolean {
  return [message.status, message.processSummary?.status].some(
    (status) => ["succeeded", "completed", "failed", "cancelled"].includes(status ?? ""),
  );
}

/** A stopped Agent cannot make an unfinished tool successful. */
export function terminalActivitySteps(
  steps: TimelineStepData[],
  terminal: boolean,
  pendingApprovalIds: Set<string>,
  unknownBody: string,
  notExecutedBody: string,
): TimelineStepData[] {
  if (!terminal) return steps;
  return steps.map((step) => {
    if (step.kind !== "tool" || step.status !== "running") return step;
    const originalId = step.id.startsWith("process-tool-") ? step.id.slice("process-tool-".length) : step.id;
    const awaitingApproval = pendingApprovalIds.has(originalId);
    return {
      ...step,
      status: awaitingApproval ? "not_executed" : "unknown",
      body: awaitingApproval ? notExecutedBody : unknownBody,
    };
  });
}
