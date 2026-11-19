import { getTelemetryEndpoint } from "@/config/runtime";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { createTelemetryClient, routeName, scrubError, VitalAccumulator, TELEMETRY_PATH } from "./telemetryCore";

let active: ReturnType<typeof createTelemetryClient> | undefined;
let dispose: (() => void) | undefined;
export function browserTraceparent(url: string, base?: string): string | undefined {
  if (!active || typeof location === "undefined") return;
  try { return active.traceparent(new URL(url, new URL(base || location.origin, location.origin)).href); } catch { return; }
}
export function reportBrowserError(error: unknown, kind?: "UnhandledRejection" | "RenderError") {
  if (typeof location === "undefined") return;
  try { if (!active?.enabled()) return; active.record({ kind: "error", route: routeName(location.pathname), ...scrubError(error, location.origin, kind) }); } catch { /* Diagnostic errors never recurse into the product flow. */ }
}
export function recordChatPerformance(event: string, payload: Record<string, unknown>) {
  const route = payload.surface === "assistant" ? "/assistant" : payload.surface === "playground" ? "/playground" : undefined;
  if (!route) return;
  // Nothing else from legacy free-form attributes reaches the network.
  if (event === "chat.stream.first_token" && typeof payload.firstTokenMs === "number") active?.record({ kind: "metric", name: "chat_first_token", route, value: payload.firstTokenMs });
  if (event === "chat.stream.finished" && typeof payload.durationMs === "number") active?.record({ kind: "metric", name: "chat_duration", route, value: payload.durationMs });
}

/** Install once before rendering. All observers are bounded and the exporter uses the original fetch. */
export function initBrowserTelemetry(): () => void {
  if (dispose) return dispose;
  // Reuse the existing deployment opt-in, but accept only the owned same-origin collector.
  const configured = getTelemetryEndpoint();
  try {
    if (!configured) return () => {};
    const endpoint = new URL(configured, location.origin);
    if (endpoint.origin !== location.origin || endpoint.pathname !== TELEMETRY_PATH || endpoint.search || endpoint.hash || endpoint.username || endpoint.password) return () => {};
  } catch { return () => {}; }
  const nativeFetch = window.fetch.bind(window);
  let generation = 0;
  const authority = () => {
    const state = useAuthStore.getState();
    return { key: `${generation}:${sdkAuthorityScope(state.user)}`, token: state.token, authenticated: state.isAuthenticated && Boolean(state.token && state.user), sessionType: "cookie" as const, csrfToken: state.csrfToken };
  };
  const client = createTelemetryClient({ origin: location.origin, authority, fetch: nativeFetch }); active = client;
  let vitals = new VitalAccumulator();
  let authorityStarted = performance.now();
  let interactionBase = (performance as Performance & { interactionCount?: number }).interactionCount ?? 0;
  let lcpCutoff = Infinity;
  const emitted = new Map<string, number>();
  const unsubscribe = useAuthStore.subscribe((state, previous) => {
    if (state.token !== previous.token || state.csrfToken !== previous.csrfToken || state.isAuthenticated !== previous.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
      generation++; client.reset(); vitals = new VitalAccumulator(); lcpCutoff = Infinity; emitted.clear();
      authorityStarted = performance.now(); interactionBase = (performance as Performance & { interactionCount?: number }).interactionCount ?? 0;
      void configureObservers();
    }
  });
  const wrappedFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const trace = client.traceparent(url);
    if (!trace) return nativeFetch(input, init);
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    if (headers.has("traceparent")) return nativeFetch(input, init);
    headers.set("traceparent", trace);
    const requestGeneration = generation;
    const start = performance.now();
    const response = await nativeFetch(input, { ...init, headers });
    if (requestGeneration === generation) client.record({ kind: "metric", name: "api_headers", route: routeName(location.pathname), value: performance.now() - start, span_id: trace.split("-")[2], http_status: response.status });
    return response;
  };
  window.fetch = wrappedFetch;
  const observers: PerformanceObserver[] = [];
  const drainers: Array<() => void> = [];
  const supported = new Set(typeof PerformanceObserver === "undefined" ? [] : PerformanceObserver.supportedEntryTypes ?? []);
  const observe = (type: string, consume: (entry: PerformanceEntry) => void, extra = {}) => {
    if (!supported.has(type)) return;
    try {
      const observer = new PerformanceObserver(list => { if (client.enabled()) list.getEntries().filter(entry => entry.startTime >= authorityStarted).forEach(consume); });
      observer.observe({ type, buffered: true, ...extra }); observers.push(observer);
      drainers.push(() => { if (client.enabled()) observer.takeRecords().filter(entry => entry.startTime >= authorityStarted).forEach(consume); });
      return observer;
    } catch { /* Unsupported browser versions retain the product flow. */ }
  };
  let lcpObserver: PerformanceObserver | undefined;
  const installObservers = () => {
    observers.forEach(observer => observer.disconnect()); observers.length = 0; drainers.length = 0;
    vitals = new VitalAccumulator(); emitted.clear();
    // buffered entries recover early vitals that occurred while the authenticated sampling config was in flight.
    lcpObserver = observe("largest-contentful-paint", entry => { if (entry.startTime <= lcpCutoff) vitals.addLcp(entry.startTime); });
    observe("layout-shift", entry => { const shift = entry as PerformanceEntry & { value: number; hadRecentInput: boolean }; vitals.addLayoutShift(shift.startTime, shift.value, shift.hadRecentInput); });
    observe("event", entry => { const event = entry as PerformanceEventTiming; vitals.addInteraction(event.interactionId, event.duration); }, { durationThreshold: 16 });
  };
  async function configureObservers() {
    const expected = generation;
    await client.configure();
    if (expected === generation && client.enabled()) installObservers();
  }
  void configureObservers();
  const finalizeLcp = () => {
    lcpCutoff = Math.min(lcpCutoff, performance.now());
    if (client.enabled()) lcpObserver?.takeRecords().filter(entry => entry.startTime >= authorityStarted && entry.startTime <= lcpCutoff).forEach(entry => vitals.addLcp(entry.startTime));
  };
  const flushVitals = () => {
    if (!client.enabled()) return;
    drainers.forEach(drain => drain());
    const record = (name: "INP" | "LCP" | "CLS", value: number, quality?: "native" | "observed") => {
      if (emitted.get(name) === value) return;
      emitted.set(name, value); client.record({ kind: "metric", name, route: routeName(location.pathname), value, quality });
    };
    if (vitals.lcp) record("LCP", vitals.lcp, "native");
    if (supported.has("layout-shift")) record("CLS", vitals.cls, "native");
    const nativeCount = (performance as Performance & { interactionCount?: number }).interactionCount;
    const inp = vitals.inp(nativeCount === undefined ? undefined : Math.max(0, nativeCount - interactionBase));
    if (inp) record("INP", inp.value, inp.quality);
    void client.flush();
  };
  const visibility = () => { if (document.visibilityState === "hidden") { finalizeLcp(); flushVitals(); } };
  const error = (event: ErrorEvent) => reportBrowserError(event.error);
  const rejection = (event: PromiseRejectionEvent) => reportBrowserError(event.reason, "UnhandledRejection");
  const restored = (event: PageTransitionEvent) => { if (event.persisted) { vitals = new VitalAccumulator(); lcpCutoff = Infinity; emitted.clear(); authorityStarted = performance.now(); void configureObservers(); } };
  document.addEventListener("visibilitychange", visibility);
  window.addEventListener("pagehide", flushVitals);
  window.addEventListener("pageshow", restored);
  window.addEventListener("error", error);
  window.addEventListener("unhandledrejection", rejection);
  window.addEventListener("pointerdown", finalizeLcp, { once: true, capture: true });
  window.addEventListener("keydown", finalizeLcp, { once: true, capture: true });
  const timer = window.setInterval(() => { void client.flush(); if (client.needsRefresh()) void configureObservers(); }, 60_000);
  dispose = () => {
    unsubscribe(); client.reset(); active = undefined;
    if (window.fetch === wrappedFetch) window.fetch = nativeFetch;
    observers.forEach(observer => observer.disconnect()); window.clearInterval(timer);
    document.removeEventListener("visibilitychange", visibility);
    window.removeEventListener("pagehide", flushVitals); window.removeEventListener("pageshow", restored);
    window.removeEventListener("error", error); window.removeEventListener("unhandledrejection", rejection);
    window.removeEventListener("pointerdown", finalizeLcp, true); window.removeEventListener("keydown", finalizeLcp, true);
    dispose = undefined;
  };
  return dispose;
}
export function recordRouteCommit(pathname: string, duration: number) {
  active?.record({ kind: "metric", name: "route_commit", route: routeName(pathname), value: duration });
}

if (import.meta.hot) import.meta.hot.dispose(() => dispose?.());
