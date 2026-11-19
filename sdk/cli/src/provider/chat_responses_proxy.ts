import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";

import type { ProviderProfile } from "./config.js";
import {
  CompatibilityError,
  responsesToChat,
  namespaceBindings,
  type ResponsesRequest,
} from "./chat_request_adapter.js";
import { projectChatStream } from "./chat_stream_projector.js";

export { responsesToChat } from "./chat_request_adapter.js";

export const LOCAL_PROXY_TOKEN_ENV = "AI_GATEWAY_LOCAL_PROXY_TOKEN";

export interface ChatCompatibilityProxy {
  baseUrl: string;
  token: string;
  close(): Promise<void>;
}

export async function startChatCompatibilityProxy(
  provider: ProviderProfile,
  env: NodeJS.ProcessEnv = process.env,
): Promise<ChatCompatibilityProxy> {
  if (provider.wire_api !== "chat_completions") {
    throw new Error("Chat compatibility proxy requires a chat_completions provider");
  }
  const token = randomBytes(32).toString("base64url");
  const server = createServer((request, response) => {
    void handleProxyRequest(request, response, provider, env, token);
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.removeListener("error", reject);
      resolve();
    });
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("Local Responses compatibility proxy did not bind a TCP port");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    token,
    close: () => new Promise<void>((resolve, reject) => {
      server.close((error) => error ? reject(error) : resolve());
      server.closeAllConnections();
    }),
  };
}

async function handleProxyRequest(
  request: IncomingMessage,
  response: ServerResponse,
  provider: ProviderProfile,
  env: NodeJS.ProcessEnv,
  localToken: string,
): Promise<void> {
  response.setHeader("Cache-Control", "no-store");
  response.setHeader("X-Content-Type-Options", "nosniff");
  if (request.method !== "POST" || request.url !== "/v1/responses") {
    jsonError(response, 404, "unsupported_local_proxy_route");
    return;
  }
  if (!matchesLocalToken(request.headers.authorization, localToken)) {
    jsonError(response, 401, "local_proxy_authentication_failed");
    return;
  }

  const clientAbort = new AbortController();
  request.once("aborted", () => clientAbort.abort(new Error("CLI runtime disconnected")));
  response.once("close", () => {
    if (!response.writableEnded) clientAbort.abort(new Error("CLI runtime disconnected"));
  });
  try {
    let body: ResponsesRequest;
    try {
      body = JSON.parse(await readBody(request, 16 * 1024 * 1024)) as ResponsesRequest;
    } catch {
      throw new CompatibilityError("responses_json_invalid");
    }
    if (!body || typeof body !== "object" || body.stream !== true) {
      jsonError(response, 400, "streaming_responses_required");
      return;
    }
    const chatBody = responsesToChat(body);
    const upstream = await openUpstream(provider, env, chatBody, clientAbort.signal);
    if (!upstream.ok) {
      await upstream.body?.cancel();
      jsonError(response, 502, `provider_http_${upstream.status}`);
      return;
    }
    if (!upstream.body) {
      jsonError(response, 502, "provider_stream_missing");
      return;
    }
    response.writeHead(200, {
      "Content-Type": "text/event-stream; charset=utf-8",
      "Cache-Control": "no-cache, no-store",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    });
    await projectChatStream(
      upstream,
      response,
      String(chatBody.model),
      provider.stream_idle_timeout_ms ?? 300_000,
      clientAbort.signal,
      namespaceBindings(body),
      new Set((chatBody.tools as Array<{ function: { name: string } }> | undefined)?.map((tool) => tool.function.name) ?? []),
    );
  } catch (error) {
    if (response.writableEnded || response.destroyed) return;
    const code = error instanceof CompatibilityError ? error.code : "chat_compatibility_stream_failed";
    if (!response.headersSent) jsonError(response, error instanceof CompatibilityError ? error.status : 502, code);
    else response.destroy(new Error(code));
  }
}

async function openUpstream(
  provider: ProviderProfile,
  env: NodeJS.ProcessEnv,
  body: Record<string, unknown>,
  signal: AbortSignal,
): Promise<Response> {
  const url = chatCompletionsUrl(provider);
  const headers = providerHeaders(provider, env);
  const retries = provider.request_max_retries ?? 4;
  for (let attempt = 0; attempt <= retries; attempt += 1) {
    if (signal.aborted) throw signal.reason;
    const controller = new AbortController();
    const forward = () => controller.abort(signal.reason);
    signal.addEventListener("abort", forward, { once: true });
    const timer = setTimeout(() => controller.abort(new Error("provider connect timeout")), 30_000);
    try {
      const response = await fetch(url, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (![429, 500, 502, 503, 504].includes(response.status) || attempt === retries) return response;
      await response.body?.cancel();
      const retryAfter = parseRetryAfter(response.headers.get("retry-after"));
      await delay(Math.min(retryAfter ?? 250 * 2 ** attempt, 5_000), signal);
    } catch (error) {
      if (signal.aborted) throw error;
      // A transport failure does not establish that the provider rejected the
      // request. Replaying it could duplicate model work or emitted tool intent.
      throw new CompatibilityError("provider_outcome_unknown", 502);
    } finally {
      clearTimeout(timer);
      signal.removeEventListener("abort", forward);
    }
  }
  throw new CompatibilityError("provider_outcome_unknown", 502);
}

function providerHeaders(provider: ProviderProfile, env: NodeJS.ProcessEnv): Record<string, string> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Accept: "text/event-stream",
    ...provider.http_headers,
  };
  if (provider.auth.type !== "none") {
    const key = env[provider.auth.api_key_env]?.trim();
    if (!key) throw new CompatibilityError("provider_credential_missing");
    if (provider.auth.type === "bearer") headers.Authorization = `Bearer ${key}`;
    else headers[provider.auth.header] = key;
  }
  for (const [header, envName] of Object.entries(provider.env_http_headers ?? {})) {
    const value = env[envName]?.trim();
    if (!value) throw new CompatibilityError("provider_header_credential_missing");
    headers[header] = value;
  }
  return headers;
}

function chatCompletionsUrl(provider: ProviderProfile): string {
  const url = new URL(provider.base_url);
  const trimmed = url.pathname.replace(/\/$/, "");
  url.pathname = /\/chat\/completions$/i.test(trimmed)
    ? trimmed
    : `${trimmed}/chat/completions`.replace(/\/+/g, "/");
  for (const [key, value] of Object.entries(provider.query_params ?? {})) url.searchParams.set(key, value);
  return url.toString();
}

function matchesLocalToken(header: string | undefined, token: string): boolean {
  if (!header) return false;
  const expected = Buffer.from(`Bearer ${token}`);
  const actual = Buffer.from(header);
  return expected.length === actual.length && timingSafeEqual(expected, actual);
}

function jsonError(response: ServerResponse, status: number, code: string): void {
  response.writeHead(status, { "Content-Type": "application/json", "Cache-Control": "no-store" });
  response.end(JSON.stringify({ error: { code, message: code, type: status >= 500 ? "server_error" : "invalid_request_error" } }));
}

async function readBody(request: IncomingMessage, limit: number): Promise<string> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    const value = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += value.length;
    if (size > limit) throw new CompatibilityError("responses_request_too_large");
    chunks.push(value);
  }
  return Buffer.concat(chunks).toString("utf8");
}

function parseRetryAfter(value: string | null): number | undefined {
  if (!value) return undefined;
  const seconds = Number(value);
  if (Number.isFinite(seconds) && seconds >= 0) return seconds * 1000;
  const date = Date.parse(value);
  return Number.isFinite(date) ? Math.max(0, date - Date.now()) : undefined;
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(done, ms);
    const abort = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      reject(signal.reason);
    };
    function done() {
      signal.removeEventListener("abort", abort);
      resolve();
    }
    signal.addEventListener("abort", abort, { once: true });
  });
}
