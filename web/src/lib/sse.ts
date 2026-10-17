export type SSEMessage<T = unknown> = { data: T };
export type SSEEvent<T = unknown> = { event: string; data: T; id?: string };
import { getAuthToken } from "@/lib/api";
import { parseSseEventPart } from "./sseEventParser";

export type SSEConnectionState = "connecting" | "streaming" | "background" | "reconnecting" | "unknown_outcome" | "server_terminal";
export interface SSEFetchOptions extends RequestInit {
  signal?: AbortSignal;
  /** Compatibility alias for idleTimeoutMs. No overall turn deadline is imposed. 0 disables. */
  timeoutMs?: number;
  /** Maximum foreground silence without received bytes, including keep-alive comments. */
  idleTimeoutMs?: number;
  preserveControlFrames?: boolean;
  onConnectionState?: (state: SSEConnectionState) => void;
  /** sseFetchEvents only: enable on a read-only GET endpoint that supports Last-Event-ID replay. Never replays a POST. */
  resume?: {
    maxRetries?: number;
    initialDelayMs?: number;
    maxDelayMs?: number;
    isTerminal: (event: SSEEvent<unknown>) => boolean;
    /** Protocol-owned reset may deliberately reuse an older retained cursor. */
    isReset?: (event: SSEEvent<unknown>) => boolean;
  };
}
export class SSEIdleTimeoutError extends Error {
  constructor() { super("SSE idle budget exceeded; the server outcome is unknown"); this.name = "SSEIdleTimeoutError"; }
}
export class SSEInterruptedError extends Error {
  readonly outcomeUnknown = true;
  constructor(cause: unknown) { super("SSE connection interrupted; the server outcome is unknown", { cause }); this.name = "SSEInterruptedError"; }
}
export class SSERequestError extends Error {
  readonly status: number;
  constructor(status: number, detail: string) { super(`SSE request failed: ${status}${detail ? ` ${detail}` : ""}`); this.name = "SSERequestError"; this.status = status; }
}
const DEFAULT_SSE_IDLE_MS = 45_000;
function authenticatedHeaders(headers?: HeadersInit): Headers {
  const resolved = new Headers(headers);
  const token = getAuthToken();
  if (token && !resolved.has("Authorization")) resolved.set("Authorization", `Bearer ${token}`);
  return resolved;
}
function notify(init: SSEFetchOptions, state: SSEConnectionState) {
  // Observers are diagnostic/UI sinks and must not change dispatch or replay semantics.
  try { init.onConnectionState?.(state); } catch { /* Ignore observer failure. */ }
}
function idleSignal(init: SSEFetchOptions) {
  const controller = new AbortController();
  const ms = init.idleTimeoutMs ?? init.timeoutMs ?? DEFAULT_SSE_IDLE_MS;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const visibility = typeof document === "undefined" ? undefined : document;
  const touch = () => {
    clearTimeout(timer);
    if (ms > 0 && !controller.signal.aborted && visibility?.visibilityState !== "hidden") {
      timer = setTimeout(() => controller.abort(new SSEIdleTimeoutError()), ms);
    }
  };
  const forwardAbort = () => controller.abort(init.signal?.reason);
  const visible = () => { touch(); notify(init, visibility?.visibilityState === "hidden" ? "background" : "streaming"); };
  if (init.signal?.aborted) forwardAbort();
  else init.signal?.addEventListener("abort", forwardAbort, { once: true });
  visibility?.addEventListener("visibilitychange", visible);
  touch();
  return { signal: controller.signal, touch, cleanup: () => {
    clearTimeout(timer);
    init.signal?.removeEventListener("abort", forwardAbort);
    visibility?.removeEventListener("visibilitychange", visible);
  } };
}
async function readResponseErrorDetail(resp: Response): Promise<string> {
  const text = (await resp.text().catch(() => "")).trim();
  if (!text) return "";
  try {
    const payload = JSON.parse(text) as Record<string, unknown>;
    const detail = payload.detail ?? payload.message ?? payload.error;
    if (typeof detail === "string") return detail.trim();
    if (detail != null) return JSON.stringify(detail);
  } catch { /* Keep plain-text error details. */ }
  return text.replace(/\s+/g, " ").trim().slice(0, 400);
}

/** Equal jitter, capped even after many failures; deterministic bounds are unit tested. */
export function sseReconnectDelayMs(attempt: number, initial = 250, maximum = 10_000): number {
  const cap = Math.min(maximum, initial * 2 ** Math.min(30, Math.max(0, attempt)));
  return Math.floor(cap / 2 + Math.random() * cap / 2);
}
export function waitForSseReconnect(signal?: AbortSignal, attempt = 0, initial = 250, maximum = 10_000): Promise<void> {
  if (signal?.aborted) return Promise.resolve();
  return new Promise(resolve => {
    const visibility = typeof document === "undefined" ? undefined : document;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const done = () => { clearTimeout(timer); signal?.removeEventListener("abort", done); visibility?.removeEventListener("visibilitychange", schedule); resolve(); };
    const schedule = () => {
      clearTimeout(timer);
      if (visibility?.visibilityState !== "hidden") timer = setTimeout(done, sseReconnectDelayMs(attempt, initial, maximum));
    };
    signal?.addEventListener("abort", done, { once: true });
    visibility?.addEventListener("visibilitychange", schedule);
    schedule();
  });
}

/** One byte reader for all protocols. A disconnect never flushes a partial frame as a complete event. */
async function* readSseParts(url: string, init: SSEFetchOptions): AsyncGenerator<string> {
  const budget = idleSignal(init);
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  const cancel = () => { void reader?.cancel().catch(() => {}); };
  notify(init, "connecting");
  try {
    const { timeoutMs: _timeout, idleTimeoutMs: _idle, preserveControlFrames: _control, onConnectionState: _state, resume: _resume, ...request } = init;
    void _timeout; void _idle; void _control; void _state; void _resume;
    const response = await fetch(url, { ...request, headers: authenticatedHeaders(init.headers), signal: budget.signal });
    if (!response.ok || !response.body) throw new SSERequestError(response.status, await readResponseErrorDetail(response));
    budget.touch();
    reader = response.body.getReader();
    budget.signal.addEventListener("abort", cancel, { once: true });
    if (budget.signal.aborted) cancel();
    notify(init, typeof document !== "undefined" && document.visibilityState === "hidden" ? "background" : "streaming");
    const decoder = new TextDecoder();
    let buffer = "";
    while (!budget.signal.aborted) {
      const { done, value } = await reader.read();
      if (budget.signal.aborted) break;
      if (value?.byteLength) budget.touch();
      if (done) {
        buffer += decoder.decode();
        // Preserve legacy final-frame handling on a clean EOF only.
        if (buffer.trim()) yield buffer.replace(/\r\n/g, "\n");
        return;
      }
      buffer += decoder.decode(value, { stream: true });
      const parts = buffer.replace(/\r\n/g, "\n").split("\n\n");
      buffer = parts.pop() ?? "";
      for (const part of parts) yield part;
    }
    if (budget.signal.reason instanceof SSEIdleTimeoutError) throw budget.signal.reason;
  } catch (error) {
    if (!init.signal?.aborted) {
      notify(init, "unknown_outcome");
      if (!(error instanceof SSERequestError) && !(error instanceof SSEIdleTimeoutError)) throw new SSEInterruptedError(error);
    }
    throw error;
  } finally {
    budget.signal.removeEventListener("abort", cancel);
    await reader?.cancel().catch(() => {});
    reader?.releaseLock();
    budget.cleanup();
  }
}
function dataText(part: string) {
  return part.split("\n").filter(line => line.startsWith("data:")).map(line => line.slice(5).trim()).join("\n");
}
// ============================================================================
// AG-UI Protocol Types
// ============================================================================

/** AG-UI Event Types - aligned with backend agui_protocol.py */
export type AGUIEventType =
  // Lifecycle
  | "run_started"
  | "run_finished"
  | "run_error"
  | "step_started"
  | "step_finished"
  // Text Message
  | "text_message_start"
  | "text_message_content"
  | "text_message_end"
  | "text_delta"
  // Tool Call
  | "tool_call_start"
  | "tool_call_args"
  | "tool_call_result"
  | "tool_call_end"
  | "tool_error"
  // State
  | "state_snapshot"
  | "state_delta"
  | "messages_snapshot"
  // Artifacts
  | "artifact_created"
  | "file_creating"
  | "file_created"
  // Document
  | "outline_ready"
  | "document_generation_start"
  | "document_generation_result"
  // Search
  | "search_started"
  | "search_progress"
  | "search_completed"
  // Code Execution
  | "code_execution_start"
  | "code_execution_result"
  // Image
  | "image_generation_start"
  | "image_generation_result"
  // Status
  | "status"
  // ReAct Thinking (transparent reasoning)
  | "thinking_start"
  | "thinking_delta"
  | "thinking_end"
  | "thinking_error"
  // Phase tracking (AgentLoop optimization)
  | "phase_started"
  | "phase_completed"
  // Error and cancellation
  | "error"
  | "cancelled"
  // Special
  | "stream_end"
  | "custom_event"
  | "raw_event";

/** Base AG-UI Event structure */
export interface AGUIEvent {
  event: AGUIEventType;
  request_id: string;
  chunk_index: number;
  timestamp?: number;
  // Lifecycle events
  run_id?: string;
  thread_id?: string;
  // Step events
  step_id?: string;
  step_name?: string;
  step_type?: string;
  parent_step_id?: string;
  // Text events
  message_id?: string;
  content?: string;
  role?: string;
  // Tool call events
  tool_call_id?: string;
  tool_name?: string;
  arguments?: string;
  result?: unknown;
  error?: string;
  status?: string;
  // Artifact events
  artifact_id?: string;
  artifact_type?: string;
  file_id?: string;
  file_name?: string;
  name?: string;
  url?: string;
  mime_type?: string;
  size?: number;
  // Document events
  doc_type?: string;
  title?: string;
  sections?: string[];
  // Search events
  query?: string;
  search_type?: string;
  result_count?: number;
  // Code events
  code?: string;
  language?: string;
  output?: string;
  success?: boolean;
  // Image events
  prompt?: string;
  // Status events
  message?: string;
  phase?: string;
  progress?: number;
  // Generic metadata
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

/** Data-only compatibility API. Mutating inference requests are dispatched once. */
export async function* sseFetch<T>(url: string, init: SSEFetchOptions): AsyncGenerator<T, void, void> {
  for await (const part of readSseParts(url, init)) {
    const text = dataText(part);
    if (!text || text === "[DONE]") continue;
    let value: T;
    try { value = JSON.parse(text) as T; } catch { continue; }
    yield value;
  }
}

/** Named events, with opt-in bounded replay exclusively for resumable read-only endpoints. */
export async function* sseFetchEvents<T>(url: string, init: SSEFetchOptions): AsyncGenerator<SSEEvent<T>, void, void> {
  if (init.signal?.aborted) throw init.signal.reason ?? new DOMException("Aborted", "AbortError");
  const resume = init.resume;
  if (resume && (init.method ?? "GET").toUpperCase() !== "GET") throw new Error("SSE resume requires a read-only GET endpoint; inference must never be resubmitted");
  const authority = getAuthToken();
  let cursor = new Headers(init.headers).get("Last-Event-ID") ?? "";
  const seen = new Set<string>();
  if (cursor) seen.add(cursor);
  let deliveredWithoutCursor = false;
  let attempts = 0;
  while (!init.signal?.aborted) {
    if (resume && authority !== getAuthToken()) throw new Error("SSE authority changed; resume cancelled");
    const headers = new Headers(init.headers);
    if (cursor) headers.set("Last-Event-ID", cursor);
    try {
      for await (const part of readSseParts(url, { ...init, headers })) {
        const event = parseSseEventPart<T>(part, { preserveControlFrames: init.preserveControlFrames });
        if (!event) continue;
        if (resume?.isReset?.(event)) seen.clear();
        if (resume && event.id && seen.has(event.id)) continue;
        if (event.id !== undefined) {
          cursor = event.id;
          if (cursor) seen.add(cursor);
        }
        if (resume && !event.id) deliveredWithoutCursor = true;
        const terminal = resume?.isTerminal(event);
        if (terminal) notify(init, "server_terminal");
        yield event;
        if (terminal) return;
      }
      if (!resume || init.signal?.aborted) return;
    } catch (error) {
      if (!resume || init.signal?.aborted || (error instanceof SSERequestError && error.status < 500 && ![408, 425, 429].includes(error.status))) throw error;
    }
    // An event without an ID cannot be replayed without possibly repeating tokens/effects.
    if (deliveredWithoutCursor || attempts >= (resume?.maxRetries ?? 5)) {
      notify(init, "unknown_outcome");
      throw new Error("SSE disconnected without a safe resume cursor or exhausted its retry budget; the server outcome is unknown");
    }
    notify(init, "reconnecting");
    await waitForSseReconnect(init.signal, attempts++, resume?.initialDelayMs, resume?.maxDelayMs);
  }
}

/** AG-UI uses the same byte activity budget, auth and cleanup as the other clients. */
export async function* sseFetchAGUI(url: string, init: SSEFetchOptions): AsyncGenerator<AGUIEvent, void, void> {
  let count = 0;
  for await (const part of readSseParts(url, init)) {
    const text = dataText(part);
    if (!text || text === "[DONE]") continue;
    const eventType = part.split("\n").find(line => line.startsWith("event:"))?.slice(6).trim();
    let event: AGUIEvent;
    try {
      event = JSON.parse(text) as AGUIEvent;
      if (eventType && !event.event) event.event = eventType as AGUIEventType;
      event.request_id ||= "";
      event.chunk_index ??= count;
      event.timestamp ??= Date.now() / 1000;
    } catch {
      event = { event: "raw_event", request_id: "", chunk_index: count, timestamp: Date.now() / 1000, raw: text };
    }
    count++;
    yield event;
  }
}

/**
 * Convert legacy StreamChunk format to AG-UI event format.
 * This helps migrate existing code that uses the old format.
 */
export function streamChunkToAGUIEvent(chunk: {
  event_type?: string;
  request_id?: string;
  chunk_index?: number;
  content?: { type?: string; data?: unknown };
  tool_call?: {
    tool_call_id?: string;
    name?: string;
    arguments?: string;
    status?: string;
  };
  metadata?: Record<string, unknown>;
}): AGUIEvent {
  const eventType = chunk.event_type || "text_delta";

  // Map legacy event types to AG-UI events
  const eventMap: Record<string, AGUIEventType> = {
    "text_delta": "text_message_content",
    "tool_call_start": "tool_call_start",
    "tool_call_delta": "tool_call_args",
    "tool_call_end": "tool_call_end",
    "tool_result": "tool_call_result",
    "stream_end": "stream_end",
    "thinking": "status",
  };

  const aguiEvent: AGUIEvent = {
    event: eventMap[eventType] || (eventType as AGUIEventType),
    request_id: chunk.request_id || "",
    chunk_index: chunk.chunk_index ?? 0,
    timestamp: Date.now() / 1000,
    metadata: chunk.metadata,
  };

  // Map content
  if (chunk.content?.data !== undefined) {
    aguiEvent.content = String(chunk.content.data);
  }

  // Map tool call
  if (chunk.tool_call) {
    aguiEvent.tool_call_id = chunk.tool_call.tool_call_id;
    aguiEvent.tool_name = chunk.tool_call.name;
    aguiEvent.arguments = chunk.tool_call.arguments;
    aguiEvent.status = chunk.tool_call.status;
  }

  return aguiEvent;
}
