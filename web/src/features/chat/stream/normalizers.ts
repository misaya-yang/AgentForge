import type { StreamChunk } from "@/types/gateway";
import type {
  ChatTurnState,
  NormalizedStreamEvent,
  NormalizedStreamEventType,
} from "./types";

function nowTs(): number {
  return Date.now();
}

export function createMessageId(prefix = "msg"): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Math.random().toString(36).slice(2, 10)}-${Date.now()}`;
}

export function toTurnState(eventType: NormalizedStreamEventType): ChatTurnState {
  switch (eventType) {
    case "message_start":
    case "message_delta":
    case "tool_call_start":
    case "tool_call_delta":
      return "streaming";
    case "message_end":
    case "tool_call_end":
    case "done":
      return "completed";
    case "error":
      return "failed";
    default:
      return "idle";
  }
}

export function isTerminalEvent(eventType: NormalizedStreamEventType): boolean {
  return eventType === "done" || eventType === "error" || eventType === "message_end";
}

export function normalizeLegacyStreamChunk(
  chunk: Partial<Omit<StreamChunk, "event_type">> & { event_type?: string }
): NormalizedStreamEvent[] {
  const rawType = chunk.event_type || "text_delta";
  const timestamp = nowTs();
  const messageId =
    (chunk.metadata?.message_id as string | undefined) ||
    (chunk.request_id ? `assistant-${chunk.request_id}` : undefined);
  const toolCallId = chunk.tool_call?.tool_call_id;
  const content =
    typeof chunk.content?.data === "string"
      ? chunk.content.data
      : chunk.content?.data != null
        ? String(chunk.content.data)
        : undefined;

  const baseMetadata: Record<string, unknown> = {
    ...(chunk.metadata || {}),
    __rawType: rawType,
  };
  if (chunk.tool_call?.name) {
    baseMetadata.tool_name = chunk.tool_call.name;
  }
  if (chunk.tool_call?.arguments != null) {
    baseMetadata.tool_arguments = chunk.tool_call.arguments;
  }

  if (!baseMetadata.status && chunk.tool_call?.status) {
    baseMetadata.status = chunk.tool_call.status;
  }

  const base: Omit<NormalizedStreamEvent, "type"> = {
    timestamp,
    sequence: chunk.chunk_index,
    messageId,
    toolCallId,
    content,
    metadata: baseMetadata,
  };

  if (rawType === "text_delta") {
    return [{ type: "message_delta", ...base }];
  }
  if (rawType === "thinking") {
    return [{ type: "heartbeat", ...base }];
  }
  if (rawType === "tool_call_start") {
    return [{ type: "tool_call_start", ...base }];
  }
  if (rawType === "tool_call_delta") {
    return [{ type: "tool_call_delta", ...base }];
  }
  if (rawType === "tool_call_end" || rawType === "tool_result") {
    return [{ type: "tool_call_end", ...base }];
  }
  if (rawType === "final") {
    return [
      { type: "message_end", ...base },
      { type: "done", ...base },
    ];
  }
  if (rawType === "stream_end") {
    return [{ type: "done", ...base }];
  }
  if (rawType === "error") {
    return [{ type: "error", ...base, error: content || "stream_error" }];
  }
  if (rawType === "message_start") {
    return [{ type: "message_start", ...base }];
  }
  if (rawType === "message_end") {
    return [{ type: "message_end", ...base }];
  }

  return [{ type: "heartbeat", ...base }];
}
