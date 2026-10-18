import { IntlMessageFormat } from "intl-messageformat";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { setImmediate } from "node:timers/promises";
import ts from "typescript";
import { QueryClient, QueryObserver, type QueryKey } from "@tanstack/react-query";
import dayjs from "dayjs";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
type Query = { queryKey: QueryKey; queryFn: (context: { signal: AbortSignal }) => Promise<unknown>; retry?: boolean; staleTime: number };
const point = (patch = {}) => ({ date: "2026-10-14", request_count: 3, avg_total_ms: 10, p50_total_ms: 8, p95_total_ms: 15, p99_total_ms: 20, avg_first_token_ms: 0, avg_llm_inference_ms: 6, avg_retrieval_ms: 0, avg_tool_call_ms: 0, avg_overhead_ms: 4, ...patch });
const payload = (patch = {}) => ({ data: [point()], start_date: "2026-10-01", end_date: "2026-10-14", granularity: "day", data_status: "live", data_freshness_minutes: 1, ...patch });
const walk = (value: unknown): Tree[] => Array.isArray(value) ? value.flatMap(walk) : value && typeof value === "object" ? [value as Tree, ...walk((value as Tree).props?.children)] : [];
const text = (value: unknown): string => Array.isArray(value) ? value.map(text).join(" ") : value && typeof value === "object" ? text((value as Tree).props?.children) : String(value ?? "");
function host(locale = "en-US") {
  const translations = JSON.parse(readFileSync(new URL(`../../../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
  const t = (key: string, options?: string | Record<string, unknown>) => {
    let value: unknown = translations;
    for (const part of key.split(".")) value = value && typeof value === "object" ? (value as Record<string, unknown>)[part] : undefined;
    return String(new IntlMessageFormat(String(value ?? (typeof options === "string" ? options : key)), locale).format((typeof options === "object" ? options : {}) as never));
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  let context = { dateRange: ["2026-10-01", "2026-10-14"], granularity: "day", serviceId: "service / same", userId: "member & one", lastRefresh: new Date(0) };
  let auth = { token: "credential-a", isAuthenticated: true, user: { user_id: "actor-a", permissions: [] as string[], roles: ["admin"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const frames = new Map<string, Slot[]>(); let slots: Slot[] = []; let cursor = 0; let effects: Array<() => void> = []; let uuid = 0;
  let currentFrames = new Set<string>(); let tree: unknown; let query!: Query; let response: unknown = payload(); let block: Promise<void> | undefined;
  const requests: Array<{ path: string; signal?: AbortSignal }> = [];
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_, key) => String(key) });
  const modules: Record<string, unknown> = {
    react: { useState(initial: unknown) { const i = cursor++; const owner = slots; owner[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useLayoutEffect: effect },
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" }, "@ant-design/icons": named, antd: named, "lucide-react": named, recharts: named,
    "react-i18next": { useTranslation: () => ({ t }) },
    "../../DashboardContext": { useDashboardContext: () => context },
    "@/store/useAppStore": { useAppStore: () => ({ darkMode: false }) },
    "@/components/SafeResponsiveChart": { SafeResponsiveChart: "SafeResponsiveChart" }, dayjs,
    "@/lib/utils": { generateUUID: () => `scope-${++uuid}` },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (state: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/api": { api: { async get(path: string, options?: { signal?: AbortSignal }) { requests.push({ path, signal: options?.signal }); const value = response; await block; if (value instanceof Error) throw value; return { data: value }; } } },
    "@tanstack/react-query": { useQuery(value: Query) { query = value; return new QueryObserver(client, value).getOptimisticResult(value); } },
  };
  function load(path: string) { const exports: Record<string, unknown> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, { exports, URLSearchParams, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/lib/sdkAuthorityScope"] = load("../../../../lib/sdkAuthorityScope.ts");
  modules["@/api/usage"] = load("../../../../api/usage.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = load("../../../../hooks/useAuthenticatedReadScope.ts");
  modules["../../styles"] = modules["../styles"] = load("../../styles.ts");
  modules["./DataStatusBadge"] = load("../DataStatusBadge.tsx");
  modules["../PanelWrapper"] = load("../PanelWrapper.tsx");
  const Panel = load("./PerformancePanel.tsx").PerformancePanel;
  function dispose(id: string) { frames.get(id)?.forEach(slot => slot.cleanup?.()); frames.delete(id); }
  function expand(value: unknown, parent = "root"): unknown {
    if (Array.isArray(value)) return value.map((item, i) => expand(item, `${parent}:${i}`));
    if (!value || typeof value !== "object") return value;
    const node = value as Tree;
    if (typeof node.type === "function") {
      const id = `${parent}:${node.type.name}:${node.key ?? ""}`; currentFrames.add(id); slots = frames.get(id) || []; frames.set(id, slots); cursor = 0;
      const result = node.type(node.props); const pending = effects; effects = []; pending.forEach(fn => fn()); return expand(result, id);
    }
    return { ...node, props: { ...node.props, children: expand(node.props.children, parent) } };
  }
  function render() { currentFrames = new Set(); tree = expand(jsx(Panel, {})); for (const id of frames.keys()) if (!currentFrames.has(id)) dispose(id); return tree; }
  function all(type?: string) { return walk(tree).filter(node => !type || node.type === type); }
  render();
  return { client, requests, t, render, all, text: () => text(tree), query: () => query,
    setResponse(value: unknown) { response = value; }, block(value?: Promise<void>) { block = value; },
    fetch: () => client.fetchQuery(query),
    context(patch: Partial<typeof context>) { context = { ...context, ...patch }; },
    auth(patch: Partial<typeof auth>) { const prior = auth; auth = { ...auth, ...patch }; [...subscribers].forEach(fn => fn(auth, prior)); },
    refresh() { return all("button").find(node => node.props["aria-label"] === t("common.refresh"))!.props.onClick as () => void; },
    retry() { return all("button").find(node => text(node.props.children).includes(t("common.retry")))!.props.onClick as () => void; },
    stats: () => all("Statistic").map(node => node.props.value),
    chart: () => all("AreaChart")[0]?.props.data as Array<Record<string, unknown>> | undefined,
    close() { for (const id of frames.keys()) dispose(id); client.clear(); },
  };
}

test("failed performance read hides numeric statistics and offers explicit GET recovery", async () => {
  const h = host(); try {
    h.setResponse(new Error("SECRET unsafe transport detail")); await assert.rejects(h.fetch()); h.render();
    assert.equal(h.stats().length, 0); assert.ok(h.text().includes(h.t("dashboard.panel.loadError"))); assert.ok(!h.text().includes("SECRET"));
    h.setResponse(payload()); h.retry()(); while (h.client.isFetching()) await setImmediate(); h.render();
    assert.deepEqual(h.stats(), [8, 15, 20, 10, 0, 6, 0, 0, 4]); assert.equal(h.requests.length, 2);
  } finally { h.close(); }
});
test("performance serializes the exact selected service user window and granularity", async () => {
  const h = host(); try {
    await h.fetch(); const url = new URL(h.requests[0].path, "https://fixture.invalid");
    assert.equal(url.searchParams.get("service_id"), "service / same"); assert.equal(url.searchParams.get("user_id"), "member & one");
    assert.equal(url.searchParams.get("start_date"), "2026-10-01"); assert.equal(url.searchParams.get("end_date"), "2026-10-14"); assert.equal(url.searchParams.get("granularity"), "day");
    assert.ok(h.requests[0].signal); assert.equal(h.query().retry, false); assert.ok(!JSON.stringify(h.query().queryKey).includes("credential"));
  } finally { h.close(); }
});
test("missing performance duration remains unknown instead of becoming zero", async () => {
  const h = host(); try { h.setResponse(payload({ data: [point({ p95_total_ms: undefined })] })); await h.fetch(); h.render(); assert.equal(h.stats()[1], h.t("common.unknown")); assert.equal(h.chart()![0].p95, null); } finally { h.close(); }
});
for (const locale of ["en-US", "zh-CN"]) {
  test(`${locale}: empty malformed and no-sample payloads never claim zero latency`, async () => {
    for (const data of [undefined, null, {}, [], [null], [point({ request_count: undefined })], [point({ request_count: -1 })], [point({ request_count: 0 })]]) {
      const h = host(locale); try { h.setResponse(payload({ data })); await h.fetch(); h.render(); assert.equal(h.stats().length, 0); assert.equal(h.chart(), undefined); assert.ok(h.text().includes(h.t("dashboard.performance.noSamples"))); assert.ok(h.retry()); } finally { h.close(); }
    }
  });
  test(`${locale}: returned zero stays zero and invalid fields stay unknown independently`, async () => {
    const h = host(locale); try {
      h.setResponse(payload({ data: [point({ avg_total_ms: null, p50_total_ms: 0, p95_total_ms: -1, p99_total_ms: Infinity, avg_first_token_ms: "0", avg_llm_inference_ms: NaN })] })); await h.fetch(); h.render();
      assert.deepEqual(h.stats(), [0, h.t("common.unknown"), h.t("common.unknown"), h.t("common.unknown"), h.t("common.unknown"), h.t("common.unknown"), 0, 0, 4]);
      assert.equal(h.all("Statistic")[1].props.suffix, undefined); assert.equal(h.all("Statistic")[0].props.suffix, "ms");
      assert.ok(h.text().includes(h.t("dashboard.performance.sourceHint"))); assert.ok(h.text().includes("2026-10-14"));
    } finally { h.close(); }
  });
}
test("refresh hides cached metrics while pending and after failure; duplicate retry coalesces", async () => {
  const h = host(); let release!: () => void; try {
    await h.fetch(); h.render(); h.block(new Promise<void>(resolve => { release = resolve; })); h.setResponse(new Error("offline")); const refresh = h.refresh(); refresh(); refresh(); h.render();
    assert.equal(h.stats().length, 0); assert.equal(h.requests.length, 2); release(); while (h.client.isFetching()) await setImmediate(); h.render(); assert.equal(h.stats().length, 0); assert.ok(h.retry());
  } finally { release?.(); h.close(); }
});
test("source error and no-data flags override retained observations; stale labels preserve only real values", async () => {
  for (const status of ["error", "collection_error", "unavailable", "no_data", "empty", "stale", "delayed", "live"]) {
    const h = host(); try { h.setResponse(payload({ data_status: status, data_freshness_minutes: 8 })); await h.fetch(); h.render();
      if (["stale", "delayed", "live"].includes(status)) { assert.equal(h.stats().length, 9); assert.ok(h.text().includes("STALE")); }
      else { assert.equal(h.stats().length, 0); assert.ok(h.retry()); }
    } finally { h.close(); }
  }
});
test("zero-request buckets are chart gaps and latest-bucket summary does not reuse older values", async () => {
  const h = host(); try { h.setResponse(payload({ data: [point(), point({ date: "2026-10-15", request_count: 0 })] })); await h.fetch(); h.render(); assert.ok(h.stats().every(value => value === h.t("common.unknown"))); assert.equal(h.chart()![1].avgLatency, null); assert.equal(h.chart()![0].avgLatency, 10); } finally { h.close(); }
});
test("filter replacement retires old GET and refresh callbacks and isolates cache values", async () => {
  for (const patch of [{ serviceId: "other" }, { userId: "other" }, { dateRange: ["2026-10-10", "2026-10-14"] }, { granularity: "hour" }, { lastRefresh: new Date(1) }]) {
    const h = host(); let release!: () => void; try {
      const oldKey = JSON.stringify(h.query().queryKey); const oldRefresh = h.refresh(); h.block(new Promise<void>(resolve => { release = resolve; })); const pending = h.fetch(); const rejected = assert.rejects(pending, /scope changed/);
      h.context(patch); h.render(); assert.notEqual(JSON.stringify(h.query().queryKey), oldKey); assert.equal(h.stats().length, 0); oldRefresh(); assert.equal(h.requests.length, 1); release(); await rejected;
    } finally { release?.(); h.close(); }
  }
});
test("authority A-B-A retires pending GET and callback before React rerender", async () => {
  const h = host(); let release!: () => void; try {
    const oldKey = JSON.stringify(h.query().queryKey); const refresh = h.refresh(); h.block(new Promise<void>(resolve => { release = resolve; })); const pending = h.fetch(); const rejected = assert.rejects(pending, /scope changed/);
    h.auth({ token: "credential-b" }); h.auth({ token: "credential-a" }); refresh(); assert.equal(h.requests.length, 1); release(); await rejected; h.render(); assert.notEqual(JSON.stringify(h.query().queryKey), oldKey); assert.equal(h.stats().length, 0);
  } finally { release?.(); h.close(); }
});
test("permission change and unmount retire old request dispatch", async () => {
  for (const dispose of [false, true]) {
    const h = host(); const read = h.query().queryFn; try { if (dispose) h.close(); else h.auth({ user: { user_id: "actor-a", permissions: ["changed"], roles: ["admin"], tier: "normal" } }); await assert.rejects(read({ signal: new AbortController().signal }), /scope changed/); assert.equal(h.requests.length, 0); } finally { h.close(); }
  }
});
test("all filters omit optional identities without changing the selected window", async () => {
  const h = host(); try { h.context({ serviceId: "all", userId: "all" }); h.render(); await h.fetch(); const url = new URL(h.requests[0].path, "https://fixture.invalid"); assert.equal(url.searchParams.has("service_id"), false); assert.equal(url.searchParams.has("user_id"), false); assert.equal(url.searchParams.get("start_date"), "2026-10-01"); } finally { h.close(); }
});

test("metric selection survives data refresh and filters; retired selector cannot change the new view", async () => {
  const h = host(); try {
    const select = h.all("Select")[0].props.onChange as (value: string) => void; select("p99"); h.render(); assert.equal(h.all("Select")[0].props.value, "p99");
    h.context({ lastRefresh: new Date(5) }); h.render(); assert.equal(h.all("Select")[0].props.value, "p99"); select("avg"); h.render(); assert.equal(h.all("Select")[0].props.value, "p99");
    h.context({ serviceId: "other" }); h.render(); assert.equal(h.all("Select")[0].props.value, "p99");
  } finally { h.close(); }
});
