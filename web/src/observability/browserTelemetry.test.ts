import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import test from "node:test";

const listeners = new Set<(state: typeof identity, previous: typeof identity) => void>();
let identity = { csrfToken: "SYNTHETIC-CSRF", token: "SYNTHETIC-A", isAuthenticated: true, user: { user_id: "user-a", roles: ["user"], permissions: [], tier: "normal" } };
let endpoint = "/api/v1/telemetry/browser";
const hub = { getState: () => identity, subscribe: (listener: (state: typeof identity, previous: typeof identity) => void) => { listeners.add(listener); return () => listeners.delete(listener); } };
Object.assign(globalThis, { __RUM_TEST_AUTH__: hub, __RUM_TEST_ENDPOINT__: () => endpoint });
const moduleUrl = new URL("./browserTelemetry.ts", import.meta.url).href;
const hooks = registerHooks({ resolve(name, context, next) {
  if (context.parentURL === moduleUrl) {
    if (name === "@/store/useAuthStore") return { url: "data:text/javascript,export const useAuthStore = globalThis.__RUM_TEST_AUTH__;", shortCircuit: true };
    if (name === "@/config/runtime") return { url: "data:text/javascript,export const getTelemetryEndpoint = () => globalThis.__RUM_TEST_ENDPOINT__();", shortCircuit: true };
    if (name === "@/lib/sdkAuthorityScope") return next(new URL("../lib/sdkAuthorityScope.ts", moduleUrl).href, context);
    if (name === "./telemetryCore") return next(new URL("./telemetryCore.ts", moduleUrl).href, context);
  }
  return next(name, context);
} });
const { initBrowserTelemetry, reportBrowserError, recordChatPerformance, browserTraceparent } = await import("./browserTelemetry.ts");
hooks.deregister();

function fixture(t: import("node:test").TestContext) {
  const original = Object.fromEntries(["window", "document", "location", "PerformanceObserver"].map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  const requests: Array<{ url: string; init: RequestInit }> = [];
  const observerCallbacks = new Map<string, (entries: PerformanceEntry[]) => void>();
  class Observer {
    static supportedEntryTypes = ["event", "layout-shift", "largest-contentful-paint"];
    private callback: (list: { getEntries: () => PerformanceEntry[] }) => void;
    constructor(callback: (list: { getEntries: () => PerformanceEntry[] }) => void) { this.callback = callback; }
    observe(options: { type: string }) { observerCallbacks.set(options.type, entries => this.callback({ getEntries: () => entries })); }
    takeRecords(): PerformanceEntry[] { return []; }
    disconnect() {}
  }
  const document = Object.assign(new EventTarget(), { visibilityState: "visible" });
  const window = Object.assign(new EventTarget(), {
    fetch: (async (input, init = {}) => { requests.push({ url: String(input), init }); return new Response(JSON.stringify(String(input).endsWith("/config") ? { enabled: true, sampled: true, sessionId: "0123456789abcdef0123456789abcdef", sampleRate: 0.01, ttlSeconds: 900 } : {})); }) as typeof fetch,
    setInterval: () => 1, clearInterval() {},
  });
  const location = { origin: "https://console.example", pathname: "/knowledge/PRIVATE-ID", search: "?token=SECRET" };
  for (const [key, value] of Object.entries({ window, document, location, PerformanceObserver: Observer })) Object.defineProperty(globalThis, key, { configurable: true, value });
  const close = initBrowserTelemetry();
  t.after(() => { close(); listeners.clear(); endpoint = "/api/v1/telemetry/browser"; for (const [name, descriptor] of Object.entries(original)) { if (descriptor) Object.defineProperty(globalThis, name, descriptor); else Reflect.deleteProperty(globalThis, name); } });
  return { requests, window, document, observerCallbacks, close };
}
async function settle() { for (let i = 0; i < 12; i++) await Promise.resolve(); }

test("actual boot wiring instruments fetch and exports scrubbed vitals/errors/legacy timing without prompt or tenant attributes", async t => {
  const h = fixture(t); await settle();
  assert.match(browserTraceparent("/api/v1/services")!, /^00-/);
  await h.window.fetch("/api/v1/services?token=PRIVATE");
  const request = h.requests.find(request => request.url.startsWith("/api/v1/services"))!;
  assert.match(new Headers(request.init.headers).get("traceparent")!, /^00-/);
  const time = performance.now() + 100;
  h.observerCallbacks.get("largest-contentful-paint")!([{ startTime: time } as PerformanceEntry]);
  h.observerCallbacks.get("layout-shift")!([{ startTime: time, value: 0.1, hadRecentInput: false } as unknown as PerformanceEntry]);
  h.observerCallbacks.get("event")!([{ startTime: time, interactionId: 7, duration: 140 } as unknown as PerformanceEntry]);
  const error = new TypeError("PRIVATE PROMPT"); error.stack = "TypeError: PRIVATE PROMPT\n at https://console.example/assets/index-abcd1234.js?token=SECRET:12:13";
  reportBrowserError(error, "RenderError");
  recordChatPerformance("chat.stream.first_token", { surface: "assistant", firstTokenMs: 250, prompt: "PRIVATE PROMPT", tenant_id: "FORGED" });
  h.document.visibilityState = "hidden"; h.document.dispatchEvent(new Event("visibilitychange")); await settle();
  const sent = h.requests.filter(request => request.init.method === "POST"); assert.equal(sent.length, 1);
  assert.equal(new Headers(sent[0].init.headers).get("Authorization"),null);
  assert.equal(new Headers(sent[0].init.headers).get("X-CSRF-Token"),"SYNTHETIC-CSRF");
  const body = String(sent[0].init.body); assert.doesNotMatch(body, /PRIVATE|SECRET|FORGED|prompt|tenant_id|user_id|query/);
  const records = JSON.parse(body).records;
  assert.ok(records.some((record: { name: string }) => record.name === "LCP"));
  assert.ok(records.some((record: { name: string }) => record.name === "INP"));
  assert.ok(records.some((record: { name: string }) => record.name === "CLS"));
  assert.ok(records.some((record: { name: string }) => record.name === "RenderError"));
  assert.ok(records.some((record: { name: string }) => record.name === "chat_first_token"));
});

test("default off and external telemetry config install no observers, trace or network requests", async t => {
  endpoint = "https://outside.example/collect";
  const h = fixture(t); await settle();
  assert.equal(h.requests.length, 0); assert.equal(h.observerCallbacks.size, 0);
  assert.equal(browserTraceparent("/api/v1/services"), undefined);
});

test("authentication change clears queued errors before a replacement authority can flush", async t => {
  const h = fixture(t); await settle(); reportBrowserError(new Error("PRIVATE"));
  const previous = identity; identity = { ...identity, token: "SYNTHETIC-B", user: { ...identity.user, user_id: "user-b" } };
  for (const callback of listeners) callback(identity, previous);
  await settle(); h.document.visibilityState = "hidden"; h.document.dispatchEvent(new Event("visibilitychange")); await settle();
  const records = h.requests.filter(request => request.init.method === "POST").flatMap(request => JSON.parse(String(request.init.body)).records);
  assert.ok(!records.some((record: { kind: string }) => record.kind === "error"));
});
