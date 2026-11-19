#!/usr/bin/env node
// Public-API Main Assistant acceptance runner. Dry-run by default; --execute
// makes real model and read-only web_search calls through the local Gateway.
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { homedir } from "node:os";
import { join } from "node:path";

const DEFAULT_ORIGIN = "http://127.0.0.1:8080";
const DEFAULT_CREDENTIALS = join(homedir(), ".cache/agentforge/secrets/local-test-admin.json");
const REQUEST_TIMEOUT_MS = 240_000;
const RUNNING_TIMEOUT_MS = 30_000;
const TERMINAL_TIMEOUT_MS = 90_000;
const TERMINAL = new Set(["completed", "succeeded", "failed", "cancelled", "blocked"]);

class RunnerFailure extends Error {
  constructor(details) {
    super("runner failure");
    this.name = "RunnerFailure";
    this.details = details;
  }
}

function argumentsOf(argv) {
  const result = new Set(argv);
  for (const arg of result) {
    if (arg !== "--execute" && arg !== "--help") {
      throw new RunnerFailure({ kind: "usage", code: "unknown_argument" });
    }
  }
  return { execute: result.has("--execute"), help: result.has("--help") };
}

function usage() {
  return [
    "Usage: node scripts/harness/main_assistant_e2e.mjs [--execute]",
    "",
    "Without --execute, print the planned public-API flow and make no requests.",
    "With --execute, make fresh synthetic sessions and real provider calls:",
    "two-turn text memory, built-in read-only web_search, active cancellation,",
    "same-session continuation, and public run/history/detail verification.",
    "",
    "Optional environment: AI_PLATFORM_GATEWAY_ORIGIN,",
    "AI_PLATFORM_ADMIN_CREDENTIALS, AI_PLATFORM_E2E_MODEL_ID.",
  ].join("\n");
}

function assert(condition, code) {
  if (!condition) throw new RunnerFailure({ kind: "assertion", code });
}

function safeFailure(error) {
  if (error instanceof RunnerFailure) return error.details;
  if (error?.name === "AbortError") {
    return { kind: "timeout", code: "request_timeout" };
  }
  return { kind: "runner", code: "unexpected_error" };
}

function safeErrorCode(bodyText) {
  try {
    const parsed = JSON.parse(bodyText);
    const code = parsed?.detail?.code ?? parsed?.code;
    return typeof code === "string" && /^[A-Z0-9_.-]{1,100}$/.test(code)
      ? code
      : undefined;
  } catch {
    return undefined;
  }
}

function urlFor(origin, route) {
  return new URL(route, origin).toString();
}

function safeRequestId(response, fallback) {
  const candidate = response.headers.get("x-request-id");
  return typeof candidate === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(candidate)
    ? candidate
    : fallback;
}

async function requestJson(
  origin,
  method,
  route,
  token,
  body,
  timeoutMs = 30_000,
  requestId = randomUUID(),
) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const headers = { accept: "application/json", "x-request-id": requestId };
  if (body !== undefined) headers["content-type"] = "application/json";
  if (token) headers.authorization = `Bearer ${token}`;
  try {
    let response;
    try {
      response = await fetch(urlFor(origin, route), {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: controller.signal,
      });
    } catch (error) {
      throw new RunnerFailure({
        kind: error?.name === "AbortError" ? "timeout" : "network",
        code: error?.name === "AbortError" ? "request_timeout" : "request_failed",
        request_id: requestId,
      });
    }
    const bodyText = await response.text();
    let data;
    try {
      data = bodyText.length ? JSON.parse(bodyText) : {};
    } catch {
      const gatewayRequestId = safeRequestId(response, requestId);
      throw new RunnerFailure({
        kind: "http",
        code: "response_not_json",
        http_status: response.status,
        request_id: gatewayRequestId,
      });
    }
    if (!response.ok) {
      const code = safeErrorCode(bodyText);
      const gatewayRequestId = safeRequestId(response, requestId);
      throw new RunnerFailure({
        kind: "http",
        http_status: response.status,
        ...(code ? { code } : {}),
        request_id: gatewayRequestId,
      });
    }
    return {
      status: response.status,
      data,
      requestId: safeRequestId(response, requestId),
    };
  } finally {
    clearTimeout(timer);
  }
}

async function requestReady(origin) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 10_000);
  try {
    const response = await fetch(urlFor(origin, "/health/ready"), {
      method: "GET",
      headers: { accept: "application/json, text/plain" },
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new RunnerFailure({
        kind: "http",
        code: "readiness_failed",
        http_status: response.status,
      });
    }
    // Read and discard the small body so the connection can be reused. Health
    // payload shape is deployment-owned and is not part of this test contract.
    await response.arrayBuffer();
    return response.status;
  } finally {
    clearTimeout(timer);
  }
}

function parseSseLine(frame, line) {
  if (line.startsWith("event:")) frame.name = line.slice(6).trim();
  if (line.startsWith("data:")) frame.data.push(line.slice(5).replace(/^ /, ""));
}

function summarizeSseFrame(frame) {
  if (!frame.data.length) return undefined;
  let value;
  try {
    value = JSON.parse(frame.data.join("\n"));
  } catch {
    return { type: frame.name || "unparsed" };
  }
  const data = value?.data ?? value?.event?.payload?.data ?? {};
  const type = value?.event_type ?? value?.event?.type ?? value?.type ?? frame.name ?? "unknown";
  const summary = { type };
  for (const key of ["tool_name", "status", "success"]) {
    if (typeof data?.[key] === "string" || typeof data?.[key] === "boolean") {
      summary[key] = data[key];
    }
  }
  return summary;
}

async function readSse(body) {
  assert(body && typeof body.getReader === "function", "stream_body_missing");
  const reader = body.getReader();
  const decoder = new TextDecoder();
  const events = [];
  let buffered = "";
  let frame = { name: "", data: [] };
  function dispatch() {
    const parsed = summarizeSseFrame(frame);
    if (parsed) events.push(parsed);
    frame = { name: "", data: [] };
  }
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffered += decoder.decode(value, { stream: true });
    for (;;) {
      const newline = buffered.indexOf("\n");
      if (newline < 0) break;
      const line = buffered.slice(0, newline).replace(/\r$/, "");
      buffered = buffered.slice(newline + 1);
      if (!line) dispatch();
      else if (!line.startsWith(":")) parseSseLine(frame, line);
    }
  }
  buffered += decoder.decode();
  if (buffered) parseSseLine(frame, buffered.replace(/\r$/, ""));
  dispatch();
  return events;
}

async function streamTurn(origin, token, sessionId, message, options = {}, onHeaders, label = "stream_turn") {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const requestId = randomUUID();
  let controlPromise;
  try {
    const body = {
      message,
      session_id: sessionId,
      ...options,
    };
    let response;
    try {
      response = await fetch(urlFor(origin, "/api/v1/assistant/chat/stream"), {
        method: "POST",
        headers: {
          accept: "text/event-stream",
          "content-type": "application/json",
          authorization: `Bearer ${token}`,
          "x-request-id": requestId,
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
    } catch (error) {
      throw new RunnerFailure({
        kind: error?.name === "AbortError" ? "timeout" : "network",
        code: error?.name === "AbortError" ? "request_timeout" : "request_failed",
        request_id: requestId,
      });
    }
    if (!response.ok) {
      const bodyText = await response.text();
      const code = safeErrorCode(bodyText);
      const gatewayRequestId = safeRequestId(response, requestId);
      throw new RunnerFailure({
        kind: "http",
        http_status: response.status,
        ...(code ? { code } : {}),
        request_id: gatewayRequestId,
      });
    }
    const runId = response.headers.get("x-run-id");
    const responseSession = response.headers.get("x-session-id");
    assert(runId, "stream_run_id_missing");
    assert(responseSession === sessionId, "stream_session_mismatch");
    const effectiveRequestId = safeRequestId(response, requestId);
    process.stderr.write(
      `${JSON.stringify({ event: "stream_started", label, session_id: responseSession, run_id: runId, request_id: effectiveRequestId })}\n`,
    );
    if (onHeaders) {
      controlPromise = Promise.resolve()
        .then(() => onHeaders({
          runId,
          sessionId: responseSession,
          requestId: effectiveRequestId,
        }))
        .catch((error) => {
          controller.abort();
          throw error;
        });
    }
    let events;
    try {
      events = await readSse(response.body);
    } catch (streamError) {
      if (controlPromise) {
        try {
          await controlPromise;
        } catch (controlError) {
          throw controlError;
        }
      }
      throw streamError;
    }
    const control = controlPromise ? await controlPromise : undefined;
    return {
      runId,
      sessionId: responseSession,
      requestId: effectiveRequestId,
      label,
      events,
      control,
    };
  } finally {
    clearTimeout(timer);
  }
}

async function runDetail(origin, token, runId) {
  const result = await requestJson(
    origin,
    "GET",
    `/api/v1/assistant/runs/${encodeURIComponent(runId)}`,
    token,
  );
  assert(result.data?.run?.run_id === runId, "run_detail_mismatch");
  return result.data.run;
}

async function pause(ms) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForRunning(origin, token, runId) {
  const deadline = Date.now() + RUNNING_TIMEOUT_MS;
  let lastStatus = "unavailable";
  while (Date.now() < deadline) {
    const run = await runDetail(origin, token, runId);
    lastStatus = String(run.status ?? "unknown");
    if (lastStatus === "running") return run;
    if (TERMINAL.has(lastStatus)) {
      throw new RunnerFailure({ kind: "assertion", code: "run_terminal_before_cancel" });
    }
    if (lastStatus === "awaiting_approval") {
      throw new RunnerFailure({ kind: "assertion", code: "cancellation_reached_approval" });
    }
    await pause(150);
  }
  throw new RunnerFailure({ kind: "assertion", code: "run_never_became_active" });
}

async function waitForTerminal(origin, token, runId, expected) {
  const expectedStatuses = new Set(Array.isArray(expected) ? expected : [expected]);
  const deadline = Date.now() + TERMINAL_TIMEOUT_MS;
  let lastStatus = "unknown";
  while (Date.now() < deadline) {
    const run = await runDetail(origin, token, runId);
    lastStatus = String(run.status ?? "unknown");
    if (TERMINAL.has(lastStatus)) {
      assert(expectedStatuses.has(lastStatus), "run_terminal_status_mismatch");
      return run;
    }
    await pause(250);
  }
  throw new RunnerFailure({ kind: "assertion", code: "run_terminal_timeout" });
}

async function chatTurn(origin, token, sessionId, message, options = {}, label = "text_turn") {
  const requestId = randomUUID();
  process.stderr.write(
    `${JSON.stringify({ event: "turn_start", label, session_id: sessionId, request_id: requestId })}\n`,
  );
  const response = await requestJson(
    origin,
    "POST",
    "/api/v1/assistant/chat",
    token,
    { message, session_id: sessionId, ...options },
    REQUEST_TIMEOUT_MS,
    requestId,
  );
  assert(response.data?.session_id === sessionId, "chat_session_mismatch");
  const runId = response.data?.run_id;
  assert(typeof runId === "string" && runId.length > 0, "chat_run_id_missing");
  const run = await runDetail(origin, token, runId);
  assert(["completed", "succeeded"].includes(run.status), "text_turn_not_successful");
  process.stderr.write(
    `${JSON.stringify({ event: "turn_complete", label, session_id: sessionId, run_id: runId, request_id: response.requestId, status: run.status })}\n`,
  );
  return {
    runId,
    requestId: response.requestId,
    content: typeof response.data.content === "string" ? response.data.content : "",
    status: run.status,
  };
}

async function publicHistory(origin, token, sessionId) {
  const [detailResult, historyResult] = await Promise.all([
    requestJson(origin, "GET", `/api/v1/assistant/sessions/${encodeURIComponent(sessionId)}`, token),
    requestJson(
      origin,
      "GET",
      `/api/v1/assistant/sessions/${encodeURIComponent(sessionId)}/history?limit=100`,
      token,
    ),
  ]);
  const detail = detailResult.data;
  const history = historyResult.data;
  assert(detail.session_id === sessionId, "session_detail_mismatch");
  assert(history.session_id === sessionId, "session_history_mismatch");
  assert(Array.isArray(history.messages), "session_history_messages_missing");
  return {
    messageCount: Number.isInteger(detail.message_count) ? detail.message_count : null,
    historyTotal: Number.isInteger(history.total) ? history.total : history.messages.length,
    historyRows: history.messages.length,
  };
}

async function credentials(path) {
  let value;
  try {
    value = JSON.parse(await readFile(path, "utf8"));
  } catch {
    throw new RunnerFailure({ kind: "configuration", code: "credentials_file_unavailable" });
  }
  assert(
    typeof value.email === "string" && typeof value.password === "string",
    "credentials_fields_missing",
  );
  return { email: value.email, password: value.password };
}

function originFromEnvironment() {
  const raw = process.env.AI_PLATFORM_GATEWAY_ORIGIN || DEFAULT_ORIGIN;
  const origin = new URL(raw);
  assert(["http:", "https:"].includes(origin.protocol), "gateway_origin_scheme_invalid");
  assert(!origin.username && !origin.password, "gateway_origin_contains_credentials");
  assert(origin.pathname === "/" && !origin.search && !origin.hash, "gateway_origin_not_origin_only");
  return origin.origin;
}

function emitEvent(event) {
  process.stderr.write(`${JSON.stringify(event)}\n`);
}

async function stage(summary, name, operation) {
  summary.stages[name] = { status: "running" };
  emitEvent({ event: "stage_started", stage: name });
  const startedAt = Date.now();
  try {
    await operation();
    summary.stages[name] = { status: "passed", duration_ms: Date.now() - startedAt };
    emitEvent({ event: "stage_result", stage: name, status: "passed" });
    return true;
  } catch (error) {
    const details = safeFailure(error);
    summary.stages[name] = {
      status: "failed",
      duration_ms: Date.now() - startedAt,
      error: details,
    };
    emitEvent({ event: "stage_result", stage: name, status: "failed", error: details });
    return false;
  }
}

function skipStage(summary, name, prerequisite) {
  summary.stages[name] = { status: "skipped", prerequisite };
  emitEvent({ event: "stage_result", stage: name, status: "skipped", prerequisite });
}

function finish(summary) {
  const entries = Object.entries(summary.stages);
  const passed = entries.length > 0 && entries.every(([, result]) => result.status === "passed");
  summary.status = passed ? "passed" : "failed";
  summary.failed_stages = entries
    .filter(([, result]) => result.status === "failed")
    .map(([name]) => name);
  summary.incomplete_stages = entries
    .filter(([, result]) => result.status === "skipped")
    .map(([name]) => name);
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
  if (!passed) process.exitCode = 1;
}

async function main() {
  const args = argumentsOf(process.argv.slice(2));
  if (args.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }
  const origin = originFromEnvironment();
  if (!args.execute) {
    process.stdout.write(
      `${JSON.stringify({
        mode: "dry_run",
        origin,
        calls_provider: false,
        steps: [
          "health readiness preflight",
          "fresh local-admin authentication",
          "two same-session text turns with a memory marker",
          "read-only built-in web_search with start/result event evidence",
          "wait for a genuinely running turn, send one cancel, verify cancelled status",
          "continue in the cancelled session and verify terminal success/history",
        ],
      }, null, 2)}\n`,
    );
    return;
  }
  assert(typeof fetch === "function", "fetch_unavailable");

  const summary = {
    mode: "execute",
    origin,
    calls_provider: true,
    tool: "web_search (read-only)",
    text_session_id: randomUUID(),
    cancellation_session_id: randomUUID(),
    stages: {},
  };
  emitEvent({
    event: "acceptance_start",
    text_session_id: summary.text_session_id,
    cancellation_session_id: summary.cancellation_session_id,
  });

  const healthOk = await stage(summary, "health_ready", async () => {
    summary.health_http_status = await requestReady(origin);
  });
  if (!healthOk) {
    for (const name of [
      "authentication",
      "text_memory",
      "web_search",
      "cancellation",
      "post_cancel_recovery",
      "text_session_history",
      "cancellation_session_history",
    ]) skipStage(summary, name, "health_ready_failed");
    finish(summary);
    return;
  }

  let token;
  const authenticationOk = await stage(summary, "authentication", async () => {
    const credentialsPath = process.env.AI_PLATFORM_ADMIN_CREDENTIALS || DEFAULT_CREDENTIALS;
    const login = await requestJson(
      origin,
      "POST",
      "/api/v1/auth/login",
      undefined,
      await credentials(credentialsPath),
    );
    token = login.data?.access_token ?? login.data?.body?.access_token;
    assert(typeof token === "string" && token.length > 0, "login_access_token_missing");
  });
  if (!authenticationOk) {
    for (const name of [
      "text_memory",
      "web_search",
      "cancellation",
      "post_cancel_recovery",
      "text_session_history",
      "cancellation_session_history",
    ]) skipStage(summary, name, "authentication_failed");
    finish(summary);
    return;
  }

  const modelId = process.env.AI_PLATFORM_E2E_MODEL_ID;
  const stableCapabilities = {
    web_search_enabled: true,
    web_search_max_results: 3,
    ...(modelId ? { model_id: modelId } : {}),
  };
  const marker = `TURN-MEMORY-${randomUUID().slice(0, 8).toUpperCase()}`;
  await stage(summary, "text_memory", async () => {
    summary.text_turns = [];
    const first = await chatTurn(
      origin,
      token,
      summary.text_session_id,
      `Remember this synthetic marker for this session: ${marker}. Reply only ACK.`,
      stableCapabilities,
      "text_turn_1",
    );
    summary.text_turns.push({ run_id: first.runId, request_id: first.requestId, status: first.status });

    const second = await chatTurn(
      origin,
      token,
      summary.text_session_id,
      "What exact marker did I give you? Reply with only the marker.",
      stableCapabilities,
      "text_turn_2",
    );
    const markerRecalled = second.content.includes(marker);
    summary.text_turns.push({
      run_id: second.runId,
      request_id: second.requestId,
      status: second.status,
      marker_recalled: markerRecalled,
    });
    assert(markerRecalled, "memory_marker_not_recalled");
    summary.memory_marker_recalled = true;
  });

  await stage(summary, "web_search", async () => {
    summary.tool_turn = { status: "starting" };
    const toolStream = await streamTurn(
      origin,
      token,
      summary.text_session_id,
      "Use the web_search tool to find the latest Node.js release information from the official nodejs.org site. Report the version and release date from the result; do not answer from memory.",
      stableCapabilities,
      async ({ runId, sessionId, requestId }) => {
        summary.tool_turn = {
          run_id: runId,
          session_id: sessionId,
          request_id: requestId,
          status: "streaming",
        };
      },
      "read_only_web_search",
    );
    const toolStart = toolStream.events.some(
      (event) => event.type === "tool_call_start" && event.tool_name === "web_search",
    );
    const toolResult = toolStream.events.some(
      (event) =>
        event.type === "tool_call_result" &&
        event.tool_name === "web_search" &&
        event.success === true &&
        event.status === "completed",
    );
    summary.tool_turn = {
      run_id: toolStream.runId,
      request_id: toolStream.requestId,
      stream_event_count: toolStream.events.length,
      start_observed: toolStart,
      successful_result_observed: toolResult,
      status: "events_received",
    };
    assert(toolStart && toolResult, "web_search_start_or_successful_result_missing");
    const toolRun = await waitForTerminal(origin, token, toolStream.runId, ["completed", "succeeded"]);
    summary.tool_turn.status = toolRun.status;
  });

  const cancellationPrompt =
    "Write a detailed 6000-word field guide with numbered sections, concrete examples, and a glossary. Begin immediately and continue until the guide is complete.";
  await stage(summary, "cancellation", async () => {
    summary.cancellation = { status: "starting" };
    const cancelStream = await streamTurn(
      origin,
      token,
      summary.cancellation_session_id,
      cancellationPrompt,
      { max_tokens: 8192, web_search_enabled: false },
      async ({ runId, sessionId, requestId }) => {
        summary.cancellation = {
          run_id: runId,
          session_id: sessionId,
          request_id: requestId,
          status: "streaming",
        };
        const active = await waitForRunning(origin, token, runId);
        summary.cancellation.active_status = active.status;
        const cancelRequestId = randomUUID();
        emitEvent({ event: "cancel_active_run", run_id: runId, request_id: cancelRequestId });
        const cancelResponse = await requestJson(
          origin,
          "POST",
          `/api/v1/assistant/tasks/${encodeURIComponent(runId)}/cancel`,
          token,
          { reason: "main_assistant_e2e_cancel" },
          30_000,
          cancelRequestId,
        );
        const cancelAcknowledged = cancelResponse.data?.cancelled === true;
        summary.cancellation.cancel_acknowledged = cancelAcknowledged;
        summary.cancellation.cancel_request_id = cancelResponse.requestId;
        assert(cancelAcknowledged, "cancel_active_run_not_acknowledged");
        const terminal = await waitForTerminal(origin, token, runId, "cancelled");
        summary.cancellation.terminal_status = terminal.status;
        summary.cancellation.status = "terminal_verified";
      },
      "cancellation_turn",
    );
    summary.cancellation.stream_event_count = cancelStream.events.length;
  });

  const cancellationVerified =
    summary.cancellation?.cancel_acknowledged === true &&
    summary.cancellation?.terminal_status === "cancelled";
  if (cancellationVerified) {
    await stage(summary, "post_cancel_recovery", async () => {
      const recovery = await chatTurn(
        origin,
        token,
        summary.cancellation_session_id,
        "Continue after the cancelled turn. Reply with exactly RECOVERED.",
        { web_search_enabled: false },
        "post_cancel_recovery",
      );
      const markerSeen = recovery.content.includes("RECOVERED");
      summary.recovery_turn = {
        run_id: recovery.runId,
        request_id: recovery.requestId,
        status: recovery.status,
        marker_seen: markerSeen,
      };
      assert(markerSeen, "post_cancel_recovery_marker_missing");
    });
  } else {
    skipStage(summary, "post_cancel_recovery", "cancelled_terminal_not_verified");
  }

  if (Array.isArray(summary.text_turns) && summary.text_turns.length > 0) {
    await stage(summary, "text_session_history", async () => {
      summary.text_session = await publicHistory(origin, token, summary.text_session_id);
      assert(summary.text_session.historyTotal > 0, "text_session_history_empty");
    });
  } else {
    skipStage(summary, "text_session_history", "no_text_turn_was_recorded");
  }

  if (summary.cancellation?.run_id) {
    await stage(summary, "cancellation_session_history", async () => {
      summary.cancellation_session = await publicHistory(
        origin,
        token,
        summary.cancellation_session_id,
      );
      assert(summary.cancellation_session.historyTotal > 0, "cancellation_session_history_empty");
    });
  } else {
    skipStage(summary, "cancellation_session_history", "cancellation_run_not_recorded");
  }

  finish(summary);
}

main().catch((error) => {
  const details = safeFailure(error);
  const summary = {
    mode: "startup",
    status: "failed",
    stages: { startup: { status: "failed", error: details } },
    failed_stages: ["startup"],
    incomplete_stages: [],
  };
  emitEvent({ event: "stage_result", stage: "startup", status: "failed", error: details });
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
  process.exitCode = 1;
});
