import type { ChatTurnState } from "../../features/chat/stream/types.ts";

export type RestoredProcessStatus =
  | "succeeded"
  | "running"
  | "failed"
  | "cancelled";

export function restoredProcessStatus(runStatus: unknown): RestoredProcessStatus {
  if (runStatus === "succeeded") return "succeeded";
  if (runStatus === "running" || runStatus === "queued" || runStatus === "awaiting_approval") {
    return "running";
  }
  if (runStatus === "cancelled") return "cancelled";
  return "failed";
}

export function restoredMessageStatus(summaryStatus: unknown): ChatTurnState {
  if (summaryStatus === "cancelled") return "cancelled";
  if (summaryStatus === "failed") return "failed";
  if (summaryStatus === "running" || summaryStatus === "blocked") return "streaming";
  return "completed";
}
