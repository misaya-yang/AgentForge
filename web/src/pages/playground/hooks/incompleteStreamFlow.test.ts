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


for (const engine of ["custom", "dify_chat", "langgraph"]) {
  test(`${engine}: premature EOF preserves partial answer and fails without replay`, async () => {
    const h = host(engine); h.setStream(async function* () { yield chunk("Partial answer"); }); await h.send();
    const message = h.messages.at(-1)!;
    assert.equal(message.status, "failed"); assert.match(message.content, /Partial answer/);
    assert.match(message.content, /playground.errors.incompleteStream/); assert.equal(message.isStreaming, false);
    assert.equal(h.loading, false); assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
    assert.deepEqual(h.outcomes, ["failed"]); assert.equal(h.requests, 1);
  });
  test(`${engine}: zero-frame EOF is failed and never invokes a second request`, async () => {
    const h = host(engine); await h.send(); assert.equal(h.messages.at(-1)?.status, "failed");
    assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0); assert.deepEqual(h.outcomes, ["failed"]);
  });
  test(`${engine}: early transport failure is failed without uncertain automatic replay`, async () => {
    const h = host(engine);
    // eslint-disable-next-line require-yield
    h.setStream(async function* () { throw new Error("lost admission response"); }); await h.send();
    assert.equal(h.messages.at(-1)?.status, "failed"); assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
    assert.deepEqual(h.outcomes, ["failed"]);
  });
  test(`${engine}: partial transport failure preserves visible content`, async () => {
    const h = host(engine); h.setStream(async function* () { yield chunk("Partial answer"); throw new Error("connection reset"); });
    await h.send(); assert.equal(h.messages.at(-1)?.status, "failed"); assert.match(h.messages.at(-1)!.content, /Partial answer/);
    assert.match(h.messages.at(-1)!.content, /Safe fixture failure/); assert.equal(h.invokes, 0);
  });
  test(`${engine}: tool-side-effect EOF keeps the tool and cannot recover stale history`, async () => {
    const h = host(engine); h.setStream(async function* () { yield tool(); }); await h.send();
    assert.equal(h.messages.at(-1)?.status, "failed"); assert.equal(h.messages.at(-1)?.toolCalls?.[0]?.toolCall.tool_call_id, "side-effect");
    assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0); assert.deepEqual(h.outcomes, ["failed"]);
  });
  test(`${engine}: valid terminal completes with unknown usage and separate text estimates`, async () => {
    const h = host(engine); h.setStream(async function* () { yield chunk("Answer"); yield chunk("", true); }); await h.send();
    assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.messages.at(-1)?.content, "Answer");
    assert.equal(h.messages.at(-1)?.stats?.totalTokens, undefined);
    assert.equal(h.messages.at(-1)?.stats?.tokenUsageSource, "unknown");
    assert.equal(typeof h.messages.at(-1)?.stats?.textTokenEstimate?.totalTokens, "number"); assert.equal(h.invokes, 0);
    assert.deepEqual(h.outcomes, ["completed"]);
  });
  test(`${engine}: explicit error terminal cannot become fallback success`, async () => {
    const h = host(engine); h.setStream(async function* () { yield tool(); yield { ...chunk("provider error", true), event_type: "error" }; });
    await h.send(); assert.equal(h.messages.at(-1)?.status, "failed"); assert.equal(h.invokes, 0);
    assert.equal(h.historyReads, 0); assert.deepEqual(h.outcomes, ["failed"]);
  });
}
test("LangGraph named end event remains authoritative", async () => {
  const h = host("langgraph"); h.setStream(async function* () { yield chunk("Answer"); yield { event: "end", data: {} }; });
  await h.send(); assert.equal(h.messages.at(-1)?.status, "completed"); assert.deepEqual(h.outcomes, ["completed"]);
});
test("LangGraph successful tool-only end needs no historical answer or replay", async () => {
  const h = host("langgraph"); h.setStream(async function* () { yield tool(); yield { event: "end", data: {} }; });
  await h.send(); assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.messages.at(-1)?.content, "");
      assert.equal(h.messages.at(-1)?.completionConfirmed, true);
  assert.equal(h.historyReads, 0); assert.equal(h.requests, 1); assert.equal(h.invokes, 0);
});
test("ordinary successful tool-only terminal is not automatically invoked again", async () => {
  const h = host(); h.setStream(async function* () { yield tool(); yield chunk("", true); }); await h.send();
  assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.invokes, 0); assert.equal(h.requests, 1);
});

for (const locale of ["en-US", "zh-CN"]) {
  test(`${locale}: incomplete-stream diagnostic is available in the actual Playground locale`, () => {
    const dictionary = JSON.parse(readFileSync(new URL(`../../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
    assert.equal(typeof dictionary.playground.errors.incompleteStream, "string");
    assert.ok(dictionary.playground.errors.incompleteStream.length > 20);
  });
}

for (const engine of ["custom", "dify_chat", "langgraph"]) {
  test(`${engine}: normalized legacy terminal without is_final remains completed`, async () => {
    const h = host(engine); h.setStream(async function* () {
      yield chunk("Answer"); yield { ...chunk("", true), is_final: false };
    }); await h.send(); assert.equal(h.messages.at(-1)?.status, "completed");
    assert.deepEqual(h.outcomes, ["completed"]); assert.equal(h.invokes, 0);
  });
}

const partialWire = 'event: messages/partial\ndata: [{"type":"AIMessageChunk","content":"Partial answer"}]\n\n';
const completeWire = 'event: messages/complete\ndata: [{"type":"ai","content":"Partial answer"}]\n\n';
const terminalCases = [
 ["partial EOF", partialWire, "failed"],
 ["JSON null end", partialWire + "event: end\ndata: null\n\n", "completed"],
 ["bare end", partialWire + "event: end\n\n", "completed"],
 ["DONE", partialWire + "data: [DONE]\n\n", "completed"],
 ["message complete EOF", partialWire + completeWire, "failed"],
 ["message complete then error", partialWire + completeWire + 'event: error\ndata: {"error":"fixture"}\n\n', "failed"],
 ["subgraph error then end", partialWire + 'event: error|worker:fixture\ndata: {"error":"fixture"}\n\nevent: end\ndata: null\n\n', "failed"],
];
for (const [name, wire, outcome] of terminalCases) test(`review real SSE transport: ${name}`, async () => {
 const h = host("langgraph"); h.setWire(wire); await h.send();
 assert.equal(h.messages.at(-1)?.status, outcome); assert.equal(h.requests, 1); assert.equal(h.invokes, 0);
});
test("review real SSE transport: named custom payload cannot supply a root terminal", async () => {
 const h = host("langgraph"); h.setWire(partialWire + 'event: custom\ndata: {"event":"end","data":{}}\n\n'); await h.send();
 assert.equal(h.messages.at(-1)?.status, "failed");
});
test("review real SSE transport: named custom error payload is only domain data", async () => {
 const h = host("langgraph"); h.setWire(partialWire + 'event: custom\ndata: {"event":"error","data":{"message":"domain data"}}\n\nevent: end\ndata: null\n\n'); await h.send();
 assert.equal(h.messages.at(-1)?.status, "completed");
});

for (const engine of ["custom", "dify_chat", "langgraph"]) {
  test(`${engine}: confirmed zero-output final stays successful without substitution`, async () => {
    const h = host(engine); h.setWire(`data: ${JSON.stringify(chunk("", true))}\n\n`); await h.send();
    assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.messages.at(-1)?.content, "");
      assert.equal(h.messages.at(-1)?.completionConfirmed, true);
    assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0); assert.deepEqual(h.outcomes, ["completed"]);
  });
}
for (const terminal of ["event: end", "data: [DONE]"]) {
  test(`LangGraph pending final buffer retains ${terminal}`, async () => {
    const h = host("langgraph"); h.setWire(partialWire + terminal); await h.send();
    assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.requests, 1);
  });
}
for (const event of ["error", "error|worker:fixture"]) {
  test(`LangGraph empty ${event} frame cannot be hidden by end`, async () => {
    const h = host("langgraph"); h.setWire(`${partialWire}event: ${event}\n\nevent: end\n\n`); await h.send();
    assert.equal(h.messages.at(-1)?.status, "failed"); assert.match(h.messages.at(-1)!.content, /Partial answer/);
    assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
  });
}
test("LangGraph explicit custom envelope stays data even if its nested value resembles a control frame", async () => {
  const h = host("langgraph"); h.setWire(partialWire + 'event: custom\ndata: {"event":"error","data":{"error":"domain-value"}}\n\nevent: end\n\n');
  await h.send(); assert.equal(h.messages.at(-1)?.status, "completed");
});
test("LangGraph subgraph end alone cannot finalize the root run", async () => {
  const h = host("langgraph"); h.setWire(partialWire + 'event: end|worker:fixture\n\n');
  await h.send(); assert.equal(h.messages.at(-1)?.status, "failed");
});
test("LangGraph message completion does not hide the next message before run end", async () => {
  const h = host("langgraph"); h.setWire(partialWire + completeWire + 'event: messages/partial\ndata: [{"type":"AIMessageChunk","content":"; final answer"}]\n\nevent: end\n\n');
  await h.send(); assert.equal(h.messages.at(-1)?.status, "completed");
  assert.equal(h.messages.at(-1)?.content, "Partial answer; final answer");
});
const adapterFixtures = JSON.parse(readFileSync(new URL("../../../../../tests/fixtures/adapters/langgraph_stream_terminal.json", import.meta.url), "utf8")) as {
  name: string; wire: string; error: boolean; text: string;
}[];
for (const fixture of adapterFixtures) {
  test(`LangGraph shared adapter wire fixture: ${fixture.name}`, async () => {
    const h = host("langgraph"); h.setWire(fixture.wire); await h.send();
    // Direct Playground transport requires a completion signal even though the
    // server adapter's legacy normal_eof fixture permits EOF normalization.
    const failed = fixture.error || fixture.name === "normal_eof";
    assert.equal(h.messages.at(-1)?.status, failed ? "failed" : "completed");
    if (fixture.text) assert.ok(h.messages.at(-1)!.content.includes(fixture.text));
    assert.ok(!h.messages.at(-1)!.content.includes("SYNTHETIC_PRIVATE_PROVIDER_DETAIL"));
    assert.equal(h.requests, 1); assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
    assert.deepEqual(h.outcomes, [failed ? "failed" : "completed"]);
  });
}

test("production named SSE transport preserves existing default filtering for other consumers", async () => {
  const h = host("langgraph"); h.setWire('event: end\n\ndata: [DONE]\n\nid: resume-cursor\nevent: progress\ndata: {"value":7}\n\n');
  assert.deepEqual(await h.readEvents(), [{ id: "resume-cursor", event: "progress", data: { value: 7 } }]);
});
test("production named SSE transport opts in to raw controls without rewriting data events", async () => {
  const h = host("langgraph"); h.setWire('event: end\n\ndata: [DONE]\n\nid: resume-cursor\nevent: progress\ndata: {"value":7}\n\n');
  assert.deepEqual(await h.readEvents({ preserveControlFrames: true }), [
    { event: "end", data: undefined }, { event: "", data: "[DONE]" },
    { id: "resume-cursor", event: "progress", data: { value: 7 } },
  ]);
});
test("Dify normalized empty final without is_final is still authoritative", async () => {
  const h = host("dify_chat"); h.setWire(`data: ${JSON.stringify({ ...chunk("", true), is_final: false })}\n\n`);
  await h.send(); assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.messages.at(-1)?.content, "");
      assert.equal(h.messages.at(-1)?.completionConfirmed, true);
  assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
});

for (const marker of ["event: end\n\n", "data: [DONE]\n\n"]) {
  for (const prefix of ["", 'event: messages/partial\ndata: [{"type":"AIMessageChunk","tool_call_chunks":[{"id":"side-effect","name":"send_notification","args":"{}","index":0}]}]\n\n']) {
    test(`native LangGraph ${prefix ? "tool-only" : "empty"} ${marker.trim()} completes without text or unbound history`, async () => {
      const h = host("langgraph"); h.setWire(prefix + marker); await h.send();
      assert.equal(h.messages.at(-1)?.status, "completed"); assert.equal(h.messages.at(-1)?.content, "");
      assert.equal(h.messages.at(-1)?.completionConfirmed, true);
      assert.equal(h.messages.at(-1)?.toolCalls?.length, prefix ? 1 : 0);
      assert.equal(h.requests, 1); assert.equal(h.invokes, 0); assert.equal(h.historyReads, 0);
      assert.deepEqual(h.outcomes, ["completed"]);
    });
  }
}
test("native LangGraph tool-only EOF remains incomplete even when thread history contains an answer", async () => {
  const h = host("langgraph"); h.setWire('event: messages/partial\ndata: [{"type":"AIMessageChunk","tool_call_chunks":[{"id":"side-effect","name":"send_notification","args":"{}","index":0}]}]\n\n');
  await h.send(); assert.equal(h.messages.at(-1)?.status, "failed");
  assert.equal(h.messages.at(-1)?.toolCalls?.length, 1); assert.equal(h.historyReads, 0); assert.equal(h.invokes, 0);
});

for (const engine of ["custom", "dify_chat", "langgraph"]) {
  for (const [name, usage, expected] of [
    ["missing", undefined, [undefined, undefined, undefined, "unknown"]],
    ["explicit zero", { input_tokens: 0, output_tokens: 0, total_tokens: 0 }, [0, 0, 0, "reported"]],
    ["reported", { input_tokens: 17, output_tokens: 9, total_tokens: 26 }, [17, 9, 26, "reported"]],
    ["partial input", { input_tokens: 17 }, [17, undefined, undefined, "reported"]],
    ["partial zero output", { output_tokens: 0 }, [undefined, 0, undefined, "reported"]],
    ["explicit total only", { total_tokens: 80 }, [undefined, undefined, 80, "reported"]],
    ["provider aliases", { prompt_tokens: 13, completion_tokens: 7, total_tokens: 20 }, [13, 7, 20, "reported"]],
    ["legacy decimal strings", { input_tokens: "0", output_tokens: "12", total_tokens: "12" }, [0, 12, 12, "reported"]],
    ["invalid counts", { input_tokens: -1, output_tokens: "invalid", total_tokens: 0.5 }, [undefined, undefined, undefined, "unknown"]],
  ] as const) {
    test(`PG02 ${engine}: ${name} remains distinct from local text estimates`, async () => {
      const h = host(engine); h.setStream(async function* () {
        yield chunk("Answer"); yield { ...chunk("", true), ...(usage ? { metadata: { usage } } : {}) };
      }); await h.send(); const message = h.messages.at(-1)!; const stats = message.stats!;
      assert.equal(message.status, "completed");
      assert.deepEqual([stats.inputTokens, stats.outputTokens, stats.totalTokens, stats.tokenUsageSource], [...expected]);
      assert.equal(typeof stats.textTokenEstimate?.totalTokens, "number");
      assert.equal(h.requests, 1); assert.equal(h.invokes, 0);
      if (engine === "langgraph") {
        const stored = h.persisted.find(value => (value as { role?: string }).role === "assistant") as {
          metadata: { stats: Record<string, unknown> };
        };
        const metadata = stored.metadata.stats;
        assert.equal(metadata.token_usage_source, expected[3]);
        assert.equal(metadata.total_tokens, expected[2]);
        assert.equal(typeof (metadata.text_token_estimate as { total_tokens: number }).total_tokens, "number");
      }
    });
  }
  test(`PG02 ${engine}: timing-only terminal cannot erase earlier reported zero`, async () => {
    const h = host(engine); h.setStream(async function* () {
      yield { ...chunk("Answer"), metadata: { usage: { input_tokens: 0, output_tokens: 7, total_tokens: 7 } } };
      yield { ...chunk("", true), metadata: { usage: { duration_ms: 42 } } };
    }); await h.send(); const stats = h.messages.at(-1)!.stats!;
    assert.deepEqual([stats.inputTokens, stats.outputTokens, stats.totalTokens], [0, 7, 7]);
  });
  test(`PG02 ${engine}: failed partial stream keeps received usage with honest provenance`, async () => {
    const h = host(engine); h.setStream(async function* () {
      yield { ...chunk("Partial"), metadata: { usage: { input_tokens: 0, output_tokens: 7 } } };
      throw new Error("transport failure");
    }); await h.send(); const message = h.messages.at(-1)!;
    assert.equal(message.status, "failed"); assert.equal(message.stats?.inputTokens, 0);
    assert.equal(message.stats?.outputTokens, 7); assert.equal(message.stats?.totalTokens, undefined);
    assert.equal(message.stats?.tokenUsageSource, "reported"); assert.equal(h.invokes, 0);
  });
}
test("PG02 LangGraph raw metadata usage and message usage use production observation", async () => {
  for (const wire of [
    'event: metadata\ndata: {"usage":{"input_tokens":0,"output_tokens":2,"total_tokens":2}}\n\n',
    'event: messages/partial\ndata: [{"type":"AIMessageChunk","content":"Answer","usage_metadata":{"input_tokens":0,"output_tokens":2,"total_tokens":2}}]\n\n',
    'event: messages/complete\ndata: [{"type":"ai","content":"Answer","usage_metadata":{"input_tokens":0,"output_tokens":2,"total_tokens":2}}]\n\n',
  ]) {
    const h = host("langgraph"); h.setWire(wire + 'event: end\n\n'); await h.send();
    assert.equal(h.messages.at(-1)?.status, "completed");
    assert.equal(h.messages.at(-1)?.stats?.inputTokens, 0);
    assert.equal(h.messages.at(-1)?.stats?.totalTokens, 2);
    assert.equal(h.messages.at(-1)?.stats?.tokenUsageSource, "reported");
  }
});

test("PG02 received Gateway annotation preserves mixed numeric fields and persisted per-field sources", async () => {
  const h = host("langgraph"); h.setStream(async function* () {
    yield chunk("Answer"); yield { ...chunk("", true), metadata: { usage: { input_tokens: 0, output_tokens: 12, total_tokens: 12,
      token_provenance: { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" } } } };
  }); await h.send(); const stats = h.messages.at(-1)!.stats!;
  assert.deepEqual([stats.inputTokens, stats.outputTokens, stats.totalTokens], [0, 12, 12]);
  assert.deepEqual(stats.tokenProvenance, { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" });
  const stored = h.persisted.at(-1) as { metadata: { stats: { token_provenance: unknown } } };
  assert.deepEqual(stored.metadata.stats.token_provenance, stats.tokenProvenance);
  assert.equal(h.tracePayloads.at(-1)?.tokenUsageSource, "reported");
  assert.deepEqual(h.tracePayloads.at(-1)?.tokenProvenance, stats.tokenProvenance);
  assert.equal(h.tracePayloads.at(-1)?.textTokenEstimate, undefined);
});

for (const content of ["Answer", ""]) {
  test(`PG02 LangGraph usage-only update after duplicate ${content ? "text" : "empty text"} keeps reported counts`, async () => {
    const h = host("langgraph");
    h.setWire('event: messages/complete\ndata: [{"id":"m1","type":"ai","content":"Answer"}]\n\n' +
      'event: updates\ndata: ' + JSON.stringify({ agent: { messages: [{ id: "m1", type: "ai", content,
        usage_metadata: { input_tokens: 0, output_tokens: 7, total_tokens: 7 } }] } }) + '\n\nevent: end\n\n');
    await h.send(); const stats = h.messages.at(-1)!.stats!;
    assert.equal(stats.inputTokens, 0); assert.equal(stats.outputTokens, 7); assert.equal(stats.totalTokens, 7);
    assert.equal(stats.tokenUsageSource, "reported"); assert.equal(h.messages.at(-1)?.content, "Answer");
  });
}
