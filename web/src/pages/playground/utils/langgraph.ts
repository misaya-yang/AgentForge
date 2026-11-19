/**
 * Pure LangGraph parsing utilities for the Playground page.
 *
 * Every function here is side-effect free and React-independent so that
 * the module can be unit-tested in isolation.
 */

import type { ToolCallWithResult } from "@/components/ChatWindow";
import type { SessionMessage, SessionMessageToolCall } from "@/api/sessions";
import type { StreamTurnState } from "@/features/chat/stream";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type LangGraphStreamEvent = { event: string; data: unknown };

export type ToolCallUpdate = {
  id: string;
  name: string;
  args: string;
  index?: number;
};

export type ToolResultUpdate = {
  id: string;
  name: string;
  content: string;
  status: "completed" | "error";
};

// ---------------------------------------------------------------------------
// Env / Misc helpers
// ---------------------------------------------------------------------------

export function readPositiveMsEnv(name: string, fallback: number): number {
  const raw = import.meta.env[name];
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

export function estimateTokens(text: string): number {
  if (!text) return 0;
  const cjkCount = text.match(/[\u4E00-\u9FFF]/g)?.length ?? 0;
  const nonCjkCount = Math.max(text.length - cjkCount, 0);
  return Math.max(1, Math.ceil(cjkCount / 2) + Math.ceil(nonCjkCount / 4));
}

export function buildTextParts(messageId: string, content: string, createdAt: string) {
  if (!content) return [];
  return [{ id: `${messageId}-part-0`, type: "text" as const, content, createdAt }];
}

// ---------------------------------------------------------------------------
// Abort / Timeout helpers
// ---------------------------------------------------------------------------

export function combineAbortSignals(signals: AbortSignal[]): AbortSignal {
  if (typeof AbortSignal.any === "function") {
    return AbortSignal.any(signals);
  }
  const controller = new AbortController();
  const abort = () => controller.abort();
  for (const signal of signals) {
    if (signal.aborted) {
      controller.abort();
      return controller.signal;
    }
    signal.addEventListener("abort", abort, { once: true });
  }
  return controller.signal;
}

export function createTimeoutSignal(timeoutMs: number): {
  signal: AbortSignal;
  cancel: () => void;
} {
  const controller = new AbortController();
  const timerId = window.setTimeout(() => controller.abort(), timeoutMs);
  return {
    signal: controller.signal,
    cancel: () => window.clearTimeout(timerId),
  };
}

// ---------------------------------------------------------------------------
// JSON / Parsing helpers
// ---------------------------------------------------------------------------

export function tryParseJsonLike(value: unknown): unknown {
  if (typeof value !== "string") return value;
  const trimmed = value.trim();
  if (!trimmed || (!trimmed.startsWith("{") && !trimmed.startsWith("["))) {
    return value;
  }
  try {
    return JSON.parse(trimmed);
  } catch {
    return value;
  }
}

export function isValidToolArgs(value: string): boolean {
  if (!value) return false;
  try {
    JSON.parse(value);
    return true;
  } catch {
    return false;
  }
}

function normalizeToolArgs(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

// ---------------------------------------------------------------------------
// LangGraph event normalization
// ---------------------------------------------------------------------------

export function normalizeLangGraphEvent(raw: LangGraphStreamEvent): LangGraphStreamEvent {
  const normalizedData = tryParseJsonLike(raw?.data);
  const data = normalizedData as Record<string, unknown> | null;
  if (!raw?.event && data && typeof data === "object" && typeof data.event === "string" && "data" in data) {
    return {
      event: data.event as string,
      data: (data as Record<string, unknown>).data,
    };
  }
  return { event: raw?.event || "", data: normalizedData };
}

export function extractMessagePayload(data: unknown): { message: Record<string, unknown>; metadata?: unknown } | null {
  const normalized = tryParseJsonLike(data);
  if (Array.isArray(normalized) && normalized.length > 0 && normalized[0] && typeof normalized[0] === "object") {
    return { message: normalized[0] as Record<string, unknown>, metadata: normalized[1] };
  }
  if (normalized && typeof normalized === "object") {
    return { message: normalized as Record<string, unknown> };
  }
  return null;
}

export function normalizeContentDelta(message: Record<string, unknown>): string | null {
  const content = message.content as unknown;
  const directText = message.text as unknown;

  // Direct text field (some LangGraph versions use this)
  if (typeof directText === "string" && directText) return directText;

  // String content (most common case)
  if (typeof content === "string") return content;

  // Array content (LangGraph multimodal format)
  if (Array.isArray(content)) {
    const parts: string[] = [];
    for (const item of content) {
      if (typeof item === "string") {
        parts.push(item);
      } else if (item && typeof item === "object") {
        const record = item as Record<string, unknown>;
        // Handle various content block formats
        const text = record.text ?? record.data ?? record.content;
        if (typeof text === "string") parts.push(text);
        // Handle type-prefixed content blocks (e.g., {"type": "text", "text": "..."})
        if (record.type === "text" && typeof record.text === "string") {
          if (!parts.includes(record.text as string)) parts.push(record.text as string);
        }
      }
    }
    const joined = parts.join("");
    return joined || null;
  }

  // Object content (nested structure)
  if (content && typeof content === "object" && !Array.isArray(content)) {
    const record = content as Record<string, unknown>;
    if (typeof record.text === "string") return record.text;
    if (typeof record.data === "string") return record.data;
  }

  return null;
}

// ---------------------------------------------------------------------------
// Tool call extraction
// ---------------------------------------------------------------------------

export function extractToolCallUpdates(message: Record<string, unknown>): ToolCallUpdate[] {
  const updates: ToolCallUpdate[] = [];
  const toolCallChunks = message.tool_call_chunks as unknown;
  const toolCalls = message.tool_calls as unknown;

  const normalizeIndex = (value: unknown): number | undefined => {
    const index =
      typeof value === "number"
        ? value
        : typeof value === "string" && value.trim()
          ? Number(value)
          : undefined;
    return index != null && Number.isFinite(index) ? index : undefined;
  };

  if (Array.isArray(toolCallChunks) && toolCallChunks.length > 0) {
    for (const chunk of toolCallChunks) {
      if (!chunk || typeof chunk !== "object") continue;
      const record = chunk as Record<string, unknown>;
      updates.push({
        id: (record.id as string) || (record.tool_call_id as string) || "",
        name: (record.name as string) || "",
        args: normalizeToolArgs(record.args ?? record.arguments),
        index: normalizeIndex(record.index),
      });
    }
    return updates.filter((u) => u.id || u.name || u.args);
  }

  if (Array.isArray(toolCalls) && toolCalls.length > 0) {
    for (const call of toolCalls) {
      if (!call || typeof call !== "object") continue;
      const record = call as Record<string, unknown>;
      updates.push({
        id: (record.id as string) || (record.tool_call_id as string) || "",
        name: (record.name as string) || "",
        args: normalizeToolArgs(record.args ?? record.arguments),
        index: normalizeIndex(record.index),
      });
    }
  }

  if (updates.length === 0) {
    const additional = message.additional_kwargs as Record<string, unknown> | undefined;
    const functionCall = additional?.function_call;
    if (functionCall && typeof functionCall === "object") {
      const record = functionCall as Record<string, unknown>;
      updates.push({
        id: (message.id as string) || "",
        name: (record.name as string) || "",
        args: normalizeToolArgs(record.arguments),
      });
    }
  }

  return updates.filter((u) => u.id || u.name || u.args);
}

export function extractToolResult(message: Record<string, unknown>): ToolResultUpdate | null {
  const msgType = (message.type as string) || "";
  const role = (message.role as string) || "";
  const isToolMessage = msgType === "tool" || msgType === "ToolMessage" || role === "tool";
  if (!isToolMessage) return null;

  const toolCallId =
    (message.tool_call_id as string) ||
    ((message.tool_call_ids as string[] | undefined)?.[0] as string | undefined) ||
    "";
  const name = (message.name as string) || "";
  let content = message.content as unknown;
  if ((content == null || content === "") && message.artifact != null) {
    content = message.artifact;
  }
  if (typeof content !== "string") {
    content = normalizeToolArgs(content);
  }
  const rawStatus = String(message.status || "").toLowerCase();
  const status = rawStatus === "error" || rawStatus === "failed" ? "error" : "completed";
  return { id: toolCallId, name, content: String(content), status };
}

// ---------------------------------------------------------------------------
// History normalization
// ---------------------------------------------------------------------------

export function normalizeHistoryContent(value: unknown): string {
  if (typeof value === "string") return value;
  if (value == null) return "";
  if (Array.isArray(value)) {
    const parts = value
      .map((item) => {
        if (typeof item === "string") return item;
        if (!item || typeof item !== "object") return "";
        const record = item as Record<string, unknown>;
        const text = record.text ?? record.data ?? record.content;
        return typeof text === "string" ? text : "";
      })
      .filter((part) => part.trim().length > 0);
    return parts.join("").trim();
  }
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    const text = record.text ?? record.data ?? record.content;
    if (typeof text === "string") return text;
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function extractRunWaitContent(result: unknown): string {
  if (result == null) return "";
  if (typeof result === "string") return result;

  const record = result as Record<string, unknown>;

  const outputs = record.outputs as unknown;
  if (Array.isArray(outputs) && outputs.length > 0) {
    const first = outputs[0] as Record<string, unknown>;
    const data = first?.data;
    return normalizeHistoryContent(data);
  }

  const messages = record.messages as unknown;
  if (Array.isArray(messages) && messages.length > 0) {
    const reversed = [...messages].reverse();
    const assistantMsg = reversed.find((m) => {
      if (!m || typeof m !== "object") return false;
      const recordMsg = m as Record<string, unknown>;
      const role = (recordMsg.role as string) || "";
      const type = (recordMsg.type as string) || "";
      return role === "assistant" || type.toLowerCase().includes("ai");
    });
    const fallbackMsg = reversed.find((m) => m && typeof m === "object");
    const target = (assistantMsg || fallbackMsg) as Record<string, unknown> | undefined;
    if (target) {
      return normalizeHistoryContent(target.content);
    }
  }

  if ("output" in record) {
    return normalizeHistoryContent(record.output);
  }

  return normalizeHistoryContent(record);
}

export function extractHistoryMessages(historyResult: unknown): Record<string, unknown>[] {
  const snapshots = extractHistoryMessageSnapshots(historyResult);
  const messages: Record<string, unknown>[] = [];
  for (const snapshot of snapshots) {
    messages.push(...snapshot);
  }
  return messages;
}

/**
 * Extract per-snapshot message arrays from a LangGraph thread history response.
 * Each snapshot corresponds to one history item's `values.messages`.
 * Returned newest-first (same order as the history API).
 */
export function extractHistoryMessageSnapshots(
  historyResult: unknown
): Record<string, unknown>[][] {
  if (!Array.isArray(historyResult)) return [];
  const snapshots: Record<string, unknown>[][] = [];
  for (const rawItem of historyResult) {
    if (!rawItem || typeof rawItem !== "object") continue;
    const values = (rawItem as Record<string, unknown>).values;
    if (!values || typeof values !== "object") continue;
    const threadMessages = (values as Record<string, unknown>).messages;
    if (!Array.isArray(threadMessages)) continue;
    const snapshot: Record<string, unknown>[] = [];
    for (const rawMessage of threadMessages) {
      if (rawMessage && typeof rawMessage === "object") {
        snapshot.push(rawMessage as Record<string, unknown>);
      }
    }
    if (snapshot.length > 0) snapshots.push(snapshot);
  }
  return snapshots;
}

/**
 * Within a single snapshot's messages, find the slice after the last occurrence
 * of the given user prompt. Returns `undefined` if the prompt is not found.
 */
export function selectSnapshotMessagesAfterPrompt(
  messages: Record<string, unknown>[],
  prompt: string
): Record<string, unknown>[] | undefined {
  const trimmedPrompt = prompt.trim();
  if (!trimmedPrompt) return undefined;

  // Scan backwards to find the last matching user message
  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i];
    const role = ((msg.role as string) || (msg.type as string) || "").toLowerCase();
    const isUser = role === "user" || role === "human" || role.includes("human");
    if (!isUser) continue;
    const content = normalizeHistoryContent(msg.content).trim();
    if (content === trimmedPrompt) {
      return messages.slice(i + 1);
    }
  }
  return undefined;
}

export function extractHistoryAssistantContent(
  historyResult: unknown,
  options?: {
    afterUserPrompt?: string;
    rejectContent?: string;
  }
): string {
  const snapshots = extractHistoryMessageSnapshots(historyResult);
  if (snapshots.length === 0) return "";
  const prompt = options?.afterUserPrompt?.trim();
  const rejectContent = options?.rejectContent?.trim();

  // Search each snapshot independently, newest first
  for (const snapshot of snapshots) {
    const candidates = prompt
      ? selectSnapshotMessagesAfterPrompt(snapshot, prompt)
      : snapshot;

    if (!candidates || candidates.length === 0) continue;

    // Find the last assistant message in this snapshot's candidates
    const assistantMsg = [...candidates].reverse().find((message) => {
      const role = ((message.role as string) || "").toLowerCase();
      const type = ((message.type as string) || "").toLowerCase();
      if (!(role === "assistant" || type.includes("ai"))) return false;
      const content = normalizeHistoryContent(message.content).trim();
      if (!content) return false;
      if (rejectContent && content === rejectContent) return false;
      return true;
    });

    if (assistantMsg) {
      return normalizeHistoryContent(assistantMsg.content);
    }
  }

  return "";
}

export function extractHistoryToolCalls(
  historyResult: unknown,
  options?: {
    afterUserPrompt?: string;
  }
): ToolCallWithResult[] | undefined {
  const snapshots = extractHistoryMessageSnapshots(historyResult);
  if (snapshots.length === 0) return undefined;
  const prompt = options?.afterUserPrompt?.trim();

  // Search each snapshot independently, newest first
  for (const snapshot of snapshots) {
    const candidates = prompt
      ? selectSnapshotMessagesAfterPrompt(snapshot, prompt)
      : snapshot;

    if (!candidates || candidates.length === 0) continue;

    const toolCalls = extractRunWaitToolCalls({ messages: candidates });
    if (toolCalls && toolCalls.length > 0) return toolCalls;
  }

  return undefined;
}

export function extractRunWaitToolCalls(result: unknown): ToolCallWithResult[] | undefined {
  if (!result || typeof result !== "object") return undefined;

  const messages = (result as Record<string, unknown>).messages;
  if (!Array.isArray(messages) || messages.length === 0) return undefined;

  const collected = new Map<string, ToolCallWithResult>();
  let autoId = 0;

  for (const rawMessage of messages) {
    if (!rawMessage || typeof rawMessage !== "object") continue;
    const message = rawMessage as Record<string, unknown>;

    for (const update of extractToolCallUpdates(message)) {
      const toolCallId = update.id || `wait-tool-${++autoId}`;
      const existing = collected.get(toolCallId);
      const argsText = update.args || existing?.argsText || "";
      collected.set(toolCallId, {
        toolCall: {
          tool_call_id: toolCallId,
          name: update.name || existing?.toolCall.name || "tool",
          arguments: argsText,
          status: existing?.toolCall.status || "running",
        },
        result: existing?.result,
        argsText,
        argsValid: argsText ? isValidToolArgs(argsText) : Boolean(existing?.argsValid),
      });
    }

    const toolResult = extractToolResult(message);
    if (!toolResult) continue;

    const toolCallId = toolResult.id || `wait-tool-${++autoId}`;
    const existing = collected.get(toolCallId);
    const argsText = existing?.argsText || "";
    collected.set(toolCallId, {
      toolCall: {
        tool_call_id: toolCallId,
        name: toolResult.name || existing?.toolCall.name || "tool",
        arguments: argsText,
        status: toolResult.status,
      },
      result: toolResult.content,
      argsText,
      argsValid: argsText ? isValidToolArgs(argsText) : Boolean(existing?.argsValid),
    });
  }

  return collected.size > 0 ? Array.from(collected.values()) : undefined;
}

export function mergeFallbackToolCalls(
  state: StreamTurnState,
  toolCalls?: ToolCallWithResult[]
): StreamTurnState {
  if (!toolCalls?.length) return state;

  const nextToolCalls = [...state.toolCalls];
  for (const item of toolCalls) {
    const toolCallId = item.toolCall.tool_call_id || `wait-tool-${nextToolCalls.length + 1}`;
    const index = nextToolCalls.findIndex((toolCall) => toolCall.id === toolCallId);
    const nextValue = {
      id: toolCallId,
      name: item.toolCall.name || "tool",
      arguments: item.argsText || item.toolCall.arguments || "",
      argsValid: Boolean(item.argsValid),
      status: item.toolCall.status || (item.result !== undefined ? "completed" : "running"),
      result: item.result,
      startedAt: index >= 0 ? nextToolCalls[index].startedAt : undefined,
      endedAt: item.result !== undefined ? performance.now() : nextToolCalls[index]?.endedAt,
    } as StreamTurnState["toolCalls"][number];

    if (index === -1) {
      nextToolCalls.push(nextValue);
      continue;
    }
    nextToolCalls[index] = {
      ...nextToolCalls[index],
      ...nextValue,
    };
  }

  return {
    ...state,
    toolCalls: nextToolCalls,
  };
}

// ---------------------------------------------------------------------------
// Session history helpers
// ---------------------------------------------------------------------------

export function dedupeHistory(history: SessionMessage[]): SessionMessage[] {
  const next: SessionMessage[] = [];
  for (const msg of history) {
    const prev = next[next.length - 1];
    if (!prev) {
      next.push(msg);
      continue;
    }
    const prevContent = normalizeHistoryContent(prev.content);
    const currContent = normalizeHistoryContent(msg.content);
    if (prev.role === msg.role && prevContent === currContent) {
      const prevTime = Date.parse(prev.timestamp ?? "");
      const currTime = Date.parse(msg.timestamp ?? "");
      if (Number.isFinite(prevTime) && Number.isFinite(currTime)) {
        if (Math.abs(currTime - prevTime) <= 3000) {
          continue;
        }
      } else {
        continue;
      }
    }
    next.push(msg);
  }
  return next;
}

export function convertToolCallsFromMetadata(toolCalls?: SessionMessageToolCall[]): ToolCallWithResult[] | undefined {
  if (!toolCalls || toolCalls.length === 0) return undefined;
  return toolCalls.map((tc) => ({
    toolCall: {
      tool_call_id: tc.tool_call_id,
      name: tc.name,
      arguments: tc.arguments,
      status: tc.status || "completed",
    },
    result: tc.result ?? undefined,
    argsText: tc.arguments,
    argsValid: true,
  }));
}

export { readReportedRouting } from "../../../features/chat/stream/routingObservation.ts";
