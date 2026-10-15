/**
 * HTTP transport client for the AI Gateway.
 *
 * Uses Node.js native https/http modules for SSE streaming (more reliable
 * than built-in fetch for streaming in Node.js).
 */

import { randomUUID } from "node:crypto";
import { request as httpsRequest } from "node:https";
import { request as httpRequest } from "node:http";
import type { CLIConfig } from "../types/config.js";
import type { StreamEvent } from "../types/events.js";
import { EventType } from "../types/events.js";
import { CLI_VERSION } from "../version.js";

export interface GatewayRequestOptions {
  signal?: AbortSignal;
  headers?: Record<string, string>;
  timeoutMs?: number;
  /** Opt-in bounded JSON transport for management commands. */
  boundedJson?: boolean;
}

export interface GatewayStreamOptions extends GatewayRequestOptions {
  method?: "GET" | "POST";
  stopOnTerminal?: boolean;
}

export class GatewayHttpError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
    readonly retryAfterMs?: number,
  ) {
    super(`HTTP ${status}: ${body || "request failed"}`);
    this.name = "GatewayHttpError";
  }
}

export class GatewayClient {
  private config: CLIConfig;

  constructor(config: CLIConfig) {
    this.config = config;
  }

  private getHeaders(extra?: Record<string, string>): Record<string, string> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      "X-API-Key": this.config.api_key,
      "X-Tenant-Id": this.config.tenant_id,
      "X-SDK-Version": `cli/${CLI_VERSION}`,
      "X-Request-Id": randomUUID(),
    };
    if (this.config.user_id) {
      headers["X-User-Id"] = this.config.user_id;
    }
    return { ...headers, ...extra };
  }

  private baseUrl(): string {
    return this.config.base_url.replace(/\/$/, "");
  }

  /**
   * Standard JSON request with auth.
   */
  async request(
    method: string,
    path: string,
    body?: Record<string, any>,
    options: GatewayRequestOptions = {},
  ): Promise<any> {
    const url = `${this.baseUrl()}${path}`;
    const headers = this.getHeaders(options.headers);
    const timeoutMs = options.timeoutMs ?? Math.max(1, this.config.timeout) * 1000;
    const controller = new AbortController();
    const forwardAbort = () => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) forwardAbort();
    else options.signal?.addEventListener("abort", forwardAbort, { once: true });
    const timer = setTimeout(() => controller.abort(new Error(`request timed out after ${timeoutMs}ms`)), timeoutMs);
    try {
      const resp = await fetch(url, {
        method,
        headers,
        body: body ? JSON.stringify(body) : undefined,
        signal: controller.signal,
        ...(options.boundedJson && { redirect: "error" as const }),
      });
      if (options.boundedJson) {
        const body = await readBoundedJson(resp);
        if (!resp.ok) throw new GatewayHttpError(resp.status, JSON.stringify(body));
        return body;
      }
      if (resp.ok) return resp.status === 204 ? undefined : await resp.json();
      const errBody = await resp.text();
      throw new GatewayHttpError(resp.status, errBody || resp.statusText, retryAfterMs(resp.headers.get("retry-after")));
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", forwardAbort);
    }
  }

  /**
   * SSE streaming POST using Node.js native http/https modules.
   * Yields parsed StreamEvent objects.
   */
  async *streamSSE(
    path: string,
    body?: Record<string, any>,
    options: GatewayStreamOptions = {},
  ): AsyncGenerator<StreamEvent> {
    const url = new URL(path, this.baseUrl());
    const headers = this.getHeaders(options.headers);
    headers["Accept"] = "text/event-stream";

    const payload = body === undefined ? undefined : JSON.stringify(body);
    if (payload !== undefined) headers["Content-Length"] = String(Buffer.byteLength(payload));

    const reqFn = url.protocol === "https:" ? httpsRequest : httpRequest;

    // Create a promise-based wrapper around the native request
    const response = await new Promise<import("node:http").IncomingMessage>(
      (resolve, reject) => {
        const req = reqFn(
          url,
          { method: options.method ?? (payload === undefined ? "GET" : "POST"), headers },
          (res) => {
            // Preserve multibyte characters across chunks, including decoder EOF flush.
            // Malformed bytes retain Node's replacement-character policy.
            res.setEncoding("utf8");
            if (res.statusCode && res.statusCode >= 400) {
              let body = "";
              res.on("data", (chunk) => (body += chunk));
              res.on("end", () =>
                reject(new GatewayHttpError(
                  res.statusCode ?? 500,
                  body || res.statusMessage || "stream request failed",
                  retryAfterMs(String(res.headers["retry-after"] ?? "")),
                )),
              );
              return;
            }
            resolve(res);
          },
        );
        req.on("error", reject);
        const abort = () => req.destroy(options.signal?.reason instanceof Error ? options.signal.reason : new Error("request aborted"));
        if (options.signal?.aborted) abort();
        else options.signal?.addEventListener("abort", abort, { once: true });
        req.setTimeout(
          options.timeoutMs ?? Math.max(1, this.config.stream_idle_timeout) * 1000,
          () => req.destroy(new Error("SSE stream idle timeout")),
        );
        req.once("close", () => options.signal?.removeEventListener("abort", abort));
        if (payload !== undefined) req.write(payload);
        req.end();
      },
    );

    // Parse SSE from the response stream with cleanup
    let buffer = "";
    let bufEvent: string | null = null;
    let bufId: string | null = null;
    let bufDataLines: string[] = [];

    try {
      for await (const chunk of response) {
        buffer += typeof chunk === "string" ? chunk : chunk.toString("utf-8");
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        for (const rawLine of lines) {
          const line = rawLine.replace(/\r$/, "");

          if (!line) {
            if (bufDataLines.length > 0) {
              const event = parseSSEEvent(bufEvent, bufDataLines, bufId);
              if (event) {
                yield event;
                if (options.stopOnTerminal !== false && isTerminalEvent(event)) return;
              }
            }
            bufEvent = null;
            bufId = null;
            bufDataLines = [];
            continue;
          }

          if (line.startsWith("data:")) {
            bufDataLines.push(line[5] === " " ? line.slice(6) : line.slice(5));
          } else if (line.startsWith("event:")) {
            bufEvent = line.slice(6).trim();
          } else if (line.startsWith("id:")) {
            bufId = line.slice(3).trim();
          }
        }
      }

      // Flush trailing
      if (bufDataLines.length > 0) {
        const event = parseSSEEvent(bufEvent, bufDataLines, bufId);
        if (event) yield event;
      }
    } finally {
      // Ensure stream is destroyed on generator abort or error
      response.destroy();
    }
  }

}

export function isTerminalEvent(event: StreamEvent): boolean {
  return (
    event.eventType === EventType.DONE ||
    event.eventType === EventType.RUN_FINISHED ||
    event.eventType === EventType.ERROR ||
    event.eventType === EventType.RUN_ERROR ||
    event.eventType === EventType.CANCELLED ||
    event.eventType === "response.completed" ||
    event.eventType === "response.failed"
  );
}

export function parseSSEEvent(
  eventField: string | null,
  dataLines: string[],
  idField: string | null = null,
): StreamEvent | null {
  const raw = dataLines.join("\n");

  if (raw.trim() === "[DONE]") {
    return { eventType: EventType.DONE, data: {}, timestamp: Date.now() / 1000 };
  }

  let payload: Record<string, any>;
  try {
    payload = JSON.parse(raw);
  } catch {
    payload = { raw };
  }

  const embeddedEvent = payload.event;
  const eventType = eventField ?? payload.event_type ??
    (typeof embeddedEvent === "string" ? embeddedEvent : undefined) ?? payload.type ?? "message";
  const timestamp = normalizeTimestamp(payload.timestamp);

  // V2 cursor streams wrap the V1-compatible payload in an `item` envelope.
  // Normalize it here so CLI consumers see the same event vocabulary from
  // either endpoint.
  if (eventType === EventType.ITEM && payload.schema_version === "agent-event/v2") {
    const envelope = embeddedEvent && typeof embeddedEvent === "object"
      ? embeddedEvent as Record<string, any>
      : payload;
    const raw = envelope.payload && typeof envelope.payload === "object"
      ? envelope.payload as Record<string, any>
      : {};
    const rawType = typeof raw.event_type === "string" ? raw.event_type : "";
    const rawData = raw.data;
    if (rawType && rawType !== "item" && rawType !== "rollout/item") {
      const projected = rawData && typeof rawData === "object" && !Array.isArray(rawData)
        ? rawData
        : typeof rawData === "string" ? { content: rawData } : { value: rawData };
      return withV2Metadata({ eventType: rawType, data: projected, timestamp }, payload, envelope, idField);
    }
    const item = rawData && typeof rawData === "object" && !Array.isArray(rawData)
      ? rawData as Record<string, any>
      : {};
    const itemPayload = item.payload && typeof item.payload === "object"
      ? item.payload as Record<string, any>
      : item;
    const text = typeof itemPayload.message === "string"
      ? itemPayload.message
      : typeof itemPayload.text === "string" ? itemPayload.text :
        Array.isArray(itemPayload.content)
          ? itemPayload.content.map((part: any) => part?.text ?? part?.content ?? "").join("")
          : "";
    if (text && (itemPayload.role === "assistant" || itemPayload.type === "agent_message")) {
      return withV2Metadata({ eventType: EventType.TEXT_DELTA, data: { content: text }, timestamp }, payload, envelope, idField);
    }
    if (text && (itemPayload.role === "reasoning" || itemPayload.type === "reasoning")) {
      return withV2Metadata({ eventType: EventType.THINKING_DELTA, data: { content: text }, timestamp }, payload, envelope, idField);
    }
    const toolName = itemPayload.name ?? itemPayload.tool;
    if (toolName || ["function_call", "tool_use", "command_execution", "mcp_tool_call"].includes(String(item.type))) {
      const status = String(item.status ?? itemPayload.status ?? "").toLowerCase();
      const terminal = ["completed", "succeeded", "failed", "error", "cancelled"].includes(status);
      return withV2Metadata({
        eventType: terminal ? EventType.TOOL_CALL_RESULT : EventType.TOOL_CALL_START,
        data: {
          tool_call_id: item.id ?? itemPayload.id,
          tool_name: toolName ?? item.type,
          arguments: itemPayload.arguments ?? itemPayload.input,
          result: itemPayload.result ?? itemPayload.output,
          status: item.status ?? itemPayload.status,
        },
        timestamp,
      }, payload, envelope, idField);
    }
    if (itemPayload.approval_id || item.type === "approval_request") {
      return withV2Metadata({ eventType: EventType.APPROVAL_REQUIRED, data: itemPayload, timestamp }, payload, envelope, idField);
    }
    if (itemPayload.artifact_id || item.type === "artifact") {
      return withV2Metadata({ eventType: EventType.ARTIFACT_CREATED, data: itemPayload, timestamp }, payload, envelope, idField);
    }
    if (item.type === "activity" || item.type === "event_msg") {
      return withV2Metadata({ eventType: EventType.ACTIVITY, data: itemPayload, timestamp }, payload, envelope, idField);
    }
    return null;
  }

  // Unwrap nested "data" — can be string (text_delta) or object
  const genericPayload = { ...payload };
  delete genericPayload.event_type;
  if (typeof genericPayload.event === "string") delete genericPayload.event;
  delete genericPayload.timestamp;
  const inner = genericPayload.data !== undefined ? genericPayload.data : genericPayload;

  let data: Record<string, any>;
  if (typeof inner === "string") {
    data = { content: inner };
  } else if (typeof inner === "object" && inner !== null && !Array.isArray(inner)) {
    data = inner;
  } else {
    data = { value: inner };
  }

  return {
    eventType,
    data,
    timestamp,
    ...(Number.isFinite(Number(payload.sequence_number)) ? { sequence: Number(payload.sequence_number) } : {}),
    ...(idField ? { eventId: idField } : {}),
  };
}

function normalizeTimestamp(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const numeric = Number(value);
    if (value.trim() && Number.isFinite(numeric)) return numeric;
    const parsed = Date.parse(value);
    if (Number.isFinite(parsed)) return parsed / 1000;
  }
  return Date.now() / 1000;
}

function withV2Metadata(
  event: StreamEvent,
  outer: Record<string, any>,
  envelope: Record<string, any>,
  idField: string | null,
): StreamEvent {
  const sequence = Number(outer.sequence ?? idField);
  return {
    ...event,
    ...(Number.isFinite(sequence) ? { sequence } : {}),
    ...(typeof outer.thread_id === "string" ? { threadId: outer.thread_id } : {}),
    ...(typeof envelope.turn_id === "string" ? { turnId: envelope.turn_id } : {}),
    ...(typeof envelope.id === "string" ? { eventId: envelope.id } : idField ? { eventId: idField } : {}),
  };
}

function retryAfterMs(value: string | null): number | undefined {
  if (!value) return undefined;
  const seconds = Number(value);
  if (Number.isFinite(seconds) && seconds >= 0) return seconds * 1000;
  const date = Date.parse(value);
  if (!Number.isFinite(date)) return undefined;
  return Math.max(0, date - Date.now());
}

async function readBoundedJson(response: Response): Promise<unknown> {
  if (!response.body) throw new Error("Missing JSON response");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 8 * 1024 * 1024) throw new Error("JSON response limit exceeded");
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally { reader.releaseLock(); }
}
