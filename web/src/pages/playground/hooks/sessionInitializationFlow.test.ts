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
function host(engine = "custom") {
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
  let fetcher: (url: string, init?: { method?: string; body?: string; headers?: Record<string, string> }) => Promise<unknown> = async () => { throw new Error("Unexpected network operation"); };
  const updates: { sessionId: string; body: unknown }[] = [];
  let streamCalls = 0;
  let createCalls = 0;
  const requests: Record<string, unknown>[] = [];
  const streamPaths: string[] = [];
  let list: () => Promise<unknown[]> = async () => [];
  let titleReads = 0;
  let titleWrites = 0;
  let syncCalls = 0;
  const copied: string[] = [];
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
    "../utils/difyConversation": { prepareDifyConversation: async () => ({ ...(engine === "dify_chat" ? { parameters: { conversation_id: "conversation-123" } } : {}), accept: async () => {} }) },
    "@/api/gateway": { invokeService: async () => { syncCalls++; return { outputs: [{ data: "Fallback" }] }; } },
    "@/api/sessions": {
      createSession: () => { createCalls++; return create(); }, deleteSession: async () => {}, listSessions: () => list(),
      getSessionHistory: () => history(), getSession: async () => { titleReads++; return { metadata: { title: "Server title" } }; },
      updateSession: async (sessionId: string, body: unknown) => { titleWrites++; updates.push({ sessionId, body }); return {}; }, addSessionMessage: async () => ({}),
    },
    "@/lib/sse": { sseFetch: (_url: string, init: { body: string }) => { streamCalls++; requests.push(JSON.parse(init.body)); return stream(); },
      sseFetchEvents: async function* (url: string, init: { body: string }) { streamCalls++; streamPaths.push(url); requests.push(JSON.parse(init.body)); for await (const data of stream()) yield { data }; },
      streamChunkToAGUIEvent: () => ({ event: "text_delta" }) },
    "@/hooks/useAgentTimeline": { useAgentTimeline: () => ({ state: { steps: [] }, processEvent: noop, reset: noop }) },
    "@/lib/utils": { getPlaygroundErrorMessage: () => "Fixture failure" },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/store/useAppStore": { useAppStore: Object.assign(() => store, { getState: () => ({ ...store,
      selectedServiceId: options.serviceId, setSelectedServiceId: (id: string) => { options.serviceId = id; } }) }) },
    "@/features/chat/stream": { ...reducer, ...normalizers },
    "@/features/chat/telemetry": { finishChatStreamTrace: noop, markChatStreamFirstToken: noop,
      startChatStreamTrace: () => ({}), trackChatHistoryRestored: noop, trackChatHistoryEmptyState: noop },
    "../utils/requestSnapshot": requestSnapshot, "../utils/langgraph": langgraph,
    "@/features/chat/stream/tokenObservation": tokenObservation,
  };
  function load(filename: string) {
    const source = readFileSync(new URL(filename, import.meta.url), "utf8");
    const exports: Record<string, (value: unknown) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
      { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
        AbortController, Error, DOMException, Date, performance, URLSearchParams,
        window: { location: { search: "", pathname: "/playground" }, history: { replaceState: noop } },
        setTimeout: () => 1, clearTimeout: noop, fetch: (url: string, init: Parameters<typeof fetcher>[1]) => fetcher(url, init),
        navigator: { clipboard: { writeText: async (text: string) => { copied.push(text); } } },
        requestAnimationFrame: (fn: () => void) => { frames.set(++frameId, fn); return frameId; },
        cancelAnimationFrame: (id: number) => frames.delete(id) });
    return exports;
  }
  modules["@/lib/api"] = { api: {
    get: async (url: string, config: { timeout: number }) => {
      assert.equal(config.timeout, 10_000);
      const response = await fetcher(url, { headers: { Authorization: `Bearer ${auth.token}` } }) as { ok: boolean; json: () => Promise<unknown> };
      if (!response.ok) throw new Error("Diagnostic GET failed");
      return { data: await response.json() };
    },
    patch: async (url: string, body: unknown, config: { timeout: number }) => {
      assert.equal(config.timeout, 10_000);
      const response = await fetcher(url, { method: "PATCH", body: JSON.stringify(body), headers: { Authorization: `Bearer ${auth.token}` } }) as { ok: boolean };
      if (!response.ok) throw new Error("Diagnostic PATCH failed");
    },
  } };
  const sessionsModule = load("./usePlaygroundSessions.ts");
  const streamModule = load("./usePlaygroundStream.ts");
  type Sessions = {
    requestSnapshot?: requestSnapshot.PlaygroundRequestSnapshot;
    requestSnapshotStatus: string;
    copyRequestSnapshot: () => Promise<void>;
    sessionEnabled: boolean; setSessionEnabled: (value: boolean) => void;
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
      serviceId: options.serviceId, activeService: services.find(value => value.service_id === options.serviceId),
      activeSessionId: store.activeSessionId, setActiveSessionId: store.setActiveSessionId,
      localTitles: { session: "Existing title", "other-session": "Other title" }, setLocalTitles: noop,
      stickToBottomRef: { current: false }, t: (value: string) => value,
    }) as { handleSend: (input: unknown[]) => Promise<void> };
  }
  return {
    auth, options, store, sessions, creationErrors, copied,
    send: () => streamHook().handleSend([{ type: "text", data: "Keep this input" }]),
    rerender: streamHook,
    setStream(value: typeof stream) { stream = value; }, setCreate(value: typeof create) { create = value; },
    setHistory(value: unknown[]) { histories = value; },
    setHistoryReader(value: typeof history) { history = value; },
    setFetch(value: typeof fetcher) { fetcher = value; }, get updates() { return updates; },
    setList(value: typeof list) { list = value; },
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

test("New then Send keeps only the new request and dispatches once to the created session", async () => {
  const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
  h.sessions().setMessages([{ role: "user", content: "Old prompt" }, { role: "assistant", content: "Old answer", status: "completed" }]);
  const creating = h.sessions().handleNewSession();
  const sending = h.send(); created.resolve({ session_id: "created-session" });
  await creating; await sending;
  assert.equal(h.createCalls, 1); assert.equal(h.streamCalls, 1); assert.equal(h.requests[0].session_id, "created-session");
  assert.equal(h.store.activeSessionId, "created-session");
  assert.equal(h.sessions().messages.length, 2); assert.equal(h.sessions().messages[0].content, "Keep this input");
  assert.equal(h.sessions().messages[1].content, "Answer"); assert.equal(h.sessions().messages[1].status, "completed");
});
for (const send of [false, true]) {
  for (const failed of [false, true]) {
    test(`late New ${failed ? "failure" : "success"} after ${send ? "Send then " : ""}select cannot replace history or dispatch`, async () => {
      const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
      const creating = h.sessions().handleNewSession(); const observedCreate = creating.catch(() => null);
      const sending = send ? h.send() : Promise.resolve();
      h.setHistory([{ role: "user", content: "Saved prompt", timestamp: "1" }, { role: "assistant", content: "Saved answer", timestamp: "2" }]);
      await h.sessions().handleSelectSession("other-session");
      if (failed) created.reject(new Error("create failed")); else created.resolve({ session_id: "late-session" });
      await observedCreate; await sending;
      assert.equal(h.store.activeSessionId, "other-session"); assert.equal(h.sessions().messages[1]?.content, "Saved answer");
      assert.equal(h.streamCalls, 0); assert.equal(h.createCalls, 1);
    });
  }
}
test("creation failure while Send waits fails visibly without another create or old-session dispatch", async () => {
  const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
  const creating = h.sessions().handleNewSession(); const observedCreate = creating.catch(() => null);
  const sending = h.send(); created.reject(new Error("creation failed"));
  await observedCreate; await sending;
  assert.equal(h.createCalls, 1); assert.equal(h.streamCalls, 0); assert.equal(h.store.activeSessionId, undefined);
  assert.equal(h.sessions().messages[0].content, "Keep this input"); assert.equal(h.sessions().messages[1].status, "failed");
  assert.equal(h.sessions().loading, false); assert.equal(h.creationErrors.length, 0);
  h.setCreate(async () => ({ session_id: "retry-session" })); await h.send();
  assert.equal(h.createCalls, 2); assert.equal(h.streamCalls, 1); assert.equal(h.requests[0].session_id, "retry-session");
});
test("New failure without Send notifies the page and never restores an implicit old target", async () => {
  const h = host(); h.sessions().setMessages([{ role: "assistant", content: "Old partial", status: "streaming", isStreaming: true }]);
  h.setCreate(async () => { throw new Error("creation failed"); });
  assert.equal(await h.sessions().handleNewSession(), null);
  assert.equal(h.creationErrors.length, 1); assert.equal(h.store.activeSessionId, undefined);
  assert.equal(h.sessions().messages.length, 0);
  h.setCreate(async () => ({ session_id: "explicit-retry-session" })); await h.send();
  assert.equal(h.createCalls, 2); assert.equal(h.requests[0].session_id, "explicit-retry-session");
});
test("newer New after selection is not deduplicated onto the abandoned create", async () => {
  const h = host(); const first = deferred<{ session_id: string }>(); const second = deferred<{ session_id: string }>();
  h.setCreate(() => h.createCalls === 1 ? first.promise : second.promise);
  const oldCreate = h.sessions().handleNewSession(); await h.sessions().handleSelectSession("other-session");
  const newCreate = h.sessions().handleNewSession(); second.resolve({ session_id: "second-created" });
  first.resolve({ session_id: "abandoned-created" }); await oldCreate; await newCreate;
  assert.equal(h.createCalls, 2); assert.equal(h.store.activeSessionId, "second-created");
});
for (const boundary of ["account", "token", "service", "unmount"]) {
  test(`late create does not publish after ${boundary} changes`, async () => {
    const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
    const creating = h.sessions().handleNewSession(); await h.commitSessionEffects();
    if (boundary === "account") h.auth.user = { ...h.auth.user, user_id: "other" };
    if (boundary === "token") h.auth.token = "replacement-token";
    if (boundary === "service") { h.options.serviceId = "other-service"; h.rerender(); }
    if (boundary === "unmount") h.unmount();
    h.store.activeSessionId = "current-view";
    h.sessions().setMessages([{ role: "assistant", content: "Current view", status: "completed" }]);
    created.resolve({ session_id: "late-created" }); await creating;
    assert.equal(h.store.activeSessionId, "current-view"); assert.equal(h.sessions().messages[0]?.content, "Current view");
  });
}
test("repeated New in the same active view shares one pending create", async () => {
  const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
  const one = h.sessions().handleNewSession(); const two = h.sessions().handleNewSession();
  created.resolve({ session_id: "one-session" }); assert.equal(await one, "one-session"); assert.equal(await two, "one-session");
  assert.equal(h.createCalls, 1);
});
test("New and waiting Send do not wait for a background session-list refresh", async () => {
  const h = host(); const list = deferred<unknown[]>(); h.setList(() => list.promise);
  const creating = h.sessions().handleNewSession(); const sending = h.send();
  // Allow only promise microtasks, then release the list before assertions to keep failures bounded.
  for (let i = 0; i < 12; i++) await Promise.resolve();
  const dispatchedBeforeList = h.streamCalls; list.resolve([]); await creating; await sending;
  assert.equal(dispatchedBeforeList, 1); assert.equal(h.requests[0].session_id, "new-session");
});


test("late initialization sidebar refresh cannot overwrite a newer session list", async () => {
  const h = host(); const listing = deferred<unknown[]>(); h.setList(() => listing.promise);
  const creating = h.sessions().handleNewSession();
  for (let i = 0; i < 10; i++) await Promise.resolve();
  await h.sessions().handleSelectSession("other-session");
  const before = h.sessions().sessions;
  listing.resolve([{ session_id: "stale-sidebar-row", metadata: { title: "Stale" } }]);
  await creating; for (let i = 0; i < 5; i++) await Promise.resolve();
  assert.equal(h.sessions().sessions, before);
});
test("sidebar refresh failure cannot turn a successfully created session into failed Send", async () => {
  const h = host(); h.setList(async () => { throw new Error("sidebar unavailable"); });
  const creating = h.sessions().handleNewSession(); const sending = h.send();
  assert.equal(await creating, "new-session"); await sending;
  assert.equal(h.streamCalls, 1); assert.equal(h.sessions().messages.at(-1)?.status, "completed");
});
for (const origin of ["new", "select"]) {
  test(`late LangGraph thread identity from ${origin} cannot be written after navigation`, async () => {
    const h = host("langgraph"); const thread = deferred<unknown>(); h.setFetch(() => thread.promise);
    // Select an uncached session to exercise its real eager-thread preparation.
    if (origin === "new") await h.sessions().handleNewSession();
    else await h.sessions().handleSelectSession("uncached-session");
    h.options.serviceId = "other-service"; h.rerender();
    thread.resolve({ ok: true, json: async () => ({ thread_id: "late-thread" }) });
    for (let i = 0; i < 8; i++) await Promise.resolve();
    assert.equal(h.updates.length, 0);
  });
}
test("reselecting the same history uses navigation generation, not only session ID", async () => {
  const h = host(); const first = deferred<unknown[]>(); const second = deferred<unknown[]>();
  let count = 0; h.setHistoryReader(() => ++count === 1 ? first.promise : second.promise);
  const old = h.sessions().handleSelectSession("session"); const current = h.sessions().handleSelectSession("session");
  second.resolve([{ role: "assistant", content: "Current answer", timestamp: "2" }]); await current;
  first.resolve([{ role: "assistant", content: "Old answer", timestamp: "1" }]); await old;
  assert.equal(h.sessions().messages[0].content, "Current answer");
});
test("a different account's New cannot reuse the previous account's pending creation", async () => {
  const h = host(); const old = deferred<{ session_id: string }>();
  h.setCreate(() => h.createCalls === 1 ? old.promise : Promise.resolve({ session_id: "current-account-session" }));
  const first = h.sessions().handleNewSession(); h.auth.user = { ...h.auth.user, user_id: "other" };
  const second = h.sessions().handleNewSession();
  old.resolve({ session_id: "old-account-session" }); await first;
  assert.equal(await second, "current-account-session");
  assert.equal(h.createCalls, 2); assert.equal(h.store.activeSessionId, "current-account-session");
});
test("Stop during real New then Send preserves cancelled input without later dispatch", async () => {
  const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
  const creating = h.sessions().handleNewSession(); const sending = h.send(); h.sessions().handleStopStreaming();
  created.resolve({ session_id: "created-after-stop" }); await creating; await sending;
  assert.equal(h.streamCalls, 0); assert.equal(h.sessions().messages[0].content, "Keep this input");
  assert.equal(h.sessions().messages[1].status, "cancelled");
});

for (const source of ["ordinary-refresh", "initial-list"]) {
  for (const boundary of ["account", "token", "service", "select", "unmount"]) {
    test(`${source} cannot publish a response after ${boundary} changes`, async () => {
      const h = host(); const listing = deferred<unknown[]>(); h.setList(() => listing.promise);
      let read = Promise.resolve();
      if (source === "ordinary-refresh") { await h.commitSessionEffects(); read = h.sessions().refreshSessions(true); }
      else await h.commitSessionEffects();
      if (boundary === "account") h.auth.user = { ...h.auth.user, user_id: "other" };
      if (boundary === "token") h.auth.token = "replacement-token";
      if (boundary === "service") { h.options.serviceId = "other-service"; h.rerender(); }
      if (boundary === "select") await h.sessions().handleSelectSession("other-session");
      if (boundary === "unmount") h.unmount();
      listing.resolve([{ session_id: "stale-row", metadata: { title: "Stale" } }]); await read;
      for (let i = 0; i < 5; i++) await Promise.resolve();
      assert.equal(h.sessions().sessions.some(row => row.session_id === "stale-row"), false);
    });
  }
}
test("selection releases a loading spinner owned by an obsolete list read", async () => {
  const h = host(); const listing = deferred<unknown[]>(); h.setList(() => listing.promise);
  const read = h.sessions().refreshSessions(); assert.equal(h.sessions().sessionsLoading, true);
  await h.sessions().handleSelectSession("other-session"); assert.equal(h.sessions().sessionsLoading, false);
  listing.resolve([]); await read; assert.equal(h.sessions().sessionsLoading, false);
});

test("an obsolete Send cannot erase the replacement thread promise or create a fallback thread", async () => {
  const h = host("langgraph"); const oldThread = deferred<unknown>(); const nextThread = deferred<unknown>(); let fetches = 0;
  h.setFetch(url => url.includes("/threads") ? (++fetches === 1 ? oldThread.promise : nextThread.promise) : Promise.resolve({ ok: false }));
  await h.sessions().handleNewSession(); const sending = h.send();
  for (let i = 0; i < 4; i++) await Promise.resolve();
  await h.sessions().handleSelectSession("uncached-other-session");
  const pending = h.sessions().pendingThreadRef.current;
  oldThread.resolve({ ok: true, json: async () => ({ thread_id: "old-thread" }) }); await sending;
  assert.equal(fetches, 2); assert.equal(h.sessions().pendingThreadRef.current, pending); assert.equal(h.streamCalls, 0);
  nextThread.resolve({ ok: true, json: async () => ({ thread_id: "current-thread" }) });
  for (let i = 0; i < 8; i++) await Promise.resolve();
  assert.equal(h.sessions().sessionThreadIdRef.current["uncached-other-session"], "current-thread");
});
for (const boundary of ["select", "token", "service", "unmount"]) {
  test(`late fallback thread cannot write metadata after ${boundary}`, async () => {
    const h = host("langgraph"); const thread = deferred<unknown>(); h.setFetch(() => thread.promise);
    h.store.activeSessionId = "uncached-session"; await h.send(); await h.commitSessionEffects();
    if (boundary === "select") await h.sessions().handleSelectSession("other-session");
    if (boundary === "token") h.auth.token = "replacement-token";
    if (boundary === "service") { h.options.serviceId = "other-service"; h.rerender(); }
    if (boundary === "unmount") h.unmount();
    thread.resolve({ ok: true, json: async () => ({ thread_id: "late-thread" }) });
    for (let i = 0; i < 8; i++) await Promise.resolve();
    assert.equal(h.updates.some(update => JSON.stringify(update.body).includes("langgraph_thread_id")), false);
  });
}
test("the production page reports standalone creation failure with localized safe text", () => {
  const filename = new URL("../index.tsx", import.meta.url);
  const source = ts.createSourceFile(filename.pathname, readFileSync(filename, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let callback: ts.Expression | undefined;
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node) && node.expression.getText(source) === "usePlaygroundSessions") {
      const args = node.arguments[0]; assert.ok(ts.isObjectLiteralExpression(args));
      const prop = args.properties.find(value => ts.isPropertyAssignment(value) && value.name.getText(source) === "onSessionCreationError");
      if (prop && ts.isPropertyAssignment(prop)) callback = prop.initializer;
    }
    ts.forEachChild(node, visit);
  };
  visit(source); assert.ok(callback);
  const code = ts.createPrinter().printNode(ts.EmitHint.Expression, callback, source);
  for (const locale of ["en-US", "zh-CN"]) {
    const translations = JSON.parse(readFileSync(new URL(`../../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
    let notice: string[] | undefined;
    const notify = vm.runInNewContext(ts.transpileModule(`(${code})`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, {
      toast: { error: (...args: string[]) => { notice = args; } },
      t: (key: string) => key.split(".").reduce((value, part) => value[part], translations),
      getPlaygroundErrorMessage: () => "Safe diagnostic",
    });
    notify(new Error("fixture transport failure"));
    const expected = translations.playground.errors.sessionCreationFailed;
    assert.equal(typeof expected, "string"); assert.ok(expected.trim().length > 0);
    assert.equal(notice?.[0], expected);
    assert.equal(notice?.[1], "Safe diagnostic");
  }
});

test("duplicate New failures are both handled and notify only once", async () => {
  const h = host(); const created = deferred<{ session_id: string }>(); h.setCreate(() => created.promise);
  const one = h.sessions().handleNewSession(); const two = h.sessions().handleNewSession();
  const observed = Promise.allSettled([one, two]); created.reject(new Error("uncertain creation"));
  const results = await observed;
  assert.deepEqual(results.map(value => value.status), ["fulfilled", "fulfilled"]);
  assert.equal(await one, null); assert.equal(await two, null); assert.equal(h.creationErrors.length, 1);
});
for (const action of ["new", "select"]) {
  for (const failed of [false, true]) {
    test(`initial list ${failed ? "rejection" : "resolution"} after ${action} cannot disable a later service switch`, async () => {
      const h = host(); const listing = deferred<unknown[]>(); let reads = 0;
      h.setList(() => ++reads === 1 ? listing.promise : Promise.resolve([]));
      await h.commitSessionEffects();
      if (action === "new") await h.sessions().handleNewSession();
      else await h.sessions().handleSelectSession("other-session");
      if (failed) listing.reject(new Error("initial list failed")); else listing.resolve([]);
      for (let i = 0; i < 6; i++) await Promise.resolve();
      h.sessions().setMessages([{ role: "assistant", content: "Previous service" }]);
      h.options.serviceId = "other-service"; h.rerender(); await h.commitSessionEffects();
      assert.equal(h.store.activeSessionId, undefined); assert.equal(h.sessions().messages.length, 0);
      assert.ok(reads >= 2);
    });
  }
}
test("service switch takes precedence even while the initial list remains pending", async () => {
  const h = host(); const listing = deferred<unknown[]>(); let reads = 0;
  h.setList(() => ++reads === 1 ? listing.promise : Promise.resolve([])); await h.commitSessionEffects();
  h.sessions().setMessages([{ role: "assistant", content: "Previous service" }]);
  h.options.serviceId = "other-service"; h.rerender(); await h.commitSessionEffects();
  assert.equal(h.sessions().messages.length, 0); assert.equal(h.store.activeSessionId, undefined);
  listing.resolve([{ session_id: "old-row" }]); for (let i = 0; i < 5; i++) await Promise.resolve();
  assert.equal(h.sessions().sessions.length, 0);
});

test("Send in the selected view consumes its pending eager thread without another POST", async () => {
  const h = host("langgraph"); const thread = deferred<unknown>(); let fetches = 0;
  h.setFetch(url => { if (!url.includes("/threads")) return Promise.resolve({ ok: false }); fetches++; return thread.promise; });
  await h.sessions().handleSelectSession("uncached-target"); const sending = h.send();
  thread.resolve({ ok: true, json: async () => ({ thread_id: "selected-thread" }) }); await sending;
  assert.equal(fetches, 1); assert.equal(h.streamCalls, 1);
  assert.equal(h.streamPaths[0], "/api/v1/proxy/service/threads/selected-thread/runs/stream");
  assert.equal(h.updates.filter(update => JSON.stringify(update.body).includes("langgraph_thread_id")).length, 1);
});

test("automatic persisted-history restore does not invalidate its queued sidebar rows", async () => {
  const h = host(); h.setList(async () => [{ session_id: "session" }]);
  h.setHistory([{ role: "assistant", content: "Restored answer", timestamp: "1" }]);
  h.holdNextSessionsUpdate(); await h.commitSessionEffects();
  for (let i = 0; i < 6; i++) await Promise.resolve();
  h.flushHeldUpdates();
  assert.equal(h.sessions().sessions[0]?.session_id, "session");
  assert.equal(h.sessions().messages[0]?.content, "Restored answer");
});
test("a newer explicit selection still invalidates the queued initial sidebar update", async () => {
  const h = host(); h.setList(async () => [{ session_id: "session" }]);
  h.setHistory([{ role: "assistant", content: "Restored answer", timestamp: "1" }]);
  h.holdNextSessionsUpdate(); await h.commitSessionEffects();
  for (let i = 0; i < 6; i++) await Promise.resolve();
  await h.sessions().handleSelectSession("other-session"); h.flushHeldUpdates();
  assert.equal(h.sessions().sessions.length, 0); assert.equal(h.store.activeSessionId, "other-session");
});

// Model the exact generic native session contract: PATCH merges top-level metadata,
// list summaries omit playground_request, and older detail GETs omit service_id.
function diagnosticStore(h: ReturnType<typeof host>) {
  let metadata: Record<string, unknown> = { title: "Existing title", langgraph_thread_id: "thread" };
  const writes: Record<string, unknown>[] = [];
  let patch: (body: Record<string, unknown>) => Promise<void> = async body => {
    metadata = { ...metadata, ...body.metadata as Record<string, unknown> };
  };
  let read: () => Promise<unknown> = async () => ({ session_id: "session", metadata: structuredClone(metadata) });
  h.setFetch(async (url, init) => {
    assert.equal(url, "/api/v1/sessions/session");
    assert.equal(init?.headers?.Authorization, "Bearer fixture-token");
    if (init?.method === "PATCH") {
      const body = JSON.parse(init.body!); writes.push(body); await patch(body);
      return { ok: true, json: async () => ({ session_id: "session", status: "updated" }) };
    }
    return { ok: true, json: read };
  });
  h.setList(async () => [{ session_id: "session", service_id: "service", metadata: { title: "Existing title", langgraph_thread_id: "thread" } }]);
  return { writes, get metadata() { return metadata; }, setPatch(value: typeof patch) { patch = value; }, setRead(value: typeof read) { read = value; } };
}
const settleDiagnostics = () => new Promise<void>(resolve => setImmediate(resolve));

test("actual Gateway dispatch snapshot copies input but native metadata roundtrip restores settings only", async () => {
  const h = host(); const store = diagnosticStore(h);
  await h.send(); await settleDiagnostics();
  const live = h.sessions().requestSnapshot!;
  assert.equal(live.input_text, h.requests[0].inputs[0].data);
  assert.equal(live.session_id, h.requests[0].session_id);
  assert.equal(h.sessions().requestSnapshotStatus, "saved");
  assert.equal(JSON.stringify(store.metadata).includes("Keep this input"), false);
  assert.equal(store.metadata.title, "Existing title");
  await h.sessions().copyRequestSnapshot();
  assert.equal(JSON.parse(h.copied[0]).content, "includes_request_text_review_before_sharing");
  await h.sessions().refreshSessions(); // Slim summary cannot supply the snapshot.
  await h.sessions().handleSelectSession("session"); await settleDiagnostics();
  assert.equal(h.sessions().requestSnapshotStatus, "restored");
  assert.equal(h.sessions().requestSnapshot?.attempt_id, live.attempt_id);
  assert.equal(h.sessions().requestSnapshot?.input_text, undefined);
  await h.sessions().copyRequestSnapshot();
  assert.equal(JSON.parse(h.copied[1]).content, "request_text_omitted");
});

test("Memory false is captured exactly without writing unrelated selected-session settings", async () => {
  const h = host(); const store = diagnosticStore(h);
  h.sessions().setSessionEnabled(false); await h.send(); await settleDiagnostics();
  assert.equal(h.requests[0].session_id, undefined);
  assert.equal(h.sessions().requestSnapshot?.memory_enabled, false);
  assert.equal(h.sessions().requestSnapshot?.session_id, undefined);
  assert.equal(h.sessions().requestSnapshotStatus, "local"); assert.equal(store.writes.length, 0);
});

test("repeated sends serialize settings writes; older save completion never relabels the newer snapshot", async () => {
  const h = host(); const store = diagnosticStore(h); const first = deferred<void>();
  store.setPatch(async () => { if (store.writes.length === 1) await first.promise; });
  await h.send(); await settleDiagnostics(); const firstAttempt = h.sessions().requestSnapshot?.attempt_id;
  await h.send(); const secondAttempt = h.sessions().requestSnapshot?.attempt_id;
  assert.notEqual(firstAttempt, secondAttempt); assert.equal(store.writes.length, 1);
  assert.equal(h.sessions().requestSnapshotStatus, "saving");
  first.resolve(); await settleDiagnostics();
  assert.equal(store.writes.length, 2);
  assert.equal(store.writes[1].metadata.playground_request.attempt_id, secondAttempt);
  assert.equal(h.sessions().requestSnapshot?.attempt_id, secondAttempt);
  assert.equal(h.sessions().requestSnapshotStatus, "saved");
});

test("failed diagnostic PATCH cannot fail successful inference or trigger replay", async () => {
  const h = host(); diagnosticStore(h);
  h.setFetch(async () => ({ ok: false, status: 500 }));
  await h.send(); await settleDiagnostics();
  assert.equal(h.sessions().messages.at(-1)?.status, "completed");
  assert.equal(h.sessions().requestSnapshotStatus, "failed");
  assert.equal(h.auth.token, "fixture-token"); assert.equal(h.streamCalls, 1);
  await h.sessions().copyRequestSnapshot(); assert.equal(h.copied.length, 1);
});

test("selected GET accepts owning Memory=false and never substitutes current edited Memory into snapshot", async () => {
  const h = host(); const store = diagnosticStore(h);
  await h.send(); await settleDiagnostics();
  const stored = { ...h.sessions().requestSnapshot!, memory_enabled: false };
  store.setRead(async () => ({ session_id: "session", service_id: "service", metadata: { playground_request: stored } }));
  await h.sessions().handleSelectSession("session"); await settleDiagnostics();
  assert.equal(h.sessions().sessionEnabled, false);
  h.sessions().setSessionEnabled(true);
  await h.sessions().copyRequestSnapshot(); assert.equal(JSON.parse(h.copied[0]).memory_enabled, false);
});

test("late metadata GET cannot overwrite edits, a newer attempt, navigation, or account", async () => {
  for (const action of ["edit", "send", "navigate", "account"]) {
    const h = host(); const store = diagnosticStore(h); await h.send(); await settleDiagnostics();
    const old = { ...h.sessions().requestSnapshot!, memory_enabled: false };
    const pending = deferred<unknown>(); store.setRead(() => pending.promise);
    await h.sessions().handleSelectSession("session");
    if (action === "edit") h.sessions().setSessionEnabled(true);
    if (action === "send") await h.send();
    if (action === "navigate") await h.sessions().handleNewSession();
    if (action === "account") h.auth.token = "new-owner-token";
    pending.resolve({ session_id: "session", metadata: { playground_request: old } }); await settleDiagnostics();
    assert.equal(h.sessions().sessionEnabled, true);
    if (action === "send") assert.notEqual(h.sessions().requestSnapshot?.attempt_id, old.attempt_id);
    if (action === "navigate" || action === "account") assert.equal(h.sessions().requestSnapshot, undefined);
  }
});

test("stop preserves live snapshot and navigation fences copy captured before view change", async () => {
  const h = host(); diagnosticStore(h); const wait = deferred<void>();
  h.setStream(async function* () { yield chunk("Partial"); await wait.promise; yield chunk("", true); });
  const sending = h.send(); await settleDiagnostics();
  h.sessions().handleStopStreaming(); assert.equal(h.sessions().requestSnapshot?.input_text, "Keep this input");
  await h.sessions().copyRequestSnapshot(); const staleCopy = h.sessions().copyRequestSnapshot;
  await h.sessions().handleNewSession(); await assert.rejects(staleCopy());
  wait.resolve(); await sending; assert.equal(h.sessions().requestSnapshot, undefined);
});

for (const engine of ["langgraph", "dify_chat"]) {
  test(`${engine} snapshot captures actual protocol-specific body and context`, async () => {
    const h = host(engine); diagnosticStore(h); await h.send(); await settleDiagnostics();
    const snapshot = h.sessions().requestSnapshot!;
    if (engine === "langgraph") {
      assert.equal(snapshot.protocol, "langgraph"); assert.equal(snapshot.thread_id, "thread");
      assert.equal(snapshot.durability, h.requests[0].durability);
      assert.equal(JSON.stringify(snapshot.stream_mode), JSON.stringify(h.requests[0].stream_mode));
      assert.equal(snapshot.input_text, h.requests[0].input.messages[0].content);
    } else {
      assert.equal(snapshot.protocol, "gateway"); assert.equal(snapshot.conversation_id, h.requests[0].parameters.conversation_id);
    }
  });
}

test("diagnostic restoration rejects explicit service mismatch without blocking history", async () => {
  const h = host(); const store = diagnosticStore(h); await h.send(); await settleDiagnostics();
  const snapshot = h.sessions().requestSnapshot;
  store.setRead(async () => ({ session_id: "session", service_id: "other-service", metadata: { playground_request: snapshot } }));
  h.setHistory([{ role: "assistant", content: "Authorized history", timestamp: "1" }]);
  await h.sessions().handleSelectSession("session"); await settleDiagnostics();
  assert.equal(h.sessions().requestSnapshot, undefined);
  assert.equal(h.sessions().messages[0].content, "Authorized history");
});

test("queued diagnostics cannot write after view or authority changes", async () => {
  for (const boundary of ["navigation", "authority"]) {
    const h = host(); const store = diagnosticStore(h); const held = deferred<void>();
    store.setPatch(() => held.promise);
    await h.send(); await settleDiagnostics(); await h.send();
    if (boundary === "navigation") await h.sessions().handleNewSession();
    else h.auth.token = "replacement-token";
    held.resolve(); await settleDiagnostics();
    assert.equal(store.writes.length, 1); assert.equal(h.sessions().requestSnapshot, undefined);
  }
});

test("explicit null service identity cannot restore a service-bound snapshot", async () => {
  const h = host(); const store = diagnosticStore(h); await h.send(); await settleDiagnostics();
  const snapshot = h.sessions().requestSnapshot;
  store.setRead(async () => ({ session_id: "session", service_id: null, metadata: { playground_request: snapshot } }));
  await h.sessions().handleSelectSession("session"); await settleDiagnostics();
  assert.equal(h.sessions().requestSnapshot, undefined);
});

test("missing selected service is retained rather than silently switching the conversation target", async () => {
  const h = host(); h.options.serviceId = "removed-service"; h.store.activeSessionId = undefined;
  h.sessions().setMessages([{ role: "user", content: "Keep current conversation" }]);
  await h.commitSessionEffects();
  assert.equal(h.options.serviceId, "removed-service");
  assert.equal(h.sessions().messages[0]?.content, "Keep current conversation");
  assert.equal(h.createCalls, 0); assert.equal(h.streamCalls, 0);
});

test("initial no-selection view still chooses the first available service without invoking a model", async () => {
  const h = host(); h.options.serviceId = ""; h.store.activeSessionId = undefined;
  await h.commitSessionEffects();
  assert.equal(h.options.serviceId, "service"); assert.equal(h.createCalls, 0); assert.equal(h.streamCalls, 0);
});
