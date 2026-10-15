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
  const options = { serviceId: "service", services };
  let stream: () => AsyncGenerator<StreamChunk | { event: string; data: unknown }> = async function* () { yield chunk("Answer"); yield chunk("", true); };
  let create: () => Promise<{ session_id: string }> = async () => ({ session_id: "new-session" });
  let histories: unknown[] = [];
  let streamCalls = 0;
  let titleReads = 0;
  let titleWrites = 0;
  let syncCalls = 0;
  const frames = new Map<number, () => void>();
  let frameId = 0;
  let holdNextMessageUpdate = false;
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
      createSession: () => create(), deleteSession: async () => {}, listSessions: async () => [],
      getSessionHistory: async () => histories, getSession: async () => { titleReads++; return { metadata: { title: "Server title" } }; },
      updateSession: async () => { titleWrites++; return {}; }, addSessionMessage: async () => ({}),
    },
    "@/lib/sse": { sseFetch: () => { streamCalls++; return stream(); },
      sseFetchEvents: async function* () { streamCalls++; for await (const data of stream()) yield "event" in data ? data : { data }; },
      streamChunkToAGUIEvent: () => ({ event: "text_delta" }) },
    "@/hooks/useAgentTimeline": { useAgentTimeline: () => ({ state: { steps: [] }, processEvent: noop, reset: noop }) },
    "@/lib/utils": { getPlaygroundErrorMessage: () => "Fixture failure" },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/store/useAppStore": { useAppStore: Object.assign(() => store, { getState: () => ({ ...store,
      selectedServiceId: options.serviceId, setSelectedServiceId: (id: string) => { options.serviceId = id; } }) }) },
    "@/features/chat/stream": { ...reducer, ...normalizers },
    "@/features/chat/telemetry": { finishChatStreamTrace: noop, markChatStreamFirstToken: noop,
      startChatStreamTrace: () => ({}), trackChatHistoryRestored: noop, trackChatHistoryEmptyState: noop },
    "@/lib/api": { api: { get: async () => ({ data: {} }), patch: async () => ({}) } }, "../utils/requestSnapshot": requestSnapshot, "../utils/langgraph": langgraph,
    "@/features/chat/stream/tokenObservation": tokenObservation,
  };
  function load(filename: string) {
    const source = readFileSync(new URL(filename, import.meta.url), "utf8");
    const exports: Record<string, (value: unknown) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
      { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
        AbortController, Error, DOMException, Date, performance, URLSearchParams,
        window: { location: { search: "", pathname: "/playground" }, history: { replaceState: noop } },
        setTimeout: () => 1, fetch: async () => { throw new Error("Unexpected network operation"); },
        requestAnimationFrame: (fn: () => void) => { frames.set(++frameId, fn); return frameId; },
        cancelAnimationFrame: (id: number) => frames.delete(id) });
    return exports;
  }
  const sessionsModule = load("./usePlaygroundSessions.ts");
  const streamModule = load("./usePlaygroundStream.ts");
  type Sessions = {
    messages: ChatMessage[]; loading: boolean;
    setMessages: (value: ChatMessage[]) => void; setLoading: (value: boolean) => void;
    handleStopStreaming: () => void; handleSelectSession: (id: string) => Promise<void>;
    handleNewSession: () => Promise<unknown>; handleDeleteSession: (id: string) => Promise<void>;
    sessionThreadIdRef: { current: Record<string, string> };
    pendingSessionInitRef: { current: Promise<string | null> | null };
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
    auth, options, store, sessions,
    send: () => streamHook().handleSend([{ type: "text", data: "Keep this input" }]),
    rerender: streamHook,
    setStream(value: typeof stream) { stream = value; }, setCreate(value: typeof create) { create = value; },
    setHistory(value: unknown[]) { histories = value; },
    flushFrames() { const pending = [...frames.values()]; frames.clear(); for (const fn of pending) fn(); },
    holdNextMessageUpdate() { holdNextMessageUpdate = true; },
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
const engines = ["custom", "dify_chat", "langgraph", "openai_agents", "claude_agent_sdk"];
for (const engine of engines) {
  for (const partial of [false, true]) {
    test(`${engine}: Stop ${partial ? "after" : "before"} content is immediately cancelled and stays cancelled after EOF`, async () => {
      const h = host(engine); const ready = deferred<void>(); const finish = deferred<void>();
      h.setStream(async function* () {
        if (partial) { yield chunk("Partial answer"); h.flushFrames(); }
        ready.resolve(); await finish.promise;
      });
      const sending = h.send(); await ready.promise;
      const controller = h.sessions().abortControllerRef.current!;
      h.sessions().handleStopStreaming();
      const stopped = h.sessions();
      // Assert after releasing the outstanding request so a failing baseline leaves no pending work.
      const immediateStatus = stopped.messages.at(-1)?.status;
      const immediateLoading = stopped.loading;
      finish.resolve(); await sending;
      assert.equal(immediateStatus, "cancelled"); assert.equal(immediateLoading, false);
      assert.equal(controller.signal.aborted, true);
      assert.equal(h.sessions().messages.at(-1)?.status, "cancelled");
      assert.equal(h.sessions().messages.at(-1)?.content, partial ? "Partial answer" : "");
      assert.equal(h.sessions().messages[0].content, "Keep this input");
      assert.equal(h.sessions().messages.at(-1)?.isStreaming, false);
      assert.equal(h.syncCalls, 0);
    });
  }
  test(`${engine}: a stopped old stream cannot finalize a new session's active response`, async () => {
    const h = host(engine); const firstReady = deferred<void>(); const firstDone = deferred<void>();
    h.setStream(async function* () { yield chunk("Old partial"); h.flushFrames(); firstReady.resolve(); await firstDone.promise; });
    const first = h.send(); await firstReady.promise;
    await h.sessions().handleSelectSession("other-session");
    const secondReady = deferred<void>(); const secondDone = deferred<void>();
    h.setStream(async function* () { yield chunk("New partial"); h.flushFrames(); secondReady.resolve(); await secondDone.promise; yield chunk("", true); });
    const second = h.send(); await secondReady.promise;
    firstDone.resolve(); await first;
    const current = h.sessions(); const status = current.messages.at(-1)?.status;
    const isStreaming = current.messages.at(-1)?.isStreaming;
    const loading = current.loading;
    secondDone.resolve(); await second;
    assert.equal(status, "streaming"); assert.equal(isStreaming, true); assert.equal(loading, true);
    assert.equal(h.sessions().messages.at(-1)?.status, "completed");
    assert.equal(h.sessions().messages.at(-1)?.content, "New partial");
  });
}
for (const engine of ["custom", "dify_chat", "openai_agents"]) {
  test(`${engine}: a late failure cannot replace restored history or issue a title read`, async () => {
    const h = host(engine); const ready = deferred<void>(); const finish = deferred<void>();
    // This transport rejects before its first frame.
    // eslint-disable-next-line require-yield
    h.setStream(async function* () { ready.resolve(); await finish.promise; throw new Error("late request failure"); });
    const sending = h.send(); await ready.promise;
    h.setHistory([{ role: "user", content: "Other prompt", timestamp: "1" }, { role: "assistant", content: "Saved answer", timestamp: "2" }]);
    await h.sessions().handleSelectSession("other-session");
    finish.resolve(); await sending;
    assert.equal(h.sessions().messages.at(-1)?.content, "Saved answer");
    assert.equal(h.sessions().messages.at(-1)?.status, "completed");
    assert.equal(h.sessions().loading, false); assert.equal(h.titleReads, 0); assert.equal(h.syncCalls, 0);
  });
}
for (const boundary of ["account", "token", "service"]) {
  test(`late Dify failure cannot update a changed ${boundary}`, async () => {
    const h = host("dify_chat"); const ready = deferred<void>(); const finish = deferred<void>();
    // eslint-disable-next-line require-yield
    h.setStream(async function* () { ready.resolve(); await finish.promise; throw new Error("late failure"); });
    const sending = h.send(); await ready.promise;
    if (boundary === "account") h.auth.user = { ...h.auth.user, user_id: "other" };
    if (boundary === "token") h.auth.token = "replacement-fixture-token";
    if (boundary === "service") { h.options.serviceId = "other-service"; h.rerender(); }
    h.sessions().setMessages([{ role: "assistant", content: "New view", status: "completed" }]);
    finish.resolve(); await sending;
    assert.equal(h.sessions().messages.length, 1); assert.equal(h.sessions().messages[0].content, "New view");
    assert.equal(h.titleReads, 0);
  });
}
test("Stop while session initialization is pending prevents a later request and title write", async () => {
  const h = host(); const pending = deferred<string | null>();
  h.sessions().pendingSessionInitRef.current = pending.promise;
  const sending = h.send(); h.sessions().handleStopStreaming();
  pending.resolve("late-session"); await sending;
  assert.equal(h.streamCalls, 0); assert.equal(h.titleWrites, 0);
  assert.equal(h.sessions().messages.at(-1)?.status, "cancelled");
});
for (const action of ["new", "delete"]) {
  test(`${action} active session immediately releases loading and late stream failure cannot replace it`, async () => {
    const h = host("dify_chat"); const ready = deferred<void>(); const finish = deferred<void>();
    h.setStream(async function* () { yield chunk("Partial"); h.flushFrames(); ready.resolve(); await finish.promise; throw new Error("late failure"); });
    const sending = h.send(); await ready.promise;
    if (action === "new") await h.sessions().handleNewSession();
    else await h.sessions().handleDeleteSession("session");
    const loading = h.sessions().loading; finish.resolve(); await sending;
    assert.equal(loading, false); assert.equal(h.sessions().messages.length, 0);
  });
}
test("a failed new-session request leaves a detached new view, never the old conversation", async () => {
  const h = host(); const ready = deferred<void>(); const finish = deferred<void>();
  h.setStream(async function* () { yield chunk("Partial"); h.flushFrames(); ready.resolve(); await finish.promise; });
  const sending = h.send(); await ready.promise;
  h.setCreate(async () => { throw new Error("session creation failed"); });
  assert.equal(await h.sessions().handleNewSession(), null);
  finish.resolve(); await sending;
  assert.equal(h.sessions().messages.length, 0); assert.equal(h.sessions().loading, false);
  assert.equal(h.store.activeSessionId, undefined);
});
test("the current Dify failure still fails the active response without replay", async () => {
  const h = host("dify_chat");
  h.setStream(async function* () { yield chunk("Partial"); throw new Error("current failure"); });
  await h.send(); assert.equal(h.sessions().messages.at(-1)?.status, "failed");
  assert.equal(h.sessions().loading, false); assert.equal(h.syncCalls, 0);
});

for (const engine of engines) {
  for (const stopped of [false, true]) {
    for (const failure of [false, true]) {
      test(`${engine}: ${stopped ? "Stop then resend" : "direct Regenerate"} owns old cancellation before late ${failure ? "error" : "EOF"}`, async () => {
        const h = host(engine); const ready = deferred<void>(); const finish = deferred<void>();
        h.setStream(async function* () { yield chunk("Old partial"); h.flushFrames(); ready.resolve(); await finish.promise;
          if (failure) throw new Error("old request failed"); });
        const first = h.send(); await ready.promise;
        const oldController = h.sessions().abortControllerRef.current!;
        if (stopped) h.sessions().handleStopStreaming();
        const nextReady = deferred<void>(); const nextFinish = deferred<void>();
        h.setStream(async function* () { yield chunk("New answer"); h.flushFrames(); nextReady.resolve(); await nextFinish.promise; yield chunk("", true); });
        const second = h.send(); await nextReady.promise;
        const immediatelyCancelled = h.sessions().messages[1].status;
        finish.resolve(); await first;
        const between = h.sessions();
        nextFinish.resolve(); await second;
        assert.equal(oldController.signal.aborted, true); assert.equal(immediatelyCancelled, "cancelled");
        assert.equal(between.messages[1].status, "cancelled"); assert.equal(between.messages[1].content, "Old partial");
        assert.equal(between.messages[3].status, "streaming"); assert.equal(between.loading, true);
        assert.equal(h.sessions().messages[3].status, "completed"); assert.equal(h.sessions().messages[3].content, "New answer");
        assert.equal(h.syncCalls, 0);
      });
    }
  }
}
test("an old queued stream updater rechecks ownership after session selection", async () => {
  const h = host(); const ready = deferred<void>(); const finish = deferred<void>();
  h.setStream(async function* () {
    yield chunk("Old partial"); h.holdNextMessageUpdate(); h.flushFrames(); ready.resolve(); await finish.promise;
  });
  const sending = h.send(); await ready.promise;
  h.setHistory([{ role: "user", content: "Saved prompt", timestamp: "1" }, { role: "assistant", content: "Saved answer", timestamp: "2" }]);
  await h.sessions().handleSelectSession("other-session");
  h.flushHeldUpdates(); finish.resolve(); await sending;
  assert.equal(h.sessions().messages[1].content, "Saved answer"); assert.equal(h.sessions().messages[1].status, "completed");
});
test("the production service-switch effect stops the old response and releases loading", async () => {
  const h = host("dify_chat"); const ready = deferred<void>(); const finish = deferred<void>();
  h.setStream(async function* () { yield chunk("Old partial"); h.flushFrames(); ready.resolve(); await finish.promise; throw new Error("late error"); });
  const sending = h.send(); await ready.promise; await h.commitSessionEffects();
  const controller = h.sessions().abortControllerRef.current!;
  h.options.serviceId = "other-service"; h.rerender(); await h.commitSessionEffects();
  const switched = h.sessions(); finish.resolve(); await sending;
  assert.equal(controller.signal.aborted, true); assert.equal(switched.loading, false);
  assert.equal(switched.messages.length, 0); assert.equal(h.sessions().messages.length, 0); assert.equal(h.titleReads, 0);
});
test("the production unmount cleanup invalidates the old request before its late failure", async () => {
  const h = host("dify_chat"); const ready = deferred<void>(); const finish = deferred<void>();
  h.setStream(async function* () { yield chunk("Old partial"); h.flushFrames(); ready.resolve(); await finish.promise; throw new Error("late error"); });
  const sending = h.send(); await ready.promise; await h.commitSessionEffects();
  const controller = h.sessions().abortControllerRef.current!;
  h.unmount(); const before = h.sessions().messages;
  finish.resolve(); await sending;
  assert.equal(controller.signal.aborted, true); assert.equal(h.sessions().messages, before); assert.equal(h.titleReads, 0);
});

for (const engine of ["custom", "dify_chat", "langgraph"]) {
  test(`PG02 ${engine}: local Stop preserves received zero and partial usage without promoting the text estimate`, async () => {
    const h = host(engine); const ready = deferred<void>(); const finish = deferred<void>();
    h.setStream(async function* () {
      yield { ...chunk("Partial answer"), metadata: { usage: { input_tokens: 0, output_tokens: 7 } } };
      h.flushFrames(); ready.resolve(); await finish.promise;
      yield { ...chunk("", true), metadata: { usage: { input_tokens: 100, output_tokens: 100, total_tokens: 200 } } };
    });
    const running = h.send(); await ready.promise; h.sessions().handleStopStreaming();
    const stats = h.sessions().messages.at(-1)!.stats!;
    assert.equal(stats.inputTokens, 0); assert.equal(stats.outputTokens, 7); assert.equal(stats.totalTokens, undefined);
    assert.equal(stats.tokenUsageSource, "reported"); assert.equal(typeof stats.textTokenEstimate?.totalTokens, "number");
    finish.resolve(); await running;
    assert.equal(h.sessions().messages.at(-1)?.status, "cancelled");
    assert.equal(h.sessions().messages.at(-1)?.stats?.outputTokens, 7);
  });
}

for (const kind of ["metadata", "updates"]) {
  test(`PG02 raw LangGraph ${kind} usage-only receipt is flushed before Stop`, async () => {
    const h = host("langgraph"); const ready = deferred<void>(); const finish = deferred<void>();
    h.setStream(async function* () {
      yield { event: "messages/complete", data: [{ id: "m1", type: "ai", content: "Answer" }] };
      h.flushFrames();
      const usage = { input_tokens: 0, output_tokens: 7, total_tokens: 7 };
      yield kind === "metadata" ? { event: "metadata", data: { usage } } :
        { event: "updates", data: { agent: { messages: [{ id: "m1", type: "ai", content: "Answer", usage_metadata: usage }] } } };
      h.flushFrames(); ready.resolve(); await finish.promise;
    });
    const sending = h.send(); await ready.promise; h.sessions().handleStopStreaming();
    const stats = h.sessions().messages.at(-1)?.stats; finish.resolve(); await sending;
    assert.equal(h.sessions().messages.at(-1)?.status, "cancelled"); assert.equal(stats?.totalTokens, 7);
    assert.equal(stats?.inputTokens, 0); assert.equal(stats?.tokenUsageSource, "reported");
  });
}

test("saved reported routing restores separately from requested settings", async () => {
 const h = host("langgraph");
 h.setHistory([{ message_id: "saved-answer", role:"assistant", content:"Saved fallback answer", created_at:"2026-10-12T00:00:00Z", metadata: {stats:{reported_routing:{source:"upstream_reported",status:"selected",provider_id:"saved-provider",model_id:"saved-model",api_key:"must-not-restore"}}} }]);
 await h.sessions().handleSelectSession("session");
 const message = h.sessions().messages.at(-1)!;
 assert.equal(message.stats?.reportedRouting?.model_id,"saved-model");
 assert.ok(!JSON.stringify(message).includes("must-not-restore"));
});
