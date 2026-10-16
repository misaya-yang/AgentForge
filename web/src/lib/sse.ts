export type SSEMessage<T = unknown> = { data: T };
export type SSEEvent<T = unknown> = { event: string; data: T; id?: string };

import { isSseDebugEnabled } from "@/config/runtime";
import { getAuthToken } from "@/lib/api";
import { parseSseEventPart } from "./sseEventParser";

export interface SSEFetchOptions extends RequestInit {
  signal?: AbortSignal;
  /** SSE stream timeout in ms. Default: 5 minutes. Set 0 to disable. */
  timeoutMs?: number;
  /** sseFetchEvents only: retain raw named/data-less and [DONE] control frames. */
  preserveControlFrames?: boolean;
  /** Return only the HTTP status instead of parsing an endpoint error body. */
  httpErrorMode?: "status";
}

const DEFAULT_SSE_TIMEOUT_MS = 5 * 60 * 1000; // 5 minutes

export class SSEHttpError extends Error {
  readonly status: number;

  constructor(status: number) {
    super(`SSE request failed: ${status}`);
    this.name = "SSEHttpError";
    this.status = status;
  }
}

function authenticatedHeaders(headers?: HeadersInit): Headers {
  const resolved = new Headers(headers);
  const token = getAuthToken();
  if (token && !resolved.has("Authorization")) {
    resolved.set("Authorization", `Bearer ${token}`);
  }
  return resolved;
}

/**
 * Create a combined AbortSignal that fires on either the caller's signal or a timeout.
 */
function createTimeoutSignal(
  existingSignal?: AbortSignal | null,
  timeoutMs?: number,
): { signal: AbortSignal; cleanup: () => void } {
  const ms = timeoutMs ?? DEFAULT_SSE_TIMEOUT_MS;
  if (ms <= 0 && !existingSignal) {
    return { signal: new AbortController().signal, cleanup: () => {} };
  }
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | null = null;
  if (ms > 0) {
    timer = setTimeout(() => controller.abort(new Error("SSE stream timeout")), ms);
  }
  const forwardAbort = () => controller.abort(existingSignal?.reason);
  if (existingSignal) {
    if (existingSignal.aborted) forwardAbort();
    else existingSignal.addEventListener("abort", forwardAbort, { once: true });
  }
  return {
    signal: controller.signal,
    cleanup: () => {
      if (timer) clearTimeout(timer);
      existingSignal?.removeEventListener("abort", forwardAbort);
    },
  };
}

async function readResponseErrorDetail(resp: Response): Promise<string> {
  const text = (await resp.text().catch(() => "")).trim();
  if (!text) return "";

  try {
    const payload = JSON.parse(text) as Record<string, unknown>;
    const detail = payload.detail ?? payload.message ?? payload.error;
    if (typeof detail === "string") return detail.trim();
    if (detail != null) return JSON.stringify(detail);
  } catch {
    // Keep plain response body if it's not JSON.
  }

  return text.replace(/\s+/g, " ").trim().slice(0, 400);
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

/**
 * Read authenticated SSE data. Returning early (including a durable terminal)
 * cancels the body; failed connections release their timeout and abort listener
 * so callers can reconnect without retaining resources from the previous read.
 */
export async function* sseFetch<T>(
  url: string,
  init: SSEFetchOptions
): AsyncGenerator<T, void, void> {
  const { httpErrorMode, ...requestInit } = init;
  const debug = import.meta.env.DEV && isSseDebugEnabled();
  const startTime = performance.now();
  if (debug) {
    console.log(`[SSE] Starting fetch to ${url}`);
  }

  const { signal: timeoutSignal, cleanup: cleanupTimeout } = createTimeoutSignal(init.signal, init.timeoutMs);
  let resp: Response;
  try {
    resp = await fetch(url, {
      ...requestInit,
      headers: authenticatedHeaders(init.headers),
      signal: timeoutSignal,
    });
  } catch (error) {
    cleanupTimeout();
    throw error;
  }

  if (debug) {
    console.log(
      `[SSE] Response received in ${(performance.now() - startTime).toFixed(0)}ms, status=${resp.status}`
    );
  }
  
  if (!resp.ok || !resp.body) {
    if (httpErrorMode === "status") {
      await resp.body?.cancel().catch(() => {});
      cleanupTimeout();
      throw new SSEHttpError(resp.status);
    }
    try {
      const errorDetail = await readResponseErrorDetail(resp);
      const suffix = errorDetail ? ` ${errorDetail}` : "";
      throw new Error(`SSE request failed: ${resp.status}${suffix}`);
    } finally {
      cleanupTimeout();
    }
  }
  const reader = resp.body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let chunkCount = 0;
  let firstChunkTime: number | null = null;
  let aborted = false;
  const abortReader = () => {
    aborted = true;
    void reader.cancel().catch(() => {});
  };
  timeoutSignal.addEventListener("abort", abortReader, { once: true });

  try {
    while (true) {
      if (aborted || timeoutSignal.aborted) {
        break;
      }
      const { done, value } = await reader.read();
      
      if (value && firstChunkTime === null) {
        firstChunkTime = performance.now();
        if (debug) {
          console.log(
            `[SSE] First data chunk received at ${(firstChunkTime - startTime).toFixed(0)}ms, size=${value.length}`
          );
        }
      }
      
      if (done) {
        if (debug) {
          console.log(
            `[SSE] Stream done. Total chunks: ${chunkCount}, duration: ${(performance.now() - startTime).toFixed(0)}ms`
          );
        }
        // 处理缓冲区中剩余的数据
        if (buffer.trim()) {
          const line = buffer.split("\n").find((l) => l.startsWith("data:"));
          if (line) {
            const jsonStr = line.slice(5).trim();
            if (jsonStr && jsonStr !== "[DONE]") {
              try {
                const parsed = JSON.parse(jsonStr);
                yield parsed as T;
              } catch {
                // 解析失败，忽略
              }
            }
          }
        }
        break;
      }
      
      buffer += decoder.decode(value, { stream: true });
      // SSE events can be separated by \n\n or \r\n\r\n - normalize to \n\n first
      const normalizedBuffer = buffer.replace(/\r\n/g, "\n");
      const parts = normalizedBuffer.split("\n\n");
      buffer = parts.pop() || "";
      
      for (const part of parts) {
        const lines = part.split("\n");
        // 找到 data: 行
        const dataLine = lines.find((l) => l.startsWith("data:"));
        if (!dataLine) continue;
        
        const jsonStr = dataLine.slice(5).trim();
        if (!jsonStr || jsonStr === "[DONE]") continue;
        
        try {
          const parsed = JSON.parse(jsonStr);
          chunkCount++;
          if (debug && (chunkCount <= 3 || chunkCount % 50 === 0)) {
            console.log(
              `[SSE] Yielding chunk #${chunkCount} at ${(performance.now() - startTime).toFixed(0)}ms`
            );
          }
          yield parsed as T;
        } catch (e) {
          if (debug) {
            console.warn("SSE parse error:", e, jsonStr);
          }
          continue;
        }
      }
    }
  } finally {
    timeoutSignal.removeEventListener("abort", abortReader);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
    cleanupTimeout();
  }
}

export async function* sseFetchEvents<T>(
  url: string,
  init: SSEFetchOptions
): AsyncGenerator<SSEEvent<T>, void, void> {
  const debug = import.meta.env.DEV && isSseDebugEnabled();
  const startTime = performance.now();

  if (debug) console.log(`[SSE-Events] Starting fetch to ${url}`);
  const { signal, cleanup: cleanupTimeout } = createTimeoutSignal(
    init.signal,
    init.timeoutMs
  );

  let resp: Response;
  try {
    resp = await fetch(url, {
      ...init,
      headers: authenticatedHeaders(init.headers),
      signal,
    });
  } catch (error) {
    cleanupTimeout();
    throw error;
  }

  if (debug) {
    console.log(`[SSE-Events] Response: status=${resp.status}, content-type=${resp.headers.get("content-type")}`);
  }

  if (!resp.ok || !resp.body) {
    const errorDetail = await readResponseErrorDetail(resp);
    const suffix = errorDetail ? ` ${errorDetail}` : "";
    cleanupTimeout();
    throw new Error(`SSE request failed: ${resp.status}${suffix}`);
  }

  const reader = resp.body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let firstChunkTime: number | null = null;
  let yieldedCount = 0;
  let aborted = false;
  const abortReader = () => {
    aborted = true;
    void reader.cancel().catch(() => {});
  };
  signal.addEventListener("abort", abortReader, { once: true });

  try {
    while (true) {
      if (aborted || signal.aborted) {
        break;
      }
      const { done, value } = await reader.read();

      if (value && firstChunkTime === null) {
        firstChunkTime = performance.now();
        if (debug) {
          console.log(`[SSE-Events] First chunk at ${(firstChunkTime - startTime).toFixed(0)}ms, size=${value.length}`);
        }
      }

      if (done) {
        if (debug) {
          console.log(`[SSE-Events] Stream done. Total yielded: ${yieldedCount}, duration: ${(performance.now() - startTime).toFixed(0)}ms`);
        }

        if (buffer.trim()) {
          const evt = parseSseEventPart<T>(buffer, { preserveControlFrames: init.preserveControlFrames });
          if (evt) {
            yieldedCount++;
            yield evt;
          }
        }
        break;
      }

      buffer += decoder.decode(value, { stream: true });
      // SSE events can be separated by \n\n or \r\n\r\n - normalize to \n\n first
      const normalizedBuffer = buffer.replace(/\r\n/g, "\n");
      const parts = normalizedBuffer.split("\n\n");
      // Keep the last incomplete part in buffer
      const lastPart = parts.pop() || "";
      buffer = lastPart;

      for (const part of parts) {
        const evt = parseSseEventPart<T>(part, { preserveControlFrames: init.preserveControlFrames });
        if (!evt) continue;
        yieldedCount++;
        if (debug && (yieldedCount <= 3 || yieldedCount % 50 === 0)) {
          console.log(`[SSE-Events] Yielding #${yieldedCount} event='${evt.event}' at ${(performance.now() - startTime).toFixed(0)}ms`);
        }
        yield evt;
      }
    }
  } finally {
    signal.removeEventListener("abort", abortReader);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
    cleanupTimeout();
  }
}

// ============================================================================
// AG-UI Protocol SSE Fetch
// ============================================================================

/**
 * AG-UI Protocol aware SSE fetch.
 *
 * This function handles the AG-UI SSE format:
 *   event: EVENT_TYPE
 *   data: {"event": "EVENT_TYPE", "request_id": "...", ...}
 *
 * It yields typed AGUIEvent objects that can be processed by useAgentTimeline
 * and useAgentStream hooks.
 *
 * @param url - The SSE endpoint URL
 * @param init - Fetch options
 * @returns AsyncGenerator of AGUIEvent objects
 */
export async function* sseFetchAGUI(
  url: string,
  init: SSEFetchOptions
): AsyncGenerator<AGUIEvent, void, void> {
  const debug = import.meta.env.DEV && isSseDebugEnabled();
  const startTime = performance.now();

  if (debug) console.log(`[SSE-AGUI] Starting fetch to ${url}`);

  const resp = await fetch(url, {
    ...init,
    signal: init.signal,
  });

  if (debug) {
    console.log(`[SSE-AGUI] Response: status=${resp.status}, content-type=${resp.headers.get("content-type")}`);
  }

  if (!resp.ok || !resp.body) {
    const errorDetail = await readResponseErrorDetail(resp);
    const suffix = errorDetail ? ` ${errorDetail}` : "";
    throw new Error(`SSE request failed: ${resp.status}${suffix}`);
  }

  const reader = resp.body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let firstChunkTime: number | null = null;
  let yieldedCount = 0;
  let aborted = false;
  const abortReader = () => {
    aborted = true;
    void reader.cancel().catch(() => {});
  };
  init.signal?.addEventListener("abort", abortReader, { once: true });

  const parseAGUIPart = (part: string): AGUIEvent | null => {
    const lines = part.split("\n");
    let eventType: AGUIEventType | "" = "";
    const dataLines: string[] = [];

    for (const line of lines) {
      if (line.startsWith("event:")) {
        eventType = line.slice(6).trim() as AGUIEventType;
      } else if (line.startsWith("data:")) {
        dataLines.push(line.slice(5).trim());
      }
    }

    const dataStr = dataLines.join("\n");
    if (!dataStr || dataStr === "[DONE]") return null;

    try {
      const parsed = JSON.parse(dataStr) as AGUIEvent;
      // Use event type from SSE event field if present, otherwise from data
      if (eventType && !parsed.event) {
        parsed.event = eventType;
      }
      // Ensure required fields have defaults
      parsed.request_id = parsed.request_id || "";
      parsed.chunk_index = parsed.chunk_index ?? yieldedCount;
      parsed.timestamp = parsed.timestamp ?? Date.now() / 1000;
      return parsed;
    } catch (e) {
      if (debug) {
        console.warn("[SSE-AGUI] Failed to parse event:", dataStr, e);
      }
      // Return raw event for unparseable data
      return {
        event: "raw_event",
        request_id: "",
        chunk_index: yieldedCount,
        timestamp: Date.now() / 1000,
        raw: dataStr,
      };
    }
  };

  try {
    while (true) {
      if (aborted || init.signal?.aborted) {
        break;
      }
      const { done, value } = await reader.read();

      if (value && firstChunkTime === null) {
        firstChunkTime = performance.now();
        if (debug) {
          console.log(`[SSE-AGUI] First chunk at ${(firstChunkTime - startTime).toFixed(0)}ms, size=${value.length}`);
        }
      }

      if (done) {
        if (debug) {
          console.log(`[SSE-AGUI] Stream done. Total yielded: ${yieldedCount}, duration: ${(performance.now() - startTime).toFixed(0)}ms`);
        }

        // Process remaining buffer
        if (buffer.trim()) {
          const evt = parseAGUIPart(buffer);
          if (evt) {
            yieldedCount++;
            yield evt;
          }
        }
        break;
      }

      buffer += decoder.decode(value, { stream: true });
      // SSE events are separated by \n\n or \r\n\r\n - normalize first
      const normalizedBuffer = buffer.replace(/\r\n/g, "\n");
      const parts = normalizedBuffer.split("\n\n");
      // Keep the last incomplete part in buffer
      buffer = parts.pop() || "";

      for (const part of parts) {
        const evt = parseAGUIPart(part);
        if (!evt) continue;
        yieldedCount++;
        if (debug && (yieldedCount <= 3 || yieldedCount % 50 === 0)) {
          console.log(`[SSE-AGUI] Yielding #${yieldedCount} event='${evt.event}' at ${(performance.now() - startTime).toFixed(0)}ms`);
        }
        yield evt;
      }
    }
  } finally {
    init.signal?.removeEventListener("abort", abortReader);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
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
