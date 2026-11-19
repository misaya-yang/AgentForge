import type { TimelineStepData } from "./TimelineStep";
import type { ChatMessage } from "../types";

export function buildReasoningStep(
  message: ChatMessage,
  streamingTitle: string,
  completedTitle: string,
): TimelineStepData | undefined {
  const body =
    message.streamingThinkingContent?.trim() || message.thinkingContent?.trim() || "";
  if (!body) return undefined;

  const streaming = Boolean(
    message.isThinkingStreaming ||
      (message.streamingThinkingContent && !message.thinkingContent),
  );
  return {
    kind: "thinking",
    id: `thinking-${message.id}`,
    title: streaming ? streamingTitle : completedTitle,
    body,
    streaming,
  };
}

/** Keep execution rows fixed above the one independently-streaming reasoning row. */
export function appendReasoningAfterActivity(
  activitySteps: TimelineStepData[],
  reasoningStep: TimelineStepData | undefined,
): TimelineStepData[] {
  if (reasoningStep) activitySteps.push(reasoningStep);
  return activitySteps;
}
