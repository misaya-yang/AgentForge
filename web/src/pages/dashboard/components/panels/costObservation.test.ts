import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { setImmediate } from "node:timers/promises";
import ts from "typescript";
import React, { type ReactNode } from "react";
import * as runtime from "react/jsx-runtime";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryObserver, type QueryKey } from "@tanstack/react-query";
import dayjs from "dayjs";

type Payload = Record<string, unknown>;
type QueryOptions = { queryKey: QueryKey; queryFn: () => Promise<unknown>; staleTime: number };
type Mode = "ok" | "error" | "absent" | "no_data" | "collection_error" | "stale" | "nan" | "empty" | "null-row" | "non-array";
const fixedNow = "2026-10-12T12:00:00Z";
const keys = ["today", "week", "month", "breakdown", "provider-breakdown", "timeseries"];

function host(modes: Record<string, Mode> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const calls: string[] = [];
  const blocks = new Map<string, Promise<void>>();
  const options = new Map<string, QueryOptions>();
  let buttons: (() => void)[] = [];
  const modules: Record<string, unknown> = {};
  function load(file: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true },
    }).outputText, { exports, URLSearchParams, require(name: string) {
      assert.ok(name in modules, `Unexpected dependency ${name}`); return modules[name];
    } });
    return exports;
  }
  const primitive = (name: string) => ({ children }: { children?: ReactNode }) => React.createElement("div", { "data-primitive": name }, children);
  const jsx = (type: Parameters<typeof runtime.jsx>[0], props: Record<string, unknown>, key?: string) => {
    if (type === "button" && props.children === "common.retry") buttons.push(props.onClick as () => void);
    return runtime.jsx(type, props, key);
  };
  Object.assign(modules, {
    react: React, "react/jsx-runtime": { ...runtime, jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAppStore": { useAppStore: () => ({ darkMode: false }) },
    antd: Object.fromEntries(["Row", "Col", "Tooltip"].map(name => [name, primitive(name)])),
    "@ant-design/icons": { SyncOutlined: primitive("SyncOutlined") },
    "lucide-react": { AlertTriangle: primitive("AlertTriangle"), RefreshCw: primitive("RefreshCw") },
  });
  modules["../../styles"] = modules["../styles"] = load("../../styles.ts");
  modules["./DataStatusBadge"] = load("../DataStatusBadge.tsx");
  modules["../PanelWrapper"] = load("../PanelWrapper.tsx");
  modules["@/lib/api"] = { api: { async get(url: string) {
    calls.push(url);
    const u = new URL(url, "https://fixture.invalid");
    const start = u.searchParams.get("start_date");
    const key = u.pathname.endsWith("/summary") ? start === "2026-10-12" ? "today" : start === "2026-10-11" ? "week" : "month"
      : u.pathname.endsWith("/timeseries") ? "timeseries" : u.searchParams.get("dimension") === "provider" ? "provider-breakdown" : "breakdown";
    await blocks.get(key);
    const mode = modes[key] || "ok";
    if (mode === "error") throw new Error("Controlled read failure; never expose raw transport details");
    const cost = key === "today" || key === "week" ? 0 : 9.5;
    const data: Payload = key === "timeseries" ? { data: [{ date: "2026-10-12", cost_usd: 0 }] }
      : key.includes("breakdown") ? { items: [{ service: "fixture-service", provider: "fixture-provider", cost_usd: 9.5 }] }
      : { total_cost_usd: cost, total_requests: 7, success_rate: 100, total_tokens: 30 };
    Object.assign(data, { data_status: "ok", data_freshness_minutes: 1, start_date: start, end_date: "2026-10-12" });
    if (["no_data", "collection_error", "stale"].includes(mode)) data.data_status = mode;
    if (mode === "null-row") { data.items = [null]; data.data = [null]; }
    if (mode === "non-array") { data.items = {}; data.data = {}; }
    if (mode === "empty") { data.items = []; data.data = []; }
    if (mode === "absent" || mode === "nan") {
      if (key.includes("breakdown") || key === "timeseries") {
        const row = ((data.items || data.data) as Payload[])[0];
        if (mode === "nan") row.cost_usd = NaN; else delete row.cost_usd;
      } else if (mode === "nan") data.total_cost_usd = NaN; else delete data.total_cost_usd;
    }
    return { data };
  } } };
  modules["@/api/usage"] = load("../../../../api/usage.ts");
  Object.assign(modules, {
    recharts: Object.fromEntries(["AreaChart", "Area", "XAxis", "YAxis", "CartesianGrid", "Tooltip", "PieChart", "Pie", "Cell"].map(name => [name, primitive(name)])),
    "@/components/SafeResponsiveChart": { SafeResponsiveChart: primitive("SafeResponsiveChart") },
    dayjs: (value?: string) => dayjs(value === undefined ? fixedNow : value),
    "../../DashboardContext": { useDashboardContext: () => ({ dateRange: ["2026-10-05", "2026-10-12"], granularity: "day", serviceId: "all", userId: "all", lastRefresh: new Date(fixedNow) }) },
    "../../hooks/useDashboardEntityLabels": { useDashboardEntityLabels: () => ({ resolveServiceLabel: (value: string) => value }) },
    "@tanstack/react-query": { useQuery(value: QueryOptions) {
      options.set(String(value.queryKey[0]), value);
      return new QueryObserver(client, { ...value, retry: false }).getOptimisticResult({ ...value, retry: false });
    } },
  });
  const Panel = load("./CostAnalysisPanel.tsx").CostAnalysisPanel as React.ComponentType;
  function render() { buttons = []; return renderToStaticMarkup(React.createElement(Panel)); }
  async function fetchAll() { await Promise.allSettled([...options.values()].map(value => client.fetchQuery({ ...value, retry: false }))); }
  async function retry(index = 0) { buttons[index](); while (client.isFetching()) await setImmediate(); }
  render();
  return { client, calls, modes, options, blocks, render, fetchAll, retry, click: (index = 0) => buttons[index](), buttonCount: () => buttons.length, close: () => client.clear() };
}
function states(html: string) { return [...html.matchAll(/data-cost-state="([^"]+)"/g)].map(match => match[1]); }

for (const key of keys) {
  test(`one ${key} read fails while five independent regions stay visible; retry touches only that GET`, async () => {
    const h = host({ [key]: "error" });
    try {
      await h.fetchAll(); const html = h.render();
      assert.equal(states(html).filter(x => x === "error").length, 1);
      assert.equal(states(html).filter(x => x === "ready").length, 5);
      assert.equal(h.buttonCount(), 1);
      assert.ok(!html.includes("Controlled read failure"));
      const failedCall = h.calls[keys.indexOf(key)];
      h.modes[key] = "ok"; await h.retry();
      assert.deepEqual(h.calls.slice(6), [failedCall]);
      assert.deepEqual(states(h.render()), Array(6).fill("ready"));
    } finally { h.close(); }
  });
}
for (const mode of ["absent", "nan", "no_data", "collection_error"] as const) {
  test(`weekly ${mode} cannot masquerade as a zero metric`, async () => {
    const h = host({ week: mode });
    try { await h.fetchAll(); const html = h.render(); assert.equal(states(html)[1], mode === "collection_error" ? "error" : "unavailable"); assert.equal((html.match(/\$0\.0000/g) || []).length, 1); assert.ok(html.includes("$9.50")); }
    finally { h.close(); }
  });
}
test("observed zero is ready and preserves original estimated USD formatting", async () => {
  const h = host(); try { await h.fetchAll(); const html = h.render(); assert.deepEqual(states(html), Array(6).fill("ready")); assert.equal((html.match(/\$0\.0000/g) || []).length, 2); assert.ok(html.includes("dashboard.cost.estimatedHint")); assert.equal(h.buttonCount(), 0); } finally { h.close(); }
});
test("initial loading is explicit per region, never a zero", () => {
  const h = host(); try { const html = h.render(); assert.deepEqual(states(html), Array(6).fill("loading")); assert.ok(!html.includes("$0.0000")); } finally { h.close(); }
});
test("all reads failing keeps six separate recoverable errors", async () => {
  const h = host(Object.fromEntries(keys.map(key => [key, "error"]))); try { await h.fetchAll(); const html = h.render(); assert.deepEqual(states(html), Array(6).fill("error")); assert.equal(h.buttonCount(), 6); assert.ok(!html.includes("$0.0000")); } finally { h.close(); }
});
test("failed refresh hides cached cost; explicitly stale source values are labelled", async () => {
  const h = host({ week: "stale" }); try {
    await h.fetchAll(); assert.equal(states(h.render())[1], "stale"); assert.ok(h.render().includes("dashboard.dataStatus.stale"));
    h.modes.week = "error";
    await h.client.fetchQuery({ ...h.options.get("dashboard-cost-week")!, staleTime: 0, retry: false }).catch(() => {});
    const html = h.render(); assert.equal(states(html)[1], "error"); assert.equal((html.match(/\$0\.0000/g) || []).length, 1);
  } finally { h.close(); }
});
for (const key of ["breakdown", "provider-breakdown", "timeseries"]) {
  test(`${key} missing cost or empty rows hides only that chart`, async () => {
    for (const mode of ["absent", "empty", "null-row", "non-array"] as const) {
      const h = host({ [key]: mode }); try { await h.fetchAll(); const html = h.render(); assert.equal(states(html)[keys.indexOf(key)], "unavailable"); assert.equal(states(html).filter(x => x === "ready").length, 5); assert.ok(html.includes("$9.50")); } finally { h.close(); }
    }
  });
}

test("all 64 success/error combinations preserve exactly the successful regions", async () => {
  const h = host();
  try {
    for (let mask = 0; mask < 64; mask++) {
      h.client.clear();
      keys.forEach((key, index) => { h.modes[key] = mask & (1 << index) ? "error" : "ok"; });
      await h.fetchAll();
      assert.deepEqual(states(h.render()), keys.map((_, index) => mask & (1 << index) ? "error" : "ready"), `mask ${mask}`);
    }
  } finally { h.close(); }
});
test("one pending read leaves five successful regions visible", async () => {
  const h = host(); let release!: () => void;
  try {
    h.blocks.set("week", new Promise<void>(resolve => { release = resolve; }));
    const fetching = h.fetchAll(); await setImmediate();
    assert.deepEqual(states(h.render()), ["ready", "loading", "ready", "ready", "ready", "ready"]);
    release(); await fetching; assert.deepEqual(states(h.render()), Array(6).fill("ready"));
  } finally { release?.(); h.close(); }
});
test("repeated local retry during an in-flight read coalesces and marks only that region loading", async () => {
  const h = host({ week: "error" }); let release!: () => void;
  try {
    await h.fetchAll(); h.render(); h.modes.week = "ok";
    h.blocks.set("week", new Promise<void>(resolve => { release = resolve; }));
    h.click(); h.click(); await setImmediate();
    assert.equal(h.calls.length, 7);
    assert.deepEqual(states(h.render()), ["ready", "loading", "ready", "ready", "ready", "ready"]);
    release(); while (h.client.isFetching()) await setImmediate();
    assert.deepEqual(states(h.render()), Array(6).fill("ready"));
  } finally { release?.(); h.close(); }
});
