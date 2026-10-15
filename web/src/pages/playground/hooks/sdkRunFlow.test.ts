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

const id = "12345678-1234-4234-8234-123456789abc";
const user = () => ({ user_id: "owner", permissions: [], roles: [], tier: "free" });
const chunk = (content = "", metadata = {}, is_final = false): StreamChunk => ({ request_id: "request-is-not-a-run", chunk_index: 0,
  event_type: is_final ? "stream_end" : "text_delta", content: { type: "text", data: content }, is_final, metadata });
const noop = () => {};
const sdk = (value: unknown) => value === "openai_agents" || value === "claude_agent_sdk";
function load(filename: string, modules: Record<string, unknown>, globals: Record<string, unknown> = {}) {
  const source = readFileSync(new URL(filename, import.meta.url), "utf8");
  const exports: Record<string, (...args: unknown[]) => unknown> = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
      Error, AbortController, performance, Date, ...globals });
  return exports;
}
function streamHost(engine = "openai_agents") {
  let messages: ChatMessage[] = [];
  const auth = { token: "test-only-token", user: user() };
  let invokes = 0;
  let clock = 0;
  let stream: () => AsyncGenerator<StreamChunk> = async function* () {};
  const ref = <T>(current: T) => ({ current });
  const opts = {
    serviceId: "service", activeService: { service_id: "service", metadata: { adapter_type: engine } },
    sessionEnabled: true, activeSessionId: "session", localTitles: { session: "Existing title" }, sessions: [], messages: [], loading: false,
    setMessages: (next: ChatMessage[] | ((current: ChatMessage[]) => ChatMessage[])) => { messages = typeof next === "function" ? next(messages) : next; },
    setLoading: noop, setActiveSessionId: noop, setLocalTitles: noop, invalidatePendingHistoryLoad: noop,
    refreshSessions: async () => {}, messagesRef: ref(messages), abortControllerRef: ref<AbortController | null>(null),
    currentRequestIdRef: ref(0), currentRequestSessionRef: ref(null), loadingHistorySessionRef: ref(null),
    sessionThreadIdRef: ref({}), pendingSessionInitRef: ref(null), pendingThreadRef: ref(null), interactionStartedRef: ref(false), stickToBottomRef: ref(false),
    t: (key: string) => key,
  };
  const modules = {
    react: { useCallback: (fn: unknown) => fn, useMemo: (fn: () => unknown) => fn(), useRef: ref },
    "@/features/chat/stream/sdkRun": run, "@/features/chat/stream/sdkLifecycle": lifecycle,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/sdkAgentConfig": { isNativeAgentSdk: sdk },
    "../utils/difyConversation": { prepareDifyConversation: async () => ({ accept: async () => {} }) },
    "@/api/gateway": { invokeService: async () => { invokes++; throw new Error("unexpected replay"); } },
    "@/api/sessions": { getSession: async () => ({}), updateSession: async () => ({}), addSessionMessage: async () => ({}) },
    "@/lib/sse": { sseFetch: () => stream(), streamChunkToAGUIEvent: () => ({ event: "text_delta" }) },
    "@/hooks/useAgentTimeline": { useAgentTimeline: () => ({ state: { steps: [] }, processEvent: noop, reset: noop }) },
    "@/lib/utils": { getPlaygroundErrorMessage: () => "Interrupted fixture stream" },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/features/chat/stream": { ...reducer, ...normalizers },
    "@/features/chat/telemetry": { finishChatStreamTrace: noop, markChatStreamFirstToken: noop, startChatStreamTrace: () => ({}) },
    "@/lib/api": { api: { get: async () => ({ data: {} }), patch: async () => ({}) } }, "../utils/requestSnapshot": requestSnapshot, "../utils/langgraph": langgraph,
    "@/features/chat/stream/tokenObservation": tokenObservation,
  };
  const exported = load("./usePlaygroundStream.ts", modules, { requestAnimationFrame: () => 1, cancelAnimationFrame: noop, performance: { now: () => clock } });
  const hook = exported.usePlaygroundStream(opts) as { handleSend: (input: unknown) => Promise<void> };
  return { auth, opts, get messages() { return messages; }, get invokes() { return invokes; },
    setNow(value: number) { clock = value; }, setStream(value: typeof stream) { stream = value; }, send: () => hook.handleSend([{ type: "text", data: "Hello" }]) };
}
for (const engine of ["openai_agents", "claude_agent_sdk"]) {
  test(`${engine} existing stream exposes admission ID before first output and never estimates unavailable usage`, async () => {
    const h = streamHost(engine); let initial: ChatMessage | undefined;
    h.setStream(async function* () {
      yield chunk("", { sdk_framework: engine, assistant_run_id: id });
      initial = h.messages.at(-1);
      yield { ...chunk("Hello from SDK"), chunk_index: 1 };
      yield { ...chunk("", { usage: { sdk_usage: { state: "unavailable" } } }, true), chunk_index: 2 };
    });
    await h.send();
    assert.deepEqual(initial?.sdkRun, { runId: id, engine, sessionId: "session" });
    assert.equal(initial?.content, ""); assert.equal(initial?.isStreaming, true);
    const message = h.messages.at(-1)!; assert.equal(message.status, "completed");
    assert.equal(message.stats?.inputTokens, undefined); assert.equal(message.stats?.totalTokens, undefined);
    assert.equal(h.invokes, 0); assert.equal(message.sdkAuthorityScope, sdkAuthorityScope(h.auth.user));
  });
}
test("existing stream keeps measured zero and terminal-only genuine identity", async () => {
  const h = streamHost(); h.setStream(async function* () {
    yield chunk("Answer");
    yield { ...chunk("", { usage: { input_tokens: 0, output_tokens: 0, sdk_usage: { state: "reported", framework: "openai_agents", assistant_run_id: id } } }, true), chunk_index: 1 };
  }); await h.send(); assert.equal(h.messages.at(-1)?.sdkRun?.runId, id); assert.equal(h.messages.at(-1)?.stats?.totalTokens, 0);
});
test("interrupted SDK stream keeps genuine ID and partial answer with no sync replay", async () => {
  const h = streamHost(); h.setStream(async function* () {
    yield chunk("Partial answer", { sdk_framework: "openai_agents", assistant_run_id: id });
    throw new Error("lost connection");
  }); await h.send(); const message = h.messages.at(-1)!;
  assert.equal(message.status, "failed"); assert.equal(message.sdkRun?.runId, id);
  assert.match(message.content, /Partial answer/); assert.equal(h.invokes, 0);
});
test("late SDK stream failure cannot overwrite a newer view or another account", async () => {
  for (const change of ["view", "account", "token"]) {
    const h = streamHost(); h.setStream(async function* () {
      yield chunk("Partial", { sdk_framework: "openai_agents", assistant_run_id: id });
      h.opts.setMessages([{ id: "new-view", role: "assistant", content: "New view", status: "completed" }]);
      if (change === "view") h.opts.currentRequestIdRef.current++;
      if (change === "account") h.auth.user = { ...user(), user_id: "other" };
      if (change === "token") h.auth.token = "replacement-test-token";
      throw new Error("late failure");
    }); await h.send(); assert.equal(h.messages.length, 1); assert.equal(h.messages[0].id, "new-view"); assert.equal(h.messages[0].content, "New view");
  }
});
test("an interrupted SDK stream without genuine metadata never fabricates a cancellable run", async () => {
  const h = streamHost(); h.setStream(async function* () { yield chunk("Partial"); });
  await h.send(); assert.equal(h.messages.at(-1)?.sdkRun, undefined); assert.equal(h.messages.at(-1)?.status, "failed");
  assert.notEqual(h.messages.at(-1)?.completionConfirmed, true); assert.equal(h.invokes, 0);
});
test("non-SDK stream separates text estimates from unknown usage", async () => {
  const h = streamHost("custom"); h.setStream(async function* () { yield chunk("Answer"); yield { ...chunk("", {}, true), chunk_index: 1 }; });
  await h.send(); assert.equal(h.messages.at(-1)?.stats?.totalTokens, undefined); assert.equal(typeof h.messages.at(-1)?.stats?.textTokenEstimate?.totalTokens, "number"); assert.equal(h.messages.at(-1)?.sdkRun, undefined);
});

function historyHost() {
  const slots: unknown[] = []; let cursor = 0;
  const auth = { token: "test-only-token", user: user() };
  let history: () => Promise<unknown[]> = async () => [];
  const options = { serviceId: "service", services: [{ service_id: "service", metadata: { adapter_type: "openai_agents" } }] };
  const react = {
    useState(initial: unknown) { const n = cursor++; if (!(n in slots)) slots[n] = initial;
      return [slots[n], (value: unknown) => { slots[n] = typeof value === "function" ? value(slots[n]) : value; }]; },
    useRef(initial: unknown) { const n = cursor++; return slots[n] ?? (slots[n] = { current: initial }); },
    useEffect: noop, useCallback: (fn: unknown) => fn,
  };
  const modules = { react, "@/features/chat/stream/sdkRun": run, "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/sdkAgentConfig": { isNativeAgentSdk: sdk }, "@/api/sessions": { getSession: async () => ({}), getSessionHistory: () => history() },
    "@/store/useAuthStore": { useAuthStore: { getState: () => auth } },
    "@/store/useAppStore": { useAppStore: () => ({ activeSessionId: "session", setActiveSessionId: noop, setLocalTitles: noop }) },
    "@/features/chat/telemetry": { trackChatHistoryRestored: noop }, "@/lib/api": { api: { get: async () => ({ data: {} }), patch: async () => ({}) } }, "../utils/requestSnapshot": requestSnapshot, "../utils/langgraph": langgraph, "@/features/chat/stream/tokenObservation": tokenObservation };
  const exports = load("./usePlaygroundSessions.ts", modules, { setTimeout: () => 1 });
  type Hook = { messages: ChatMessage[]; handleSelectSession: (id: string) => Promise<void>; handleStopStreaming: () => void; setMessages: (value: ChatMessage[]) => void };
  return { auth, setHistory(value: typeof history) { history = value; }, render() { cursor = 0; return exports.usePlaygroundSessions(options) as Hook; } };
}
test("existing history restores scoped SDK identity and unknown usage without changing legacy history", async () => {
  const h = historyHost(); h.setHistory(async () => [
    { role: "assistant", timestamp: "1", content: "SDK", metadata: { stats: { output_tokens: 5, sdk_usage: { framework: "openai_agents", assistant_run_id: id, state: "unavailable" } } } },
    { role: "assistant", timestamp: "2", content: "Legacy", metadata: { stats: { total_tokens: 50 } } },
  ]); await h.render().handleSelectSession("session"); const messages = h.render().messages;
  assert.deepEqual(messages[0].sdkRun, { runId: id, engine: "openai_agents", sessionId: "session" });
  assert.equal(messages[0].sdkAuthorityScope, sdkAuthorityScope(h.auth.user));
  assert.equal(messages[0].stats?.totalTokens, undefined); assert.equal(messages[1].sdkRun, undefined); assert.equal(messages[1].stats?.totalTokens, 50);
});
test("revoked source and an account-switched history response cannot create run controls", async () => {
  const h = historyHost(); const row = { role: "assistant", timestamp: "1", content: "SDK", metadata: { source_access_revoked: true,
    stats: { sdk_usage: { framework: "openai_agents", assistant_run_id: id } } } };
  h.setHistory(async () => [row]); await h.render().handleSelectSession("session"); assert.equal(h.render().messages[0].sdkRun, undefined);
  h.setHistory(async () => { h.auth.user = { ...user(), user_id: "other" }; return [row]; });
  await h.render().handleSelectSession("session"); assert.equal(h.render().messages.length, 0);
});
test("existing SDK transport-stop marks the local message interrupted while preserving genuine identity", () => {
  const h = historyHost(); h.render().setMessages([{ role: "assistant", content: "Partial", isStreaming: true, status: "streaming",
    sdkRun: { runId: id, engine: "openai_agents", sessionId: "session" } }]);
  h.render().handleStopStreaming(); const message = h.render().messages[0];
  assert.equal(message.status, "cancelled"); assert.notEqual(message.completionConfirmed, true); assert.equal(message.isStreaming, false); assert.equal(message.sdkRun?.runId, id);
});

test("metadata-only admission survives interruption even when no provider output is received", async () => {
  const h = streamHost(); h.setStream(async function* () {
    yield chunk("", { sdk_framework: "openai_agents", assistant_run_id: id });
    throw new Error("interrupted before first token");
  }); await h.send(); assert.equal(h.messages.at(-1)?.sdkRun?.runId, id);
  assert.equal(h.messages.at(-1)?.status, "failed");
  assert.notEqual(h.messages.at(-1)?.completionConfirmed, true); assert.equal(h.invokes, 0);
});

for (const engine of ["openai_agents", "claude_agent_sdk"]) {
  for (const measured of [false, true]) {
    test(`${engine} genuine empty completion remains completed with ${measured ? "zero" : "unavailable"} usage`, async () => {
      const h = streamHost(engine); h.setStream(async function* () {
        yield chunk("", { sdk_framework: engine, assistant_run_id: id });
        const usage = measured ? { input_tokens: 0, output_tokens: 0, total_tokens: 0, sdk_usage: { state: "reported" } } :
          { sdk_usage: { state: "unavailable" } };
        yield { ...chunk("", { usage }, true), chunk_index: 1 };
      }); await h.send(); const message = h.messages.at(-1)!;
      assert.equal(message.status, "completed"); assert.equal(message.content, "");
      assert.equal(message.completionConfirmed, true);
      assert.equal(message.isStreaming, false); assert.equal(message.sdkRun?.runId, id);
      assert.equal(message.stats?.totalTokens, measured ? 0 : undefined); assert.equal(h.invokes, 0);
    });
  }
}
test("genuine empty SDK admission without terminal is still failed and never replayed", async () => {
  const h = streamHost(); h.setStream(async function* () {
    yield chunk("", { sdk_framework: "openai_agents", assistant_run_id: id });
  }); await h.send(); assert.equal(h.messages.at(-1)?.status, "failed");
  assert.notEqual(h.messages.at(-1)?.completionConfirmed, true);
  assert.equal(h.messages.at(-1)?.sdkRun?.runId, id); assert.equal(h.invokes, 0);
});
test("SDK error terminal with genuine identity is never promoted to successful empty completion", async () => {
  const h = streamHost(); h.setStream(async function* () {
    yield chunk("", { sdk_framework: "openai_agents", assistant_run_id: id });
    yield { ...chunk("Provider failed", {}, true), event_type: "error", chunk_index: 1 };
  }); await h.send(); assert.equal(h.messages.at(-1)?.status, "failed");
  assert.notEqual(h.messages.at(-1)?.completionConfirmed, true); assert.equal(h.invokes, 0);
});
test("SDK admission does not start first-token timing while legacy empty deltas keep their semantics", async () => {
  for (const engine of ["openai_agents", "custom"]) {
    const h = streamHost(engine); h.setStream(async function* () {
      h.setNow(10); yield chunk("", engine === "openai_agents" ? { sdk_framework: engine, assistant_run_id: id } : {});
      h.setNow(80); yield { ...chunk("Answer"), chunk_index: 1 };
      h.setNow(100); yield { ...chunk("", {}, true), chunk_index: 2 };
    }); await h.send(); assert.equal(h.messages.at(-1)?.stats?.firstTokenMs, engine === "openai_agents" ? 80 : 10);
  }
});
test("empty SDK history restores identity, local interruption and unavailable usage", async () => {
  for (const [local, expected] of [["interrupted", "cancelled"], ["failed", "failed"], ["completed", "completed"]]) {
    const h = historyHost(); h.setHistory(async () => [{ role: "assistant", timestamp: "1", content: "",
      metadata: { sdk_framework: "openai_agents", assistant_run_id: id, sdk_stream_status: local,
        stats: { sdk_usage: { state: "unavailable" } } } }]);
    await h.render().handleSelectSession("session"); const message = h.render().messages[0];
    assert.equal(message.status, expected); assert.equal(message.content, "");
    assert.equal(message.completionConfirmed, local === "completed");
    assert.equal(message.sdkRun?.runId, id); assert.equal(message.stats?.totalTokens, undefined);
  }
});
test("a local SDK history outcome without genuine SDK identity cannot change legacy history status", async () => {
  const h = historyHost(); h.setHistory(async () => [{ role: "assistant", timestamp: "1", content: "Legacy",
    metadata: { sdk_stream_status: "interrupted", stats: { total_tokens: 50 } } }]);
  await h.render().handleSelectSession("session"); const message = h.render().messages[0];
  assert.equal(message.status, "completed"); assert.equal(message.sdkRun, undefined); assert.equal(message.stats?.totalTokens, 50);
});

test("PG02 production history restores unknown, explicit zero, local estimates and Gateway field provenance", async () => {
  const h = historyHost(); h.setHistory(async () => [
    { role: "assistant", timestamp: "a", content: "No receipt" },
    { role: "assistant", timestamp: "b", content: "Old zero", metadata: { stats: { input_tokens: 0, output_tokens: 0, total_tokens: 0 } } },
    { role: "assistant", timestamp: "c", content: "New partial", metadata: { stats: { input_tokens: 9,
      token_usage_source: "reported", text_token_estimate: { input_tokens: 2, output_tokens: 3, total_tokens: 5 } } } },
    { role: "assistant", timestamp: "d", content: "New Gateway", metadata: { stats: { input_tokens: 0, output_tokens: 3, total_tokens: 3,
      token_provenance: { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" } } } },
  ]); await h.render().handleSelectSession("session"); const messages = h.render().messages;
  assert.equal(messages[0].stats?.tokenUsageSource, "unknown"); assert.equal(messages[0].stats?.totalTokens, undefined);
  assert.equal(messages[1].stats?.tokenUsageSource, "unknown"); assert.equal(messages[1].stats?.totalTokens, 0);
  assert.equal(messages[2].stats?.tokenUsageSource, "reported"); assert.equal(messages[2].stats?.totalTokens, undefined);
  assert.equal(messages[2].stats?.inputTokens, 9); assert.equal(messages[2].stats?.textTokenEstimate?.totalTokens, 5);
  assert.equal(messages[3].stats?.tokenUsageSource, "reported"); assert.equal(messages[3].stats?.inputTokens, 0);
  assert.equal(messages[3].stats?.tokenProvenance?.input_tokens, "upstream");
  assert.equal(messages[3].stats?.tokenProvenance?.output_tokens, "estimated");
  assert.equal(messages[3].stats?.tokenProvenance?.total_tokens, "derived");
});
