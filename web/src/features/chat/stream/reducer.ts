import { isSdkLifecycleChunk, reduceSdkToolActivity, type SdkToolActivity } from "./sdkLifecycle.ts";
import type { StreamChunk } from "@/types/gateway";
import { normalizeLegacyStreamChunk } from "./normalizers.ts";
import type { ChatTurnState, NormalizedStreamEvent } from "./types.ts";

export type StreamToolCallStatus = "pending" | "running" | "completed" | "error";

export interface StreamToolCallState {
  id: string;
  name: string;
  arguments: string;
  argsValid: boolean;
  status: StreamToolCallStatus;
  result?: string;
  startedAt?: number;
  endedAt?: number;
}

export interface StreamUsageStats {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
}

export interface StreamTurnState {
  status: ChatTurnState;
  content: string;
  error?: string;
  toolCalls: StreamToolCallState[];
  sdkToolActivity: SdkToolActivity[];
  usage: StreamUsageStats;
  firstTokenMs?: number;
  durationMs?: number;
  startedAtMs: number;
  lastMessageSequence?: number;
  toolSequenceById: Record<string, number>;
}

export interface StreamReducerContext {
  autoToolCallCounter: number;
}

export interface StreamReduceResult {
  state: StreamTurnState;
  changed: boolean;
  terminal: boolean;
}

function isTerminalStatus(status: ChatTurnState): boolean {
  return status === "completed" || status === "failed" || status === "cancelled";
}

function toNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }
  return undefined;
}

function normalizeUsage(usage?: Record<string, unknown>): StreamUsageStats {
  if (!usage) return {};
  const input = toNumber(usage.input_tokens ?? usage.prompt_tokens);
  const output = toNumber(usage.output_tokens ?? usage.completion_tokens);
  const total = toNumber(usage.total_tokens);
  return {
    inputTokens: input,
    outputTokens: output,
    totalTokens: total ?? ((input ?? 0) + (output ?? 0) || undefined),
  };
}

function extractTiming(usage?: Record<string, unknown>): {
  durationMs?: number;
  firstTokenMs?: number;
} {
  if (!usage) return {};
  return {
    durationMs: toNumber(usage.duration_ms),
    firstTokenMs: toNumber(usage.first_token_ms),
  };
}

function tryParseJson(value: string): unknown | null {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

function mergeToolArguments(current: string, incoming: string): string {
  if (!current) return incoming;
  if (!incoming) return current;
  if (incoming.startsWith(current)) return incoming;
  if (current.startsWith(incoming)) return current;

  const currentTrimmed = current.trim();
  const incomingTrimmed = incoming.trim();
  const currentLooksLikeJson = currentTrimmed.startsWith("{") && currentTrimmed.endsWith("}");
  const incomingLooksLikeJson = incomingTrimmed.startsWith("{") && incomingTrimmed.endsWith("}");

  if (currentLooksLikeJson && incomingLooksLikeJson) {
    return incoming.length >= current.length ? incoming : current;
  }
  if (incomingTrimmed.startsWith("{")) {
    return incoming;
  }
  if (currentLooksLikeJson && !incomingLooksLikeJson) {
    return current + incoming;
  }

  const maxOverlap = Math.min(current.length, incoming.length, 10);
  for (let size = maxOverlap; size > 0; size -= 1) {
    const suffix = current.slice(-size);
    const prefix = incoming.slice(0, size);
    if (suffix === prefix) {
      if (size <= 2 && /^[{}[\]:,"]+$/.test(suffix)) {
        continue;
      }
      return current + incoming.slice(size);
    }
  }

  return current + incoming;
}

function resolveToolArguments(
  current: string,
  incoming: string,
  currentValid: boolean
): { text: string; isValid: boolean } {
  if (!incoming) return { text: current, isValid: currentValid };

  const incomingParsed = tryParseJson(incoming);
  if (incomingParsed !== null) {
    const isEmptyPlaceholder =
      typeof incomingParsed === "object" &&
      incomingParsed !== null &&
      Object.values(incomingParsed).every(
        (value) =>
          value === "" ||
          value === null ||
          (Array.isArray(value) && value.length === 0)
      );
    if (isEmptyPlaceholder && currentValid) {
      const currentParsed = tryParseJson(current);
      if (currentParsed && typeof currentParsed === "object") {
        const currentHasContent = Object.values(currentParsed).some(
          (value) =>
            value !== "" &&
            value !== null &&
            !(Array.isArray(value) && value.length === 0)
        );
        if (currentHasContent) {
          return { text: current, isValid: true };
        }
      }
    }
    return { text: incoming, isValid: true };
  }

  if (!current) {
    return { text: incoming, isValid: false };
  }
  if (currentValid) {
    return { text: current, isValid: true };
  }

  const merged = mergeToolArguments(current, incoming);
  return { text: merged, isValid: tryParseJson(merged) !== null };
}

function normalizeString(value: unknown): string {
  if (typeof value === "string") return value;
  if (value == null) return "";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function markStreamFirstToken(
  state: StreamTurnState,
  nowMs: number,
): StreamTurnState {
  if (state.firstTokenMs != null) return state;
  return {
    ...state,
    firstTokenMs: Math.max(0, Math.round(nowMs - state.startedAtMs)),
  };
}

function withDuration(state: StreamTurnState, nowMs: number): StreamTurnState {
  if (state.durationMs != null) return state;
  return {
    ...state,
    durationMs: Math.max(0, Math.round(nowMs - state.startedAtMs)),
  };
}

function shouldSkipBySequence(
  state: StreamTurnState,
  event: NormalizedStreamEvent,
  toolCallId?: string
): boolean {
  if (event.sequence == null) return false;
  if (!toolCallId) {
    return (
      state.lastMessageSequence != null && event.sequence <= state.lastMessageSequence
    );
  }
  const last = state.toolSequenceById[toolCallId];
  return last != null && event.sequence <= last;
}

function recordSequence(
  state: StreamTurnState,
  event: NormalizedStreamEvent,
  toolCallId?: string
): StreamTurnState {
  if (event.sequence == null) return state;
  if (!toolCallId) {
    return { ...state, lastMessageSequence: event.sequence };
  }
  return {
    ...state,
    toolSequenceById: {
      ...state.toolSequenceById,
      [toolCallId]: event.sequence,
    },
  };
}

function extractToolName(event: NormalizedStreamEvent): string {
  // Return empty string when the event doesn't carry a name — callers OR
  // with the existing tool.name, so we don't clobber a good name from an
  // earlier event (e.g. tool_call_start sets name="search_web", then a
  // later tool_call_end arrives without tool_name metadata → we must not
  // overwrite with a fallback placeholder).
  const metadata = event.metadata || {};
  return typeof metadata.tool_name === "string" ? metadata.tool_name : "";
}

function extractToolArguments(event: NormalizedStreamEvent): string {
  const metadata = event.metadata || {};
  if ("tool_arguments" in metadata) {
    return normalizeString(metadata.tool_arguments);
  }
  return "";
}

function extractRawType(event: NormalizedStreamEvent): string | undefined {
  const metadata = event.metadata || {};
  return typeof metadata.__rawType === "string" ? metadata.__rawType : undefined;
}

function getToolCallId(
  event: NormalizedStreamEvent,
  context: StreamReducerContext
): string {
  if (event.toolCallId) return event.toolCallId;
  context.autoToolCallCounter += 1;
  return `auto-${context.autoToolCallCounter}`;
}

function upsertToolCall(
  state: StreamTurnState,
  toolCallId: string,
  updater: (tool: StreamToolCallState) => StreamToolCallState
): StreamTurnState {
  const index = state.toolCalls.findIndex((tool) => tool.id === toolCallId);
  if (index === -1) {
    const seed: StreamToolCallState = {
      id: toolCallId,
      // Empty seed, filled by the first event's extractToolName result.
      // Keeps the `extractToolName(event) || tool.name` guard meaningful.
      name: "",
      arguments: "",
      argsValid: false,
      status: "pending",
    };
    return {
      ...state,
      toolCalls: [...state.toolCalls, updater(seed)],
    };
  }
  const nextToolCalls = [...state.toolCalls];
  nextToolCalls[index] = updater(nextToolCalls[index]);
  return {
    ...state,
    toolCalls: nextToolCalls,
  };
}

export function createStreamReducerContext(): StreamReducerContext {
  return {
    autoToolCallCounter: 0,
  };
}

export function createStreamTurnState(startedAtMs: number): StreamTurnState {
  return {
    status: "idle",
    content: "",
    toolCalls: [],
    sdkToolActivity: [],
    usage: {},
    startedAtMs,
    toolSequenceById: {},
  };
}

export function applyUsageToTurnState(
  state: StreamTurnState,
  usageLike: Record<string, unknown> | undefined
): StreamTurnState {
  if (!usageLike) return state;
  const usage = normalizeUsage(usageLike);
  const timing = extractTiming(usageLike);
  let next = {
    ...state,
    usage: {
      ...state.usage,
      ...usage,
    },
  };
  if (timing.durationMs != null) {
    next = { ...next, durationMs: timing.durationMs };
  }
  // Only apply server-reported firstTokenMs if we haven't already measured it
  // client-side. The client marker fires on the first thought, tool call, or text
  // event (~2s), but the server may report a later value that only counts the
  // first *text* token after tool execution (~5-8s), which misleads the user.
  if (timing.firstTokenMs != null && next.firstTokenMs == null) {
    next = { ...next, firstTokenMs: timing.firstTokenMs };
  }
  return next;
}

export function completeStreamTurn(
  state: StreamTurnState,
  nowMs: number
): StreamTurnState {
  if (state.status === "failed" || state.status === "cancelled") {
    return withDuration(state, nowMs);
  }
  return withDuration({ ...state, status: "completed" }, nowMs);
}

export function cancelStreamTurn(
  state: StreamTurnState,
  nowMs: number
): StreamTurnState {
  if (
    state.status === "completed" ||
    state.status === "failed" ||
    state.status === "cancelled"
  ) {
    return withDuration(state, nowMs);
  }
  return withDuration({ ...state, status: "cancelled" }, nowMs);
}

export function failStreamTurn(
  state: StreamTurnState,
  message: string,
  nowMs: number
): StreamTurnState {
  if (
    state.status === "completed" ||
    state.status === "failed" ||
    state.status === "cancelled"
  ) {
    return withDuration(state, nowMs);
  }
  return withDuration(
    {
      ...state,
      status: "failed",
      error: message,
    },
    nowMs
  );
}

export function setStreamTurnContent(
  state: StreamTurnState,
  content: string
): StreamTurnState {
  return {
    ...state,
    content,
  };
}

export function reduceNormalizedStreamEvent(
  state: StreamTurnState,
  event: NormalizedStreamEvent,
  context: StreamReducerContext,
  nowMs: number
): StreamReduceResult {
  let next = state;
  let terminal = false;
  let changed = false;

  switch (event.type) {
    case "message_start":
      if (!isTerminalStatus(state.status) && state.status !== "streaming") {
        next = { ...state, status: "streaming" };
        changed = true;
      }
      break;
    case "message_delta": {
      if (isTerminalStatus(state.status) || shouldSkipBySequence(state, event)) {
        break;
      }
      const delta = event.content || "";
      next = recordSequence(state, event);
      next = markStreamFirstToken(
        {
          ...next,
          status: "streaming",
          content: `${next.content}${delta}`,
        },
        nowMs
      );
      changed = true;
      break;
    }
    case "message_end":
      next = recordSequence(state, event);
      next = completeStreamTurn(next, nowMs);
      changed = true;
      terminal = true;
      break;
    case "tool_call_start":
    case "tool_call_delta": {
      const toolCallId = getToolCallId(event, context);
      if (isTerminalStatus(state.status) || shouldSkipBySequence(state, event, toolCallId)) {
        break;
      }
      const incomingArgs = extractToolArguments(event);
      next = recordSequence(state, event, toolCallId);
      next = markStreamFirstToken(
        upsertToolCall(next, toolCallId, (tool) => {
          const resolvedArgs = resolveToolArguments(
            tool.arguments,
            incomingArgs,
            tool.argsValid
          );
          return {
            ...tool,
            id: toolCallId,
            name: extractToolName(event) || tool.name,
            arguments: resolvedArgs.text,
            argsValid: resolvedArgs.isValid,
            status: "running",
            startedAt: tool.startedAt ?? nowMs,
          };
        }),
        nowMs
      );
      next = {
        ...next,
        status: "streaming",
      };
      changed = true;
      break;
    }
    case "tool_call_end": {
      const toolCallId = getToolCallId(event, context);
      if (isTerminalStatus(state.status) || shouldSkipBySequence(state, event, toolCallId)) {
        break;
      }
      const rawType = extractRawType(event);
      const rawStatus =
        typeof event.metadata?.status === "string"
          ? event.metadata.status.trim().toLowerCase()
          : "";
      const failed = ["cancelled", "not_executed", "error", "failed"].includes(rawStatus);
      const incomingArgs = extractToolArguments(event);
      next = recordSequence(state, event, toolCallId);
      next = upsertToolCall(next, toolCallId, (tool) => {
        const resolvedArgs = resolveToolArguments(
          tool.arguments,
          incomingArgs,
          tool.argsValid
        );
        const resultText =
          rawType === "tool_result" && event.content !== undefined
            ? normalizeString(event.content)
            : tool.result;
        return {
          ...tool,
          id: toolCallId,
          name: extractToolName(event) || tool.name,
          arguments: resolvedArgs.text,
          argsValid: resolvedArgs.isValid,
          result: resultText,
          status: failed ? "error" : "completed",
          endedAt: nowMs,
        };
      });
      next = {
        ...next,
        status: "streaming",
      };
      changed = true;
      break;
    }
    case "error":
      next = failStreamTurn(state, event.error || event.content || "stream_error", nowMs);
      changed = true;
      terminal = true;
      break;
    case "done":
      next = completeStreamTurn(state, nowMs);
      changed = true;
      terminal = true;
      break;
    case "heartbeat":
      if (state.status === "idle") {
        next = { ...state, status: "streaming" };
        changed = true;
      }
      break;
    default:
      break;
  }

  const usageCandidate =
    next.status === "failed"
      ? undefined
      : (event.metadata?.usage as Record<string, unknown> | undefined);
  if (usageCandidate) {
    const withUsage = applyUsageToTurnState(next, usageCandidate);
    if (withUsage !== next) {
      next = withUsage;
      changed = true;
    }
  }

  return {
    state: next,
    changed,
    terminal,
  };
}

export function reduceNormalizedStreamEvents(
  state: StreamTurnState,
  events: NormalizedStreamEvent[],
  context: StreamReducerContext,
  nowMs: number
): StreamReduceResult {
  let next = state;
  let changed = false;
  let terminal = false;
  for (const event of events) {
    const result = reduceNormalizedStreamEvent(next, event, context, nowMs);
    next = result.state;
    changed = changed || result.changed;
    terminal = terminal || result.terminal;
  }
  return { state: next, changed, terminal };
}

export function reduceLegacyStreamChunk(
  state: StreamTurnState,
  chunk: Partial<Omit<StreamChunk, "event_type">> & { event_type?: string },
  context: StreamReducerContext,
  nowMs: number
): StreamReduceResult {
  if (isSdkLifecycleChunk(chunk)) {
    if (isTerminalStatus(state.status)) return { state, changed: false, terminal: false };
    const activity = reduceSdkToolActivity(state.sdkToolActivity, chunk);
    if (!activity) return { state, changed: false, terminal: false };
    return { state: { ...state, sdkToolActivity: activity, status: "streaming" }, changed: true, terminal: false };
  }
  return reduceNormalizedStreamEvents(
    state,
    normalizeLegacyStreamChunk(chunk),
    context,
    nowMs
  );
}
