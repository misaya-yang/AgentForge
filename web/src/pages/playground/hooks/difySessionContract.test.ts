import * as requestSnapshot from "../utils/requestSnapshot.ts";
import * as tokenObservation from "../../../features/chat/stream/tokenObservation.ts";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as run from "../../../features/chat/stream/sdkRun.ts";
import * as reducer from "../../../features/chat/stream/reducer.ts";
import * as normalizers from "../../../features/chat/stream/normalizers.ts";
import * as lifecycle from "../../../features/chat/stream/sdkLifecycle.ts";
import * as langgraph from "../utils/langgraph.ts";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";
import type { ChatMessage } from "../../../components/ChatWindow.tsx";
import type { StreamChunk } from "../../../types/gateway.ts";

const noop = () => {};
const isSdk = (value: unknown) => value === "openai_agents" || value === "claude_agent_sdk";
const chunk = (content: string, final = false): StreamChunk => ({
  request_id: "request", chunk_index: final ? 1 : 0, event_type: final ? "stream_end" : "text_delta",
  is_final: final, content: { type: "text", data: content },
});
function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function host(engine = "dify_chat", response: Record<string, unknown>, patchFailure = false) {
  const slots: unknown[] = [];
  let cursor = 0;
  let phase = "sessions";
  const streamSlots: unknown[] = [];
  let streamCursor = 0;
  const auth = { token: "fixture-token", user: { user_id: "owner", roles: [], permissions: [], tier: "free" } };
  const store = { activeSessionId: "session" as string | undefined, playgroundSidebarOpen: true,
    setActiveSessionId(value: string | undefined) { store.activeSessionId = value; }, setLocalTitles: noop };
  const services = ["service", "other-service"].map(service_id => ({ service_id,
    ...(engine === "langgraph" ? { service_type: "langgraph" } : {}), metadata: { adapter_type: engine } }));
  const creationErrors: unknown[] = [];
  const options = { serviceId: "service", services, onSessionCreationError: (error: unknown) => creationErrors.push(error) };
  let stream: () => AsyncGenerator<StreamChunk> = async function* () { yield chunk("Answer"); yield chunk("", true); };
  let create: () => Promise<{ session_id: string }> = async () => ({ session_id: "new-session" });
  let histories: unknown[] = [];
  let history: () => Promise<unknown[]> = async () => histories;
  let fetcher: (url: string) => Promise<unknown> = async () => { throw new Error("Unexpected network operation"); };
  const updates: { sessionId: string; body: unknown }[] = [];
  let streamCalls = 0;
  let createCalls = 0;
  const requests: Record<string, unknown>[] = [];
  const captures: unknown[] = [];
  let captureSideEffect = () => {};
  const streamPaths: string[] = [];
  let list: () => Promise<unknown[]> = async () => [];
  let titleReads = 0;
  let titleWrites = 0;
  let syncCalls = 0;
  let patch: () => Promise<void> = async () => {};
  let detailRead: () => Promise<void> = async () => {};
  let productionErrors = false;
  const frames = new Map<number, () => void>();
  let frameId = 0;
  let holdNextMessageUpdate = false;
  let holdNextSessionsUpdate = false;
  const heldUpdates: (() => void)[] = [];
  type Effect = { run: () => void | (() => void); deps: unknown[] };
  let effectCursor = 0;
  const effects: Effect[] = [];
  const installedEffects: { deps: unknown[]; cleanup?: () => void }[] = [];
  const react = {
    useState(initial: unknown) { const index = cursor++; if (!(index in slots)) slots[index] = initial;
      return [slots[index], (value: unknown) => {
        const apply = () => { slots[index] = typeof value === "function" ? value(slots[index]) : value; };
        if (index === 0 && holdNextMessageUpdate) { holdNextMessageUpdate = false; heldUpdates.push(apply); }
        else if (index === 3 && holdNextSessionsUpdate) { holdNextSessionsUpdate = false; heldUpdates.push(apply); }
        else apply();
      }]; },
    useRef(initial: unknown) { const target = phase === "sessions" ? slots : streamSlots;
      const index = phase === "sessions" ? cursor++ : streamCursor++;
      return target[index] ?? (target[index] = { current: initial }); },
    useEffect: (run: Effect["run"], deps: unknown[]) => { effects[effectCursor++] = { run, deps }; },
    useCallback(value: unknown, deps: unknown[]) {
      const target = phase === "sessions" ? slots : streamSlots;
      const index = phase === "sessions" ? cursor++ : streamCursor++;
      const previous = target[index] as { value: unknown; deps: unknown[] } | undefined;
      if (previous && previous.deps.length === deps.length && previous.deps.every((entry, i) => Object.is(entry, deps[i]))) return previous.value;
      target[index] = { value, deps }; return value;
    },
    useMemo: (value: () => unknown) => value(),
  };
  const modules: Record<string, unknown> = {
    react, "@/features/chat/stream/sdkRun": run, "@/features/chat/stream/sdkLifecycle": lifecycle,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/sdkAgentConfig": { isNativeAgentSdk: isSdk },
    "../utils/difyConversation": { prepareDifyConversation: async () => ({ accept: async () => {} }) },
    "@/api/gateway": { invokeService: async () => { syncCalls++; return { outputs: [{ data: "Fallback" }] }; } },
    "@/api/sessions": {
      createSession: () => { createCalls++; return create(); }, deleteSession: async () => {}, listSessions: () => list(),
      getSessionHistory: () => history(), getSession: async () => { titleReads++; return { metadata: { title: "Server title" } }; },
      updateSession: async (sessionId: string, body: unknown) => { titleWrites++; updates.push({ sessionId, body }); return {}; }, addSessionMessage: async () => ({}),
    },
    "@/lib/sse": { sseFetch: (_url: string, init: { body: string }) => { streamCalls++; requests.push(JSON.parse(init.body)); return stream(); },
      sseFetchEvents: async function* (url: string) { streamCalls++; streamPaths.push(url); for await (const data of stream()) yield { data }; },
      streamChunkToAGUIEvent: () => ({ event: "text_delta" }) },
    "@/hooks/useAgentTimeline": { useAgentTimeline: () => ({ state: { steps: [] }, processEvent: noop, reset: noop }) },
    "@/lib/utils": { getPlaygroundErrorMessage: (error: unknown, t: unknown) => productionErrors ? utils.getPlaygroundErrorMessage(error, t) : "Fixture failure" },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/store/useAppStore": { useAppStore: Object.assign(() => store, { getState: () => ({ ...store,
      selectedServiceId: options.serviceId, setSelectedServiceId: (id: string) => { options.serviceId = id; } }) }) },
    "@/features/chat/stream": { ...reducer, ...normalizers },
    "@/features/chat/telemetry": { finishChatStreamTrace: noop, markChatStreamFirstToken: noop,
      startChatStreamTrace: () => ({}), trackChatHistoryRestored: noop, trackChatHistoryEmptyState: noop },
    "../utils/requestSnapshot": requestSnapshot,
    "../utils/langgraph": langgraph,
    "@/features/chat/stream/tokenObservation": tokenObservation,
  };
  function load(filename: string) {
    const source = readFileSync(new URL(filename, import.meta.url), "utf8");
    const exports: Record<string, (value: unknown) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
      { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
        AbortController, Error, DOMException, Date, performance, URLSearchParams,
        window: { location: { search: "", pathname: "/playground" }, history: { replaceState: noop } },
        setTimeout: () => 1, fetch: (url: string) => fetcher(url),
        requestAnimationFrame: (fn: () => void) => { frames.set(++frameId, fn); return frameId; },
        cancelAnimationFrame: (id: number) => frames.delete(id) });
    return exports;
  }
  const utilityExports: Record<string, (...args: unknown[]) => unknown> = {};
  const utilities = readFileSync(new URL("../../../lib/utils.ts", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(utilities, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports: utilityExports, Error, require: (name: string) => { if (name === "clsx") return { clsx: noop }; if (name === "@/lib/fetchClient") return { HttpError: class HttpError extends Error {} }; throw new Error(name); } });
  const utils = utilityExports;
  const api = {
    async get(url: string) {
      assert.equal(url, `/api/v1/sessions/${response.session_id}`);
      await detailRead();
      return { data: JSON.parse(JSON.stringify(response)) };
    },
    async patch(url: string, body: unknown) {
      assert.equal(url, `/api/v1/sessions/${response.session_id}`);
      updates.push({ sessionId: String(response.session_id), body });
      const metadata = (body as { metadata?: Record<string, unknown> }).metadata;
      if (metadata?.dify_conversation) await patch();
      if (patchFailure) throw new Error("metadata write failed");
      return { data: { status: "updated" } };
    },
  };
  modules["@/lib/api"] = { api };
  const transportSessions = load("../../../api/sessions.ts");
  modules["@/api/sessions"] = { ...(modules["@/api/sessions"] as object),
    getSession: transportSessions.getSession, updateSession: transportSessions.updateSession };
  modules["../utils/difyConversation"] = load("../utils/difyConversation.ts");
  store.activeSessionId = String(response.session_id);
  const sessionsModule = load("./usePlaygroundSessions.ts");
  const streamModule = load("./usePlaygroundStream.ts");
  type Sessions = {
    messages: ChatMessage[]; loading: boolean; sessions: { session_id: string }[]; sessionsLoading: boolean;
    refreshSessions: (silent?: boolean) => Promise<void>;
    setMessages: (value: ChatMessage[]) => void; setLoading: (value: boolean) => void;
    handleStopStreaming: () => void; handleSelectSession: (id: string) => Promise<void>;
    handleNewSession: () => Promise<unknown>; handleDeleteSession: (id: string) => Promise<void>;
    sessionThreadIdRef: { current: Record<string, string> };
    pendingSessionInitRef: { current: Promise<string | null> | null };
    pendingThreadRef: { current: Promise<string | null> | null };
    abortControllerRef: { current: AbortController | null };
  };
  function sessions() { phase = "sessions"; cursor = 0; effectCursor = 0; return sessionsModule.usePlaygroundSessions(options) as Sessions; }
  function streamHook() {
    const session = sessions();
    session.sessionThreadIdRef.current = { session: "thread", "other-session": "other-thread" };
    phase = "stream"; streamCursor = 0;
    return streamModule.usePlaygroundStream({ ...session,
      onRequestSnapshot: (capture: unknown) => { captures.push(capture); captureSideEffect(); },
      isSelectionCurrent: (serviceId: string, sessionId: string | undefined) => options.serviceId === serviceId && store.activeSessionId === sessionId,
      serviceId: options.serviceId, activeService: services.find(value => value.service_id === options.serviceId),
      activeSessionId: store.activeSessionId, setActiveSessionId: store.setActiveSessionId,
      localTitles: { session: "Existing title", "other-session": "Other title" }, setLocalTitles: noop,
      stickToBottomRef: { current: false }, t: (value: string) => value,
    }) as { handleSend: (input: unknown[], fields?: Record<string, unknown>) => Promise<void> };
  }
  return {
    auth, options, store, sessions, creationErrors, captures, services,
    send: (fields?: Record<string, unknown>) => streamHook().handleSend([{ type: "text", data: "Keep this input" }], fields),
    rerender: streamHook,
    setStream(value: typeof stream) { stream = value; }, setCreate(value: typeof create) { create = value; },
    setHistory(value: unknown[]) { histories = value; },
    setHistoryReader(value: typeof history) { history = value; },
    setFetch(value: typeof fetcher) { fetcher = value; }, get updates() { return updates; },
    setList(value: typeof list) { list = value; },
    setPatch(value: typeof patch) { patch = value; },
    setDetailRead(value: typeof detailRead) { detailRead = value; },
    useProductionErrors() { productionErrors = true; },
    setCaptureSideEffect(value: typeof captureSideEffect) { captureSideEffect = value; },
    get createCalls() { return createCalls; }, get requests() { return requests; }, get streamPaths() { return streamPaths; },
    flushFrames() { const pending = [...frames.values()]; frames.clear(); for (const fn of pending) fn(); },
    holdNextMessageUpdate() { holdNextMessageUpdate = true; },
    holdNextSessionsUpdate() { holdNextSessionsUpdate = true; },
    flushHeldUpdates() { for (const apply of heldUpdates.splice(0)) apply(); },
    async commitSessionEffects() {
      sessions();
      for (const [index, effect] of [...effects].entries()) {
        const previous = installedEffects[index];
        if (previous && previous.deps.length === effect.deps.length &&
            previous.deps.every((value, i) => Object.is(value, effect.deps[i]))) continue;
        previous?.cleanup?.();
        const cleanup = effect.run();
        installedEffects[index] = { deps: effect.deps, cleanup: typeof cleanup === "function" ? cleanup : undefined };
      }
      await Promise.resolve(); await Promise.resolve();
    },
    unmount() { for (const effect of installedEffects) effect.cleanup?.(); },
    get streamCalls() { return streamCalls; }, get titleReads() { return titleReads; },
    get titleWrites() { return titleWrites; }, get syncCalls() { return syncCalls; },
  };
}


// Exact JSON emitted by the production Python POST/GET route functions.
// Only the transport boundary is controlled; session API, Dify preparation,
// and Playground stream/session hooks are the production implementations.
const fixtures = JSON.parse(readFileSync(
  process.env.DIFY_SESSION_RESPONSES || new URL("../../../../../tests/fixtures/adapters/dify_session_detail.json", import.meta.url), "utf8"));
for (const key of ["fresh", "resumed", "wrong_binding"]) {
  test(`Python route → production Dify ${key} session dispatch`, async () => {
    const h = host("dify_chat", fixtures[key]);
    await h.send();
    assert.equal(h.streamCalls, 1);
    assert.equal(h.syncCalls, 0);
    assert.equal(h.requests[0].session_id, fixtures[key].session_id);
    assert.equal((h.requests[0].parameters as Record<string, unknown> | undefined)?.conversation_id,
      key === "resumed" ? "saved-conversation" : undefined);
    assert.equal(h.sessions().messages.at(-1)?.status, "completed");
  });
}
for (const key of ["mismatch", "null", "missing"]) {
  test(`Dify rejects ${key} service identity before provider dispatch`, async () => {
    const response = key === "missing" ? { ...fixtures.fresh } : fixtures[key];
    if (key === "missing") delete response.service_id;
    const h = host("dify_chat", response); await h.send();
    assert.equal(h.streamCalls, 0); assert.equal(h.syncCalls, 0);
    assert.equal(h.sessions().messages.at(-1)?.status, "failed");
    assert.equal(h.updates.filter(update => JSON.stringify(update.body).includes("dify_conversation")).length, 0);
  });
}
test("metadata identity is persisted before interrupted stream finishes, without replay", async () => {
  const h = host("dify_chat", fixtures.fresh);
  const reached = deferred<void>(); const finish = deferred<void>();
  h.setStream(async function* () {
    yield { ...chunk("Partial"), metadata: { dify_conversation_id: "new-conversation" } };
    reached.resolve(); await finish.promise; throw new Error("stream interrupted");
  });
  const sending = h.send();
  // Avoid hanging if a regression rejects preparation before stream entry.
  await Promise.race([reached.promise, sending]);
  try {
    assert.equal(h.streamCalls, 1);
    assert.equal(h.updates.filter(update => JSON.stringify(update.body).includes("dify_conversation")).length, 1);
    assert.equal(JSON.stringify(h.updates.find(update => JSON.stringify(update.body).includes("dify_conversation"))!.body), JSON.stringify({ metadata: {
      dify_conversation: { service_id: "service", conversation_id: "new-conversation" },
    } }));
  } finally { finish.resolve(); await sending; }
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  assert.equal(h.sessions().messages.at(-1)?.status, "failed");
});
test("metadata persistence failure is visible and never invokes provider again", async () => {
  const h = host("dify_chat", fixtures.fresh, true);
  h.setStream(async function* () {
    yield { ...chunk("Partial"), metadata: { dify_conversation_id: "new-conversation" } };
    yield chunk("", true);
  });
  await h.send();
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  assert.equal(h.updates.filter(update => JSON.stringify(update.body).includes("dify_conversation")).length, 1);
  assert.equal(h.sessions().messages.at(-1)?.status, "failed");
  assert.equal(h.sessions().messages.at(-1)?.content, "Partial\n\nFixture failure");
});
test("repeated saved metadata does not rewrite the existing conversation binding", async () => {
  const h = host("dify_chat", fixtures.resumed);
  h.setStream(async function* () {
    yield { ...chunk("Answer"), metadata: { dify_conversation_id: "saved-conversation" } };
    yield chunk("", true);
  });
  await h.send();
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0); assert.equal(h.updates.filter(update => JSON.stringify(update.body).includes("dify_conversation")).length, 0);
  assert.equal(h.sessions().messages.at(-1)?.status, "completed");
});


for (const [name, metadata, expected, content] of [
  ["completed", { gateway_stream_status: "completed" }, "completed", "answer"],
  ["failed partial", { gateway_stream_status: "failed" }, "failed", "partial answer"],
  ["interrupted partial", { gateway_stream_status: "interrupted" }, "cancelled", "partial answer"],
  ["legacy", {}, "completed", "legacy answer"],
  ["unknown marker", { gateway_stream_status: "unexpected" }, "failed", "partial answer"],
  ["null marker", { gateway_stream_status: null }, "failed", "partial answer"],
  ["numeric marker", { gateway_stream_status: 42 }, "failed", "partial answer"],
  ["undefined own marker", { gateway_stream_status: undefined }, "failed", "partial answer"],
  ["tool-only failure", { gateway_stream_status: "failed", tool_calls: [
    { tool_call_id: "tool", name: "fixture", arguments: "{}", status: "error" },
  ] }, "failed", ""],
  ...["failed", "interrupted", "completed"].map(status => [
    `SDK precedence ${status}`, {
      sdk_framework: "openai_agents", assistant_run_id: "00000000-0000-4000-8000-000000000001",
      sdk_stream_status: status, gateway_stream_status: status === "completed" ? "failed" : "completed",
    }, status === "interrupted" ? "cancelled" : status, "SDK answer",
  ]),
] as const) {
  test(`History reload preserves local stream observation: ${name}`, async () => {
    const h = host("dify_chat", fixtures.fresh);
    h.setHistory([{ role: "assistant", content, metadata, timestamp: "2026-10-12T00:00:00Z" }]);
    await h.sessions().handleSelectSession(fixtures.fresh.session_id);
    assert.equal(h.sessions().messages.at(-1)?.status, expected);
    assert.equal(h.sessions().messages.at(-1)?.completionConfirmed,
      name === "completed" || name === "SDK precedence completed");
    assert.equal(h.sessions().messages.at(-1)?.content, content);
    assert.equal(h.streamCalls, 0);
    assert.equal(h.syncCalls, 0);
  });
}


for (const published of [false, true]) {
  test(`Dify Stop retains ${published ? "published text" : "the current empty view"} while binding is pending`, async () => {
  const h = host("dify_chat", fixtures.fresh);
  const entered = deferred<void>(); const release = deferred<void>();
  let advanced = 0; let closed = false;
  h.setPatch(async () => { entered.resolve(); await release.promise; });
  h.setStream(async function* () {
    try {
      yield { ...chunk("Partial"), metadata: { dify_conversation_id: "new-conversation" } };
      advanced++; yield chunk("", true);
    } finally { closed = true; }
  });
  const sending = h.send();
  await Promise.race([entered.promise, sending]);
  try {
    if (published) h.flushFrames();
    assert.equal(h.sessions().messages.at(-1)?.content, published ? "Partial" : "");
    assert.equal(h.sessions().messages.at(-1)?.status, "streaming");
    assert.equal(advanced, 0);
    h.sessions().handleStopStreaming();
    assert.equal(h.sessions().messages.at(-1)?.status, "cancelled");
  } finally { release.resolve(); await sending; }
  h.flushFrames();
  // Stop invalidates queued publications. No stale PATCH/RAF may publish later.
  assert.equal(h.sessions().messages.at(-1)?.content, published ? "Partial" : "");
  assert.equal(h.sessions().messages.at(-1)?.status, "cancelled");
  assert.equal(advanced, 0); assert.equal(closed, true);
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  });
}

for (const rejects of [false, true]) {
  test(`Dify pending binding ${rejects ? "failure" : "success"} cannot change a newly selected view`, async () => {
    const h = host("dify_chat", fixtures.fresh);
    const entered = deferred<void>(); const release = deferred<void>();
    h.setPatch(async () => { entered.resolve(); await release.promise; if (rejects) throw new Error("metadata write failed"); });
    h.setStream(async function* () {
      yield { ...chunk("Old partial"), metadata: { dify_conversation_id: "new-conversation" } };
      yield chunk("", true);
    });
    const sending = h.send();
    await Promise.race([entered.promise, sending]);
    let selected: string;
    try {
      await h.sessions().handleSelectSession("other-session");
      h.sessions().setMessages([{ id: "selected", role: "assistant", content: "Selected view", status: "completed" }]);
      selected = JSON.stringify(h.sessions().messages);
    } finally { release.resolve(); await sending; }
    h.flushFrames();
    assert.equal(JSON.stringify(h.sessions().messages), selected);
    assert.equal(h.store.activeSessionId, "other-session");
    assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  });
}

for (const rejects of [false, true]) {
  test(`Dify final metadata waits for binding ${rejects ? "failure" : "success"} before terminal processing`, async () => {
    const h = host("dify_chat", fixtures.fresh);
    const entered = deferred<void>(); const release = deferred<void>();
    let closed = false;
    h.setPatch(async () => { entered.resolve(); await release.promise; if (rejects) throw new Error("metadata write failed"); });
    h.setStream(async function* () {
      try {
        yield chunk("Answer");
        yield { ...chunk("", true), metadata: { dify_conversation_id: "new-conversation" } };
      } finally { closed = true; }
    });
    const sending = h.send();
    await Promise.race([entered.promise, sending]);
    try {
      h.flushFrames();
      assert.equal(h.sessions().messages.at(-1)?.content, "Answer");
      assert.equal(h.sessions().messages.at(-1)?.status, "streaming");
    } finally { release.resolve(); await sending; }
    h.flushFrames();
    assert.equal(h.sessions().messages.at(-1)?.status, rejects ? "failed" : "completed");
    assert.equal(h.sessions().messages.at(-1)?.content, rejects ? "Answer\n\nFixture failure" : "Answer");
    assert.equal(closed, true);
    assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  });
}

for (const [name, metadata, confirmed] of [
  ["no metadata", undefined, undefined],
  ["legacy empty metadata", {}, false],
  ["confirmed empty", { gateway_stream_status: "completed" }, true],
  ["unknown empty", { gateway_stream_status: "unknown" }, false],
  ["SDK without local terminal", { sdk_framework: "openai_agents", assistant_run_id: "00000000-0000-4000-8000-000000000001", gateway_stream_status: "completed" }, false],
] as const) test(`empty history completion provenance: ${name}`, async () => {
  const h = host("dify_chat", fixtures.fresh);
  h.setHistory([{ role: "assistant", content: " \n", metadata, timestamp: "2026-10-12T00:00:00Z" }]);
  await h.sessions().handleSelectSession(fixtures.fresh.session_id);
  assert.equal(h.sessions().messages.at(-1)?.completionConfirmed, confirmed);
  assert.equal(h.sessions().messages.at(-1)?.content, " \n");
  assert.equal(h.streamCalls, 0); assert.equal(h.syncCalls, 0);
});

for (const key of ["fresh", "resumed"]) test(`custom Dify inputs preserve typed values and independent ${key} conversation identity`, async () => {
  const h = host("dify_chat", fixtures[key]);
  const fields = JSON.parse('{"topic":"private-input-marker","count":2,"flag":false,"nested":{"items":[null,3]},"conversation_id":"field-only","__proto__":{"polluted":true}}');
  await h.send(fields);
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0);
  const parameters = h.requests[0].parameters as Record<string, unknown>;
  assert.ok(parameters, "Dify custom parameters must be present");
  assert.equal(JSON.stringify(parameters.inputs), JSON.stringify(fields));
  assert.equal(parameters.conversation_id, key === "resumed" ? "saved-conversation" : undefined);
  assert.doesNotMatch(JSON.stringify(h.captures), /private-input-marker|field-only|polluted/);
  assert.doesNotMatch(JSON.stringify(h.updates), /private-input-marker|field-only|polluted/);
  assert.doesNotMatch(JSON.stringify(h.sessions().messages), /private-input-marker|field-only|polluted/);
});
test("custom Dify fields are captured before asynchronous session preparation", async () => {
  const h = host("dify_chat", fixtures.fresh); const fields = { nested: { value: "original" } };
  const sending = h.send(fields); fields.nested.value = "changed"; await sending;
  assert.equal(JSON.stringify((h.requests[0].parameters as Record<string, unknown>).inputs), '{"nested":{"value":"original"}}');
});
for (const engine of ["dify", "openai_agents", "claude_agent_sdk", "langgraph"]) test(`custom Dify fields never enter ${engine} dispatch`, async () => {
  const h = host(engine, fixtures.fresh); await h.send({ private: "dify-only-marker" });
  assert.doesNotMatch(JSON.stringify(h.requests), /dify-only-marker/); assert.doesNotMatch(JSON.stringify(h.captures), /dify-only-marker/);
});
test("custom Dify inputs preserve single-attempt interruption behavior", async () => {
  const h = host("dify_chat", fixtures.fresh);
  h.useProductionErrors();
  h.setStream(async function* () { yield chunk("Partial"); throw new Error("private-input-marker"); });
  await h.send({ topic: "private-input-marker" });
  assert.equal(h.streamCalls, 1); assert.equal(h.syncCalls, 0); assert.equal(h.sessions().messages.at(-1)?.status, "failed");
  assert.doesNotMatch(JSON.stringify(h.sessions().messages), /private-input-marker/);
});
for (const conflict of ["proxy", "langgraph"]) test(`contradictory Dify ${conflict} routing cannot forward custom fields`, async () => {
  const h = host("dify_chat", fixtures.fresh);
  for (const service of h.services) {
    if (conflict === "proxy") Object.assign(service.metadata, { proxy_mode: "transparent" });
    else Object.assign(service, { service_type: "langgraph" });
  }
  await h.send({ private: "dify-only-marker" });
  assert.doesNotMatch(JSON.stringify(h.requests), /dify-only-marker/); assert.doesNotMatch(JSON.stringify(h.captures), /dify-only-marker/);
});
for (const phase of ["detail", "initialization"]) for (const change of ["service", "session", "token", "permissions"]) {
  test(`custom Dify pending ${phase} cannot dispatch after pre-rerender ${change} mutation`, async () => {
    const h = host("dify_chat", fixtures.fresh); const entered = deferred<void>(); const release = deferred<void>();
    let initialization: Promise<unknown> | undefined;
    if (phase === "detail") h.setDetailRead(async () => { entered.resolve(); await release.promise; });
    else {
      h.setCreate(async () => { entered.resolve(); await release.promise; return { session_id: fixtures.fresh.session_id }; });
      initialization = h.sessions().handleNewSession();
    }
    const sending = h.send({ private: "never-dispatched" }); await Promise.race([entered.promise, sending]);
    if (change === "service") h.options.serviceId = "other-service";
    else if (change === "session") {
      if (phase === "initialization") await h.sessions().handleSelectSession("other-session");
      else h.store.activeSessionId = "other-session";
    }
    else if (change === "token") h.auth.token = "other-token";
    else Object.assign(h.auth.user, { permissions: ["changed"] });
    release.resolve(); await initialization; await sending;
    assert.equal(h.streamCalls, 0); assert.equal(h.syncCalls, 0); assert.equal(h.captures.length, 0);
  });
}
for (const change of ["service", "session", "token", "permissions"]) test(`custom Dify final transport boundary rechecks ${change} after diagnostic callback`, async () => {
  const h = host("dify_chat", fixtures.fresh);
  h.setCaptureSideEffect(() => {
    if (change === "service") h.options.serviceId = "other-service";
    else if (change === "session") h.store.activeSessionId = "other-session";
    else if (change === "token") h.auth.token = "new-token";
    else Object.assign(h.auth.user, { permissions: ["new"] });
  });
  await h.send({ private: "never-dispatched" });
  assert.equal(h.captures.length, 1); assert.equal(h.streamCalls, 0); assert.equal(h.syncCalls, 0);
  assert.doesNotMatch(JSON.stringify(h.captures), /never-dispatched/);
});
