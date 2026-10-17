import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { test, type TestContext } from "node:test";

import {
  ASSISTANT_VIEW_DETACH_REASON,
  type AgentV2Event,
} from "../features/chat/runtimeV2State.ts";

const apiUrl = `data:text/javascript,${encodeURIComponent("export const api = { post: async () => { throw new Error('unconfigured fixture'); } }; export const getAuthToken = () => null;")}`;
const apiModule = new URL("./agentThreads.ts", import.meta.url).href;
const transportModule = new URL("../lib/sse.ts", import.meta.url).href;
const hooks = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (context.parentURL === apiModule || context.parentURL === transportModule) {
      if (specifier === "@/lib/api") return { url: apiUrl, shortCircuit: true };
      if (specifier === "@/config/runtime") return {
        url: "data:text/javascript,export const isSseDebugEnabled = () => false;", shortCircuit: true,
      };
      const paths: Record<string, string> = {
        "@/lib/sse": "../lib/sse.ts",
        "@/features/chat/runtimeV2State": "../features/chat/runtimeV2State.ts",
        "./agentTurnPayload": "./agentTurnPayload.ts",
        "./sseEventParser": "../lib/sseEventParser.ts",
      };
      if (paths[specifier]) return nextResolve(new URL(paths[specifier], apiModule).href, context);
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    const loaded = nextLoad(url, context);
    return url === transportModule ? { ...loaded, source: `import.meta.env = { DEV: false };\n${loaded.source}` } : loaded;
  },
});
const { api } = await import(apiUrl);
const { getAgentRuntimeV2RunSnapshot, streamAgentRuntimeV2 } = await import("./agentThreads.ts");
hooks.deregister();

function browserTimers(t: TestContext) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { setTimeout, clearTimeout } });
  t.after(() => {
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  });
}

function event(sequence: number, type: string, data: unknown = {}, turnId = "turn-1"): AgentV2Event {
  return {
    schema_version: "agent-event/v2", thread_id: "thread-1", sequence,
    timestamp: "2026-09-22T12:00:00Z",
    event: { id: `event-${sequence}`, key: `event-${sequence}`, type, turn_id: turnId, item_id: null, status: null, payload: { event_type: type, data } },
  };
}

function response(events: AgentV2Event[], open = false) {
  let cancelled = 0;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(events.map(item => `data: ${JSON.stringify(item)}\n\n`).join("")));
      if (!open) controller.close();
    },
    cancel() { cancelled++; },
  });
  return { response: new Response(body), cancelled: () => cancelled };
}

function startFixture(t: TestContext) {
  browserTimers(t);
  const calls: string[] = [];
  t.mock.method(api, "post", async (path: string) => {
    calls.push(path);
    if (path === "/api/v2/agent/threads") return { data: { thread: { thread_id: "thread-1", session_id: "session-1" } } };
    if (path.endsWith("/turns")) return { data: { turn: { id: "turn-1", events_url: "/api/v2/agent/threads/thread-1/events?after_sequence=0&turn_id=turn-1" } } };
    if (path.endsWith(":interrupt")) return { data: {} };
    throw new Error(`unexpected fixture request: ${path}`);
  });
  return calls;
}

test("turn stream reconnects after 100 events, deduplicates cursor and reaches terminal after 150", { timeout: 5_000 }, async t => {
  const calls = startFixture(t);
  const first = response(Array.from({ length: 100 }, (_, index) => event(index + 1, "text_delta", { content: `${index + 1},` })));
  const second = response([
    event(100, "text_delta", { content: "duplicate" }),
    ...Array.from({ length: 50 }, (_, index) => event(index + 101, "text_delta", { content: `${index + 101},` })),
    event(151, "run_finished", { run_id: "turn-1" }),
  ], true);
  const requested: URL[] = [];
  t.mock.method(globalThis, "fetch", async (url: string) => {
    requested.push(new URL(url, "http://fixture"));
    assert.ok(requested.length <= 2, "terminal must stop reconnecting");
    return requested.length === 1 ? first.response : second.response;
  });
  const projected = [];
  for await (const item of streamAgentRuntimeV2({ message: "hello", session_id: "session-1" }, t.signal)) projected.push(item);
  assert.deepEqual(requested.map(url => url.searchParams.get("after_sequence")), ["0", "100"]);
  assert.deepEqual(requested.map(url => url.searchParams.get("turn_id")), ["turn-1", "turn-1"]);
  assert.equal(projected.filter(item => item.event_type === "text_delta").map(item => item.data).join(""), Array.from({ length: 150 }, (_, index) => `${index + 1},`).join(""));
  assert.equal(projected.filter(item => item.event_type === "run_finished").length, 1);
  assert.equal(second.cancelled(), 1);
  assert.equal(calls.filter(path => path.endsWith("/turns")).length, 1);
  assert.equal(calls.filter(path => path.endsWith(":interrupt")).length, 0);
});

test("durable snapshot preserves approval rejection across duplicate replay and closes at terminal", async t => {
  browserTimers(t);
  const stream = response([
    event(1, "approval_required", { approval_id: "approval-1", tool_name: "write", tool_call_id: "call-1" }),
    event(2, "approval_result", { approval_id: "approval-1", approved: false }),
    event(2, "approval_required", { approval_id: "stale-approval", tool_name: "write" }),
    event(3, "run_error", { status: "failed" }),
  ], true);
  t.mock.method(globalThis, "fetch", async (url: string) => {
    const query = new URL(url, "http://fixture").searchParams;
    assert.equal(query.get("turn_id"), "turn-1");
    return stream.response;
  });
  const snapshot = await getAgentRuntimeV2RunSnapshot("thread-1", "turn-1");
  assert.equal(snapshot.lastSequence, 3);
  assert.equal(snapshot.rejectedApproval, true);
  assert.equal(snapshot.pendingApproval, undefined);
  assert.equal(snapshot.terminalStatus, "failed");
  assert.equal(stream.cancelled(), 1);
});

for (const explicitAbort of [false, true]) {
  test(`pending approval ${explicitAbort ? "Stop interrupts exactly once" : "consumer detachment keeps the durable approval"}`, async t => {
    const calls = startFixture(t);
    const incoming = response([event(1, "approval_required", { approval_id: "approval-1", tool_name: "write" })], true);
    t.mock.method(globalThis, "fetch", async () => incoming.response);
    const caller = new AbortController();
    const stream = streamAgentRuntimeV2({ message: "write" }, caller.signal);
    assert.equal((await stream.next()).value?.event_type, "approval_required");
    if (explicitAbort) caller.abort();
    await stream.return();
    assert.equal(incoming.cancelled(), 1);
    assert.equal(calls.filter(path => path.endsWith(":interrupt")).length, explicitAbort ? 1 : 0);
  });
}

test("detaching the assistant view does not interrupt an admitted turn", async t => {
  const calls = startFixture(t);
  const incoming = response([event(1, "text_delta", { content: "partial" })], true);
  t.mock.method(globalThis, "fetch", async () => incoming.response);
  const caller = new AbortController();
  const stream = streamAgentRuntimeV2({ message: "long task" }, caller.signal);
  assert.equal((await stream.next()).value?.event_type, "text_delta");
  caller.abort(ASSISTANT_VIEW_DETACH_REASON);
  await stream.return();
  assert.equal(calls.filter(path => path.endsWith("/turns")).length, 1);
  assert.equal(calls.filter(path => path.endsWith(":interrupt")).length, 0);
});

test("a permanent 409 stream error is surfaced without retrying the admitted turn", async t => {
  const calls = startFixture(t);
  let fetches = 0;
  t.mock.method(globalThis, "fetch", async () => {
    fetches++;
    return new Response('{"detail":"Resume the existing turn"}', { status: 409 });
  });
  await assert.rejects(streamAgentRuntimeV2({ message: "hello" }).next(), /409.*Resume the existing turn/);
  assert.equal(fetches, 1);
  assert.equal(calls.filter(path => path.endsWith("/turns")).length, 1);
  assert.equal(calls.filter(path => path.endsWith(":interrupt")).length, 1);
});


test("selected provider reaches both real thread and turn requests without changing model ID", async t => {
  browserTimers(t);
  const sent: Array<{ path: string; body: Record<string, unknown> }> = [];
  t.mock.method(api, "post", async (path: string, body: Record<string, unknown>) => {
    sent.push({ path, body });
    if (path === "/api/v2/agent/threads") return { data: { thread: { thread_id: "thread-1", session_id: "session-1" } } };
    if (path.endsWith("/turns")) return { data: { turn: { id: "turn-1", events_url: "/api/v2/agent/threads/thread-1/events?after_sequence=0&turn_id=turn-1" } } };
    throw new Error(`unexpected request: ${path}`);
  });
  t.mock.method(globalThis, "fetch", async () => response([event(1, "run_finished", { run_id: "turn-1" })]).response);
  for await (const item of streamAgentRuntimeV2({ message: "hello", session_id: "session-1", model_id: "same", provider_id: "second" })) { assert.ok(item.event_type); }
  assert.equal(sent.length, 2);
  assert.deepEqual(sent[0].body, { session_id: "session-1", model_id: "same", provider_id: "second" });
  assert.equal(sent[1].body.model_id, "same");
  assert.equal(sent[1].body.provider_id, "second");
});

test("network flap after delivered tokens resumes the cursor, reports state and never starts a second turn", { timeout: 5_000 }, async t => {
  const calls = startFixture(t);
  t.mock.method(Math, "random", () => 0);
  const first = response([event(1, "run_started"), event(2, "text_delta", { content: "A" })]);
  const final = response([event(2, "text_delta", { content: "duplicate" }), event(3, "tool_call_result", { tool_call_id: "effect-1", result: "recorded" }), event(4, "text_delta", { content: "B" }), event(5, "run_finished")], true);
  const cursors: string[] = []; const states: string[] = [];
  t.mock.method(globalThis, "fetch", async (url: string) => {
    cursors.push(new URL(url, "http://fixture").searchParams.get("after_sequence")!);
    if (cursors.length === 2) throw new TypeError("network disconnected");
    assert.ok(cursors.length <= 3); return cursors.length === 1 ? first.response : final.response;
  });
  const received = [];
  for await (const item of streamAgentRuntimeV2({ message: "hello", session_id: "session-1" }, t.signal, { onConnectionState: state => states.push(state) })) received.push(item);
  assert.deepEqual(cursors, ["0", "2", "2"]);
  assert.equal(received.filter(item => item.event_type === "text_delta").map(item => item.data).join(""), "AB");
  assert.equal(received.filter(item => item.event_type === "run_finished").length, 1);
  assert.equal(received.filter(item => item.event_type === "tool_call_result").length, 1);
  assert.ok(states.includes("reconnecting")); assert.equal(states.at(-1), "server_terminal");
  assert.equal(calls.filter(path => path.endsWith("/turns")).length, 1);
  assert.equal(calls.filter(path => path.endsWith(":interrupt")).length, 0);
});
