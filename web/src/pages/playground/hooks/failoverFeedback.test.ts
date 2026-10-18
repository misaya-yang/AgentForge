import * as tokenObservation from "../../../features/chat/stream/tokenObservation.ts";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { parseSseEventPart } from "../../../lib/sseEventParser.ts";
import * as run from "../../../features/chat/stream/sdkRun.ts";
import * as reducer from "../../../features/chat/stream/reducer.ts";
import * as normalizers from "../../../features/chat/stream/normalizers.ts";
import * as lifecycle from "../../../features/chat/stream/sdkLifecycle.ts";
import * as langgraph from "../utils/langgraph.ts";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";
import type { ChatMessage } from "../../../components/ChatWindow.tsx";
import type { StreamChunk } from "../../../types/gateway.ts";

const noop = () => {};
const chunk = (content = "", is_final = false): StreamChunk => ({ request_id: "fixture-request", chunk_index: is_final ? 2 : 0,
  event_type: is_final ? "stream_end" : "text_delta", content: { type: "text", data: content }, is_final });
const tool = (): StreamChunk => ({ ...chunk(), event_type: "tool_call_start", tool_call: {
  tool_call_id: "side-effect", name: "send_notification", arguments: "{}", status: "running",
} });
type FixtureEvent = StreamChunk | { event: string; data: unknown };
function host(engine = "custom") {
  let messages: ChatMessage[] = [];
  let loading = false;
  let invokes = 0;
  let requests = 0;
  let historyReads = 0;
  let wire: string | null = null;
  let stream: () => AsyncGenerator<FixtureEvent> = async function* () {};
  const outcomes: string[] = [];
  const tracePayloads: Record<string, unknown>[] = [];
  const persisted: unknown[] = [];
  const ref = <T>(current: T) => ({ current });
  const auth = { token: "fixture-token", user: { user_id: "owner", permissions: [], roles: [], tier: "free" } };
  const opts = {
    serviceId: "service", activeService: { service_id: "service", metadata: { adapter_type: engine } },
    sessionEnabled: true, activeSessionId: "session", localTitles: { session: "Title" }, sessions: [], messages: [], loading: false,
    setMessages: (next: ChatMessage[] | ((current: ChatMessage[]) => ChatMessage[])) => { messages = typeof next === "function" ? next(messages) : next; },
    setLoading: (value: boolean) => { loading = value; }, setActiveSessionId: noop, setLocalTitles: noop, invalidatePendingHistoryLoad: noop,
    refreshSessions: async () => {}, messagesRef: ref(messages), abortControllerRef: ref<AbortController | null>(null),
    currentRequestIdRef: ref(0), currentRequestSessionRef: ref(null), loadingHistorySessionRef: ref(null),
    sessionThreadIdRef: ref({ session: "thread" }), pendingSessionInitRef: ref(null), pendingThreadRef: ref(null), interactionStartedRef: ref(false), stickToBottomRef: ref(false),
    t: (key: string) => key,
  };
  const wireExports: Record<string, (...args: unknown[]) => unknown> = {};
  const wireSource = readFileSync(new URL("../../../lib/sse.ts", import.meta.url), "utf8").replaceAll("import.meta.env.DEV", "false");
  const wireModules: Record<string, unknown> = {
    "@/config/runtime": { isSseDebugEnabled: () => false },
    "@/lib/api": { getAuthToken: () => "fixture-token" },
    "./sseEventParser": { parseSseEventPart },
    "./fetchClient.ts": { sendStreamRequest: async () => { requests++; return new Response(wire, { headers: { "Content-Type": "text/event-stream" } }); } },
  };
  vm.runInNewContext(ts.transpileModule(wireSource, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports: wireExports, require: (name: string) => { assert.ok(name in wireModules, name); return wireModules[name]; },
      Error, AbortController, Date, performance, Headers, TextDecoder, setTimeout, clearTimeout,
      fetch: async () => { requests++; return new Response(wire, { headers: {"Content-Type":"text/event-stream"} }); } });
  const modules: Record<string, unknown> = {
    react: { useCallback: (fn: unknown) => fn, useMemo: (fn: () => unknown) => fn(), useRef: ref },
    "@/features/chat/stream/sdkRun": run, "@/features/chat/stream/sdkLifecycle": lifecycle,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/sdkAgentConfig": { isNativeAgentSdk: (value: unknown) => value === "openai_agents" || value === "claude_agent_sdk" },
    "../utils/difyConversation": { prepareDifyConversation: async () => ({ accept: async () => {} }) },
    "@/api/gateway": { invokeService: async () => { invokes++; return { outputs: [{ data: "Automatically replayed output" }] }; } },
    "@/api/sessions": { getSession: async () => ({}), updateSession: async () => ({}),
      addSessionMessage: async (_id: string, message: unknown) => { persisted.push(message); } },
    "@/lib/sse": { sseFetch: (...args: unknown[]) => { if (wire !== null) return wireExports.sseFetch(...args); requests++; return stream(); },
      sseFetchEvents: async function* (...args: unknown[]) { if (wire !== null) { yield* wireExports.sseFetchEvents(...args) as AsyncGenerator<FixtureEvent>; return; } requests++; for await (const event of stream()) yield "event" in event ? event : { data: event }; },
      streamChunkToAGUIEvent: () => ({ event: "text_delta" }) },
    "@/hooks/useAgentTimeline": { useAgentTimeline: () => ({ state: { steps: [] }, processEvent: noop, reset: noop }) },
    "@/lib/utils": { getPlaygroundErrorMessage: () => "Safe fixture failure" },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/features/chat/stream": { ...reducer, ...normalizers },
    "@/features/chat/telemetry": { finishChatStreamTrace: (_trace: unknown, outcome: string, payload: Record<string, unknown>) => { outcomes.push(outcome); tracePayloads.push(payload); },
      markChatStreamFirstToken: noop, startChatStreamTrace: () => ({}) },
    "../utils/langgraph": langgraph,
    "@/features/chat/stream/tokenObservation": tokenObservation,
  };
  const source = readFileSync(new URL("./usePlaygroundStream.ts", import.meta.url), "utf8");
  const exports: Record<string, (value: unknown) => unknown> = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
      Error, AbortController, Date, performance, requestAnimationFrame: () => 1, cancelAnimationFrame: noop,
      fetch: async (_path: string, init: { method: string }) => { assert.equal(init.method, "GET"); historyReads++;
        return { ok: true, json: async () => [{ values: { messages: [
          { type: "human", content: "Hello" }, { type: "ai", content: "Recovered output" },
        ] } }] }; } });
  const hook = exports.usePlaygroundStream(opts) as { handleSend: (input: unknown[]) => Promise<void> };
  return { opts, outcomes, persisted, auth, tracePayloads,
    async readEvents(options: { preserveControlFrames?: boolean } = {}) {
      const events = [];
      for await (const event of wireExports.sseFetchEvents("/fixture", options) as AsyncGenerator<unknown>) events.push(event);
      return events;
    },
    setWire(value: string) { wire = value; }, setStream(value: typeof stream) { stream = value; },
    send: () => hook.handleSend([{ type: "text", data: "Hello" }]),
    get messages() { return messages; }, get loading() { return loading; }, get invokes() { return invokes; },
    get requests() { return requests; }, get historyReads() { return historyReads; } };
}

const selected = (provider_id: string, model_id: string) => ({ event: "custom", data: { type: "gateway_model_failover", status: "selected", provider_id, model_id, reason: "primary_unavailable" } });
test("selected fallback identity is retained from result, saved history and diagnostics", async () => {
  const h = host("langgraph");
  h.setStream(async function* () { yield selected("fixture-primary", "fixture-primary-model"); yield selected("fixture-fallback", "fixture-fallback-model"); yield chunk("Fallback answer"); yield { event: "end", data: {} }; });
  await h.send();
  assert.equal(h.messages.at(-1)?.status, "completed");
  assert.equal(h.requests, 1); assert.equal(h.invokes, 0);
  const result = JSON.stringify({messages: h.messages, persisted: h.persisted, traces: h.tracePayloads});
  assert.equal(h.messages.at(-1)?.stats?.reportedRouting?.model_id, "fixture-fallback-model");
  assert.match(JSON.stringify(h.persisted), /fixture-fallback-model/);
  assert.match(JSON.stringify(h.tracePayloads), /fixture-fallback-model/);
  assert.equal(h.persisted.length, 2);
  assert.ok(!result.includes("fixture-primary-model"));
});
for (const afterOutput of [false, true]) test(`transport failure ${afterOutput ? "after" : "before"} output does not replay`, async () => {
  const h = host("langgraph");
  h.setStream(async function* () { yield selected("fixture-fallback", "fixture-fallback-model"); if (afterOutput) yield tool(); throw new Error("connection reset"); });
  await h.send();
  assert.equal(h.requests, 1); assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
  assert.equal(h.messages.at(-1)?.status, "failed");
  assert.equal(h.messages.at(-1)?.stats?.reportedRouting?.model_id, "fixture-fallback-model");
  assert.match(JSON.stringify(h.tracePayloads), /fixture-fallback-model/);
  if(afterOutput) assert.equal(h.messages.at(-1)?.toolCalls?.[0]?.toolCall.tool_call_id, "side-effect");
});
test("actual SSE parser delivers selected event but successful result retains model identity", async () => {
  const h = host("langgraph");
  const event = selected("wire-fallback-provider", "wire-fallback-model");
  h.setWire(`event: custom\ndata: ${JSON.stringify(event.data)}\n\nevent: messages/complete\ndata: ${JSON.stringify([{type: "ai", id:"reply", content:"Wire fallback answer"}])}\n\nevent: end\n\n`);
  await h.send();
  assert.equal(h.messages.at(-1)?.status, "completed");
  assert.equal(h.messages.at(-1)?.content, "Wire fallback answer");
  assert.equal(h.requests, 1); assert.equal(h.invokes, 0);
  assert.equal(h.messages.at(-1)?.stats?.reportedRouting?.provider_id, "wire-fallback-provider");
  assert.match(JSON.stringify(h.persisted), /wire-fallback-model/);
  assert.match(JSON.stringify(h.tracePayloads), /wire-fallback-model/);
});
for (const toolOnly of [false, true]) test(`reported routing survives confirmed ${toolOnly ? "tool-only" : "empty"} end`, async () => {
 const h=host("langgraph"); h.setStream(async function*(){ yield selected("provider-empty","model-empty"); if(toolOnly) yield tool(); yield {event:"end",data:{}}; }); await h.send();
 assert.equal(h.messages.at(-1)?.status,"completed"); assert.equal(h.messages.at(-1)?.stats?.reportedRouting?.model_id,"model-empty");
 assert.match(JSON.stringify(h.tracePayloads),/model-empty/); assert.equal(h.requests,1); assert.equal(h.invokes,0);
 // A confirmed empty terminal with a reported selection remains a useful saved observation.
 assert.equal(h.persisted.length,2);
 assert.match(JSON.stringify(h.persisted),/model-empty/);
});
test("invalid later selection clears older reported identity rather than claiming it remains selected", async () => {
 const h=host("langgraph"); h.setStream(async function*(){yield selected("old-provider","old-model");yield selected("https://bad.test/?token=secret","sk-secret");yield chunk("Answer");yield {event:"end",data:{}};}); await h.send();
 assert.equal(h.messages.at(-1)?.stats?.reportedRouting,undefined);
 assert.ok(!JSON.stringify([h.messages,h.persisted,h.tracePayloads]).includes("old-model"));
 assert.ok(!JSON.stringify([h.messages,h.persisted,h.tracePayloads]).includes("secret"));
});

test("secret-shaped selected payload cannot reach live, history or telemetry", async () => {
 const h=host("langgraph"); h.setStream(async function*(){yield selected("provider","vendor/sk-fakefixture");yield selected("ghp_FAKEFIXTURE12345","model");yield chunk("Answer");yield {event:"end",data:{}};});await h.send();
 assert.equal(h.messages.at(-1)?.status,"completed");assert.equal(h.messages.at(-1)?.stats?.reportedRouting,undefined);
 assert.doesNotMatch(JSON.stringify([h.messages,h.persisted,h.tracePayloads]), /sk-fakefixture|ghp_FAKEFIXTURE/);
});
test("new send without selection cannot inherit prior reported routing", async () => {
 const h=host("langgraph");h.setStream(async function*(){yield selected("first-provider","first-model");yield chunk("First");yield {event:"end",data:{}};});await h.send();
 h.setStream(async function*(){yield chunk("Second");yield {event:"end",data:{}};});await h.send();
 assert.equal(h.messages.at(-1)?.stats?.reportedRouting,undefined);
 assert.ok(!JSON.stringify(h.persisted.at(-1)).includes("first-model"));
 assert.ok(!JSON.stringify(h.tracePayloads.at(-1)).includes("first-model"));
});
test("account change fences late selected routing from live answer and saved history", async () => {
 const h=host("langgraph");h.setStream(async function*(){yield selected("old-provider","old-model");h.auth.token="replacement-fixture-token";yield selected("late-provider","late-model");yield chunk("Late answer");yield {event:"end",data:{}};});await h.send();
 assert.ok(!JSON.stringify([h.messages,h.persisted]).includes("late-model"));
 assert.equal(h.persisted.length,1);assert.equal(h.requests,1);
});


const langchainToolCases: Array<{name: string; status: string; messages: Array<Record<string, unknown>>}> = JSON.parse(readFileSync(new URL("../../../../../tests/fixtures/adapters/langchain_tool_roundtrip.json", import.meta.url), "utf8")).cases;
for (const fixtureCase of langchainToolCases) {
  for (const mode of ["messages", "updates"]) {
    test(`LangChain actual hook preserves ${fixtureCase.name} through ${mode} and manual history`, async () => {
      const h = host("langgraph");
      const events = fixtureCase.messages.map(message => mode === "updates" && message.type === "tool"
        ? {event: "updates", data: {tools: {messages: [message]}}}
        : {event: "messages", data: [message, {}]});
      h.setWire(events.map(event => `event: ${event.event}\ndata: ${JSON.stringify(event.data)}\n\n`).join("") + "event: end\ndata: {}\n\n");
      await h.send();
      const expected = fixtureCase.status === "error" ? "error" : "completed";
      const result = fixtureCase.messages.find(message => message.type === "tool")!.content;
      assert.equal(h.messages.at(-1)?.status, "completed");
      assert.equal(h.messages.at(-1)?.toolCalls?.[0]?.toolCall.status, expected);
      assert.equal(h.messages.at(-1)?.toolCalls?.[0]?.result, result);
      const saved = h.persisted.at(-1) as {role: string; content: string; metadata: {tool_calls: Array<{status: string; result: unknown}>}};
      assert.equal(saved.role, "assistant");
      assert.equal(saved.metadata.tool_calls[0].status, expected);
      assert.equal(saved.metadata.tool_calls[0].result, result);
      if (fixtureCase.name.startsWith("tool_only") || fixtureCase.name.startsWith("result_only")) assert.equal(saved.content, "");
      assert.equal(h.requests, 1);
      assert.equal(h.invokes, 0);
    });
  }
}
