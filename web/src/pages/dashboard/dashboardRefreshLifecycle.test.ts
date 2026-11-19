import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import dayjs from "dayjs";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { createDashboardRefreshScheduler } from "./dashboardRefreshScheduler.ts";
import * as navigation from "./failureTraceNavigation.ts";

type Tree = { type: unknown; props: Record<string, unknown> };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
function text(value: unknown): string { if (typeof value === "string") return value; if (Array.isArray(value)) return value.map(text).join(" "); return value && typeof value === "object" ? text((value as Tree).props?.children) : ""; }
function clock() {
  let now = 0; let visible = true; let id = 0; const timers = new Map<number, { at: number; callback: () => void }>(); const retained: Array<() => void> = [];
  return { now: () => now, visible: () => visible, setVisible(value: boolean) { visible = value; }, timers, retained,
    setTimeout(callback: () => void, delay: number) { const key = ++id; timers.set(key, { at: now + delay, callback }); retained.push(callback); return key; },
    clearTimeout(key: number) { timers.delete(key); },
    advance(ms: number) { const end = now + ms; for (;;) { const next = [...timers].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0]; if (!next) break; now = next[1].at; timers.delete(next[0]); next[1].callback(); } now = end; },
  };
}
function scheduler(intervalMs = 1000) {
  const timer = clock(); let requests = 0; const visibility: boolean[] = [];
  const control = createDashboardRefreshScheduler({ intervalMs, now: timer.now, visible: timer.visible, schedule: (fn, delay) => { const id = timer.setTimeout(fn, delay); return () => timer.clearTimeout(id); }, onRequest: () => { requests++; }, onVisibility: value => visibility.push(value) });
  return { timer, control, count: () => requests, visibility };
}
test("hidden time causes zero scheduled requests and overdue resume coalesces missed intervals", () => {
  const h = scheduler(); h.timer.advance(500); h.timer.setVisible(false); h.control.visibilityChanged(); assert.equal(h.timer.timers.size, 0); h.timer.advance(10000); assert.equal(h.count(), 0);
  h.timer.setVisible(true); h.control.visibilityChanged(); h.control.visibilityChanged(); assert.equal(h.count(), 1); assert.equal(h.timer.timers.size, 1); h.timer.advance(1000); assert.equal(h.count(), 2); h.control.dispose();
});
test("short hidden period keeps remaining deadline; manual refresh restarts it even while hidden", () => {
  const h = scheduler(); h.timer.advance(200); h.timer.setVisible(false); h.control.visibilityChanged(); h.timer.advance(200); h.timer.setVisible(true); h.control.visibilityChanged(); assert.equal(h.count(), 0); h.timer.advance(599); assert.equal(h.count(), 0); h.timer.advance(1); assert.equal(h.count(), 1);
  h.timer.setVisible(false); h.control.visibilityChanged(); h.control.manual(); assert.equal(h.count(), 2); assert.equal(h.timer.timers.size, 0); h.timer.advance(500); h.timer.setVisible(true); h.control.visibilityChanged(); assert.equal(h.count(), 2); h.timer.advance(500); assert.equal(h.count(), 3); h.control.dispose();
});
test("manual mode never catches up, and disposed or cancelled timer callbacks cannot dispatch", () => {
  const h = scheduler(0); h.timer.setVisible(false); h.control.visibilityChanged(); h.timer.advance(10000); h.timer.setVisible(true); h.control.visibilityChanged(); assert.equal(h.count(), 0); h.control.manual(); assert.equal(h.count(), 1); h.control.dispose(); h.control.manual(); h.control.visibilityChanged(); assert.equal(h.count(), 1);
  const active = scheduler(); const queued = active.timer.retained[0]; active.timer.setVisible(false); active.control.visibilityChanged(); queued(); assert.equal(active.count(), 0); active.control.dispose(); queued(); assert.equal(active.count(), 0);
});
test("a visibility race at the timer boundary pauses rather than requesting hidden reads", () => {
  const h = scheduler(); h.timer.setVisible(false); h.timer.advance(1000); assert.equal(h.count(), 0); assert.equal(h.timer.timers.size, 0); assert.equal(h.visibility.at(-1), false); h.timer.setVisible(true); h.control.visibilityChanged(); assert.equal(h.count(), 1); h.control.dispose();
});
function host() {
  const timer = clock(); const listeners = new Set<() => void>(); let slots: Slot[] = []; let cursor = 0; let effects: Array<() => void> = []; const contextSlots: Slot[] = [];
  let params = new URLSearchParams("tab=tracing&from=2026-10-01&to=2026-10-13&service=s&user=u&trace_status=error");
  const setParams = (fn: (value: URLSearchParams) => URLSearchParams) => { params = fn(params); };
  class ClockDate extends Date { constructor(value?: string | number) { super(value === undefined ? 1_700_000_000_000 + timer.now() : value); } static now() { return 1_700_000_000_000 + timer.now(); } }
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const previous = slots[i]; if (!previous || deps.some((d, j) => !Object.is(d, previous.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const previous = slots[i]; if (!previous || deps.some((d, j) => !Object.is(d, previous.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { previous?.cleanup?.(); next.cleanup = fn(); }); } };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: { createContext: () => ({ Provider: "Provider" }), useContext: () => { throw new Error("Not used"); }, useState(initial: unknown) { const i = cursor++; const owner = slots; owner[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect },
    "react/jsx-runtime": { jsx, jsxs: jsx }, dayjs,
    "react-router-dom": { useSearchParams: () => [params, setParams] }, "./failureTraceNavigation": navigation,
    "../dashboardRefreshScheduler": { createDashboardRefreshScheduler },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
  };
  function compile(path: string) { const exports: Record<string, (props?: unknown) => Tree> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, Date: ClockDate, URLSearchParams, performance: { now: timer.now }, document: { get visibilityState() { return timer.visible() ? "visible" : "hidden"; }, addEventListener: (name: string, fn: () => void) => { assert.equal(name, "visibilitychange"); listeners.add(fn); }, removeEventListener: (name: string, fn: () => void) => { assert.equal(name, "visibilitychange"); listeners.delete(fn); } }, setTimeout: timer.setTimeout, clearTimeout: timer.clearTimeout, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["./hooks/useDashboardRefresh"] = compile("./hooks/useDashboardRefresh.ts"); const provider = compile("./DashboardContext.tsx").DashboardProvider; const status = compile("./components/DashboardRefreshStatus.tsx").DashboardRefreshStatus;
  type Value = { lastRefresh: Date; refreshMode: string; refreshInterval: number; triggerRefresh: () => void; setRefreshInterval: (value: number) => void; dateRange: string[]; serviceId: string; userId: string; traceFilter: unknown };
  return { timer, listeners,
    render() { slots = contextSlots; cursor = 0; const tree = provider({ children: "child" }); const pending = effects; effects = []; pending.forEach(fn => fn()); return tree.props.value as Value; },
    visibility(value: boolean) { timer.setVisible(value); [...listeners].forEach(fn => fn()); },
    unmount() { contextSlots.forEach(slot => slot.cleanup?.()); },
    header(value: Value) { return status({ requestedAt: value.lastRefresh, mode: value.refreshMode }); },
  };
}
test("actual context pauses query-key-triggered reads, resumes once and preserves URL filters", async () => {
  const h = host(); const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } }); let reads = 0;
  const options = (value: ReturnType<typeof h.render>) => ({ queryKey: ["refresh-probe", value.lastRefresh.getTime()], queryFn: async () => { reads++; return reads; } });
  const initial = h.render(); const observer = new QueryObserver(client, options(initial)); const unsubscribe = observer.subscribe(() => {}); await new Promise(resolve => setImmediate(resolve)); assert.equal(reads, 1);
  h.visibility(false); h.timer.advance(180000); const hidden = h.render(); observer.setOptions(options(hidden)); await new Promise(resolve => setImmediate(resolve)); assert.equal(reads, 1); assert.equal(hidden.refreshMode, "paused");
  h.visibility(true); const resumed = h.render(); observer.setOptions(options(resumed)); await new Promise(resolve => setImmediate(resolve)); assert.equal(reads, 2); assert.deepEqual(resumed.dateRange, ["2026-10-01", "2026-10-13"]); assert.equal(resumed.serviceId, "s"); assert.equal(resumed.userId, "u"); assert.equal((resumed.traceFilter as { status: string }).status, "error");
  unsubscribe(); client.clear(); h.unmount();
});
test("actual context interval changes replace timers, manual mode disables resume and manual request stays operative", () => {
  const h = host(); h.render(); h.timer.advance(10000); h.render().setRefreshInterval(30); h.render(); assert.equal(h.timer.timers.size, 1); h.timer.advance(29999); assert.equal(h.render().lastRefresh.getTime(), 1_700_000_000_000); h.timer.advance(1); assert.equal(h.render().lastRefresh.getTime(), 1_700_000_040_000);
  h.render().setRefreshInterval(0); h.render(); assert.equal(h.timer.timers.size, 0); h.visibility(false); h.timer.advance(120000); h.visibility(true); assert.equal(h.render().refreshMode, "manual"); assert.equal(h.render().lastRefresh.getTime(), 1_700_000_040_000); h.render().triggerRefresh(); assert.equal(h.render().lastRefresh.getTime(), 1_700_000_160_000); h.unmount();
});
test("actual context unmount removes listeners and makes retained manual/timer callbacks inert", () => {
  const h = host(); const value = h.render(); const timer = h.timer.retained[0]; assert.equal(h.listeners.size, 1); h.unmount(); assert.equal(h.listeners.size, 0); assert.equal(h.timer.timers.size, 0); value.triggerRefresh(); timer(); assert.equal(h.timer.timers.size, 0);
});
test("actual header labels request time separately from data freshness and declares pause limits", () => {
  const h = host(); h.render(); h.visibility(false); const value = h.render(); const header = h.header(value); assert.match(text(header), /refreshLifecycle.paused/); assert.match(text(header), /refreshLifecycle.requestedAt/); assert.equal(header.props.title, "dashboard.refreshLifecycle.notice"); h.unmount();
});
