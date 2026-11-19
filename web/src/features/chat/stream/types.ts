export type NormalizedStreamEventType =
  | "message_start"
  | "message_delta"
  | "message_end"
  | "tool_call_start"
  | "tool_call_delta"
  | "tool_call_end"
  | "error"
  | "done"
  | "heartbeat";

export type ChatTurnState = "idle" | "streaming" | "completed" | "cancelled" | "failed";
export type ChatMessageRole = "user" | "assistant" | "system" | "tool";

export interface NormalizedStreamEvent {
  type: NormalizedStreamEventType;
  timestamp: number;
  sequence?: number;
  messageId?: string;
  toolCallId?: string;
  content?: string;
  error?: string;
  metadata?: Record<string, unknown>;
}

export type MessagePartType = "text" | "tool_call" | "tool_result";

export interface MessagePart {
  id: string;
  type: MessagePartType;
  content: string;
  createdAt: string;
  metadata?: Record<string, unknown>;
}

export interface ChatMessageMeta {
  usage?: {
    inputTokens?: number;
    outputTokens?: number;
    totalTokens?: number;
  };
  firstTokenMs?: number;
  durationMs?: number;
  [key: string]: unknown;
}

export interface UnifiedChatMessage {
  id: string;
  role: ChatMessageRole;
  createdAt: string;
  parts: MessagePart[];
  status: ChatTurnState;
  meta?: ChatMessageMeta;
}

