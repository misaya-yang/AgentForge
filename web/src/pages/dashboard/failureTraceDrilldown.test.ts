import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as taskTraceNavigation from "../tasks/taskTraceNavigation.ts";
import * as navigation from "./failureTraceNavigation.ts";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
type Query = { queryKey: unknown[]; queryFn: (context: { signal: AbortSignal }) => Promise<unknown> };
const noop = () => {};
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const node = value as Tree; return [node, ...nodes(node.props?.children)]; }
const failure = (dimension: "service" | "model" | "provider" = "service") => ({ dimension, dimension_value: "same / id", error_type: "timeout", failure_count: 8, success_count: 1, total_count: 9, success_rate: 11 });
const trace = (id = "request-a") => ({ request_id: id, trace_id: `trace-${id}`, status: "error", model: "m", provider: "p", service_id: "s", user_id: "u", input_tokens: 1, output_tokens: 2, total_cost_usd: 0, request_total_duration_ms: 1, trace_steps: [] });
function host() {
  let params = new URLSearchParams("tab=reliability&from=2026-10-01&to=2026-10-13&service=service-global&user=user-global&keep=yes"); let locationKey = 0;
  const history: string[] = []; let auth = { token: "a", isAuthenticated: true, user: { user_id: "a", permissions: [] as string[], roles: ["member"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const frames = new Map<string, Slot[]>(); let slots: Slot[] = []; let cursor = 0; let effects: Array<() => void> = []; let uuid = 0;
  let tree: unknown; let childKey = ""; let query!: Query; let queryResult: Record<string, unknown> = {}; let queryIdentity = "";
  const requests: Array<{ path: string; signal: AbortSignal; resolve: (value: unknown) => void; reject: (error: Error) => void }> = [];
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_, key) => String(key) });
  const setParams = (fn: (current: URLSearchParams) => URLSearchParams, options?: { replace?: boolean }) => { if (!options?.replace) history.push(params.toString()); params = fn(params); locationKey++; };
  const context = () => ({ dateRange: navigation.dashboardDateRange(params, ["2026-10-01", "2026-10-13"]), serviceId: params.get("service") || "all", userId: params.get("user") || "all", lastRefresh: new Date(0), traceFilter: navigation.readTraceFilter(params), clearTraceFilter: () => setParams(navigation.clearFailureTrace) });
  const modules: Record<string, unknown> = {
    react: { useState(initial: unknown) { const i = cursor++; const owner = slots; owner[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "@ant-design/icons": named,
    antd: { Table: "Table", Button: "Button", Input: "Input", Tag: "Tag", Spin: "Spin", Segmented: "Segmented", Empty: Object.assign(() => {}, { PRESENTED_IMAGE_SIMPLE: "empty" }) },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) },
    "react-router-dom": { useSearchParams: () => [params, setParams], Link: "Link", useLocation: () => ({ key: String(locationKey), search: `?${params}` }) },
    "../../DashboardContext": { useDashboardContext: context }, "../PanelWrapper": { PanelWrapper: "PanelWrapper" },
    "@/store/useAppStore": { useAppStore: () => ({ darkMode: false }) }, "../../styles": { getColors: () => new Proxy({}, { get: () => "gray" }) },
    "../../hooks/useDashboardEntityLabels": { useDashboardEntityLabels: () => ({ resolveServiceLabel: (id: string) => id, resolveUserLabel: (id: string) => id }) },
    "../../failureTraceNavigation": navigation,
    "../taskTraceNavigation": taskTraceNavigation,
    "@/lib/utils": { generateUUID: () => `scope-${++uuid}` }, "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (state: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/api": { api: { get: (path: string, options: { signal: AbortSignal }) => new Promise((resolve, reject) => requests.push({ path, signal: options?.signal, resolve: value => resolve({ data: value }), reject })) } },
    "@tanstack/react-query": { useQuery: (next: Query) => { const key = JSON.stringify(next.queryKey); if (queryIdentity !== key) { queryIdentity = key; queryResult = {}; } query = next; return { isError: false, isLoading: false, refetch: noop, ...queryResult }; } },
  };
  function compile(path: string) { const exports: Record<string, () => Tree> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, URLSearchParams, AbortController, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/api/usage"] = compile("../../api/usage.ts"); modules["@/hooks/useAuthenticatedReadScope"] = compile("../../hooks/useAuthenticatedReadScope.ts");
  modules["@/pages/tasks/components/TaskTraceLookup"] = compile("../tasks/components/TaskTraceLookup.tsx");
  const failurePanel = compile("./components/panels/FailureAnalysisPanel.tsx").FailureAnalysisPanel;
  const tracePanel = compile("./components/panels/RequestTracePanel.tsx").RequestTracePanel;
  const flush = () => { const pending = effects; effects = []; pending.forEach(fn => fn()); };
  function frame(id: string) { slots = frames.get(id) || []; frames.set(id, slots); cursor = 0; }
  function dispose(id: string) { frames.get(id)?.forEach(slot => slot.cleanup?.()); frames.delete(id); }
  return { requests,
    render(kind: "failure" | "trace" = "trace") { frame(kind); const outer = (kind === "failure" ? failurePanel : tracePanel)(); flush(); if (!outer) { tree = null; return tree; } const key = `${kind}:${outer.key}`; if (childKey && key !== childKey) dispose(childKey); childKey = key; frame(key); tree = (outer.type as (props: unknown) => Tree)(outer.props); flush(); return tree; },
    result(value: unknown) { queryResult = { data: value }; }, error() { queryResult = { ...queryResult, isError: true }; },
    read() { return query.queryFn({ signal: new AbortController().signal }); },
    node(type: string) { const found = nodes(tree).find(node => node.type === type); assert.ok(found, type); return found; },
    all: () => nodes(tree), params: () => new URLSearchParams(params), context,
    navigate(value: URLSearchParams) { setParams(() => value); }, back() { params = new URLSearchParams(history.pop()); locationKey++; },
    auth(patch: Partial<typeof auth>) { const previous = auth; auth = { ...auth, ...patch }; [...subscribers].forEach(fn => fn(auth, previous)); },
  };
}

test("all three failure actions preserve global filters and serialize only the selected group into sampled-trace API", async () => {
  for (const dimension of navigation.failureDimensions) {
    const h = host(); h.render("failure"); const item = failure(dimension); h.result({ items: [item] }); h.render("failure");
    const columns = h.node("Table").props.columns as Array<{ key: string; render: (value: unknown, item: unknown) => Tree }>;
    const button = nodes(columns.find(column => column.key === "inspect")!.render(null, item)).find(node => node.type === "Button")!; (button.props.onClick as () => void)();
    assert.equal(h.params().get("tab"), "tracing"); assert.equal(h.params().get("keep"), "yes"); assert.equal(h.params().get("failure_dimension"), dimension);
    h.render(); const pending = h.read(); const url = new URL(h.requests[0].path, "https://fixture.invalid"); assert.equal(url.searchParams.get("status"), "error"); assert.equal(url.searchParams.get("error_type"), "timeout"); assert.equal(url.searchParams.get(dimension === "service" ? "service_id" : dimension), item.dimension_value); assert.equal(url.searchParams.get("user_id"), "user-global"); assert.equal(url.searchParams.get("start_date"), "2026-10-01"); assert.equal(url.searchParams.get("limit"), "15"); h.requests[0].resolve([]); await pending;
    h.navigate(navigation.failureTraceReturn(h.params())); assert.equal(h.params().get("tab"), "reliability"); assert.equal(h.params().get("failure_dimension"), dimension); assert.equal(h.params().get("trace_error"), null);
  }
});
test("browser back and clear retain date/service/user, invalid dates do not become dashboard windows", () => {
  const h = host(); const before = h.params().toString(); h.navigate(navigation.failureTraceLocation(h.params(), failure(), h.context())); h.back(); assert.equal(h.params().toString(), before);
  const params = navigation.clearFailureTrace(navigation.failureTraceLocation(h.params(), failure("model"), h.context())); assert.equal(params.get("service"), "service-global"); assert.equal(params.get("from"), "2026-10-01");
  assert.deepEqual(navigation.dashboardDateRange(new URLSearchParams("from=2026-02-30&to=2026-03-01"), ["2026-10-01", "2026-10-13"]), ["2026-10-01", "2026-10-13"]);
});
test("query failure is not rendered as an empty successful sample and replacement data drops old selected detail", () => {
  const h = host(); h.render(); const old = trace(); const first = [old]; h.result(first); h.render(); const row = (h.node("Table").props.onRow as (item: unknown) => { onClick: () => void })(old); row.onClick(); h.render();
  h.result([trace("new")]); h.render(); assert.ok(h.all().find(node => typeof node.type === "function" && node.props.trace)?.props.trace); const timeline = h.all().find(node => typeof node.type === "function" && node.props.trace); assert.equal((timeline?.props.trace as { request_id: string }).request_id, "new"); row.onClick(); h.render(); assert.equal((h.all().find(node => typeof node.type === "function" && node.props.trace)?.props.trace as { request_id: string }).request_id, "new");
  h.error(); h.render(); assert.equal(h.node("PanelWrapper").props.dataStatus, "error");
});
test("late request-ID result cannot survive navigation; direct search cannot override a failure group", async () => {
  const h = host(); h.render(); (h.node("Input").props.onChange as (event: unknown) => void)({ target: { value: "id / 1" } }); h.render(); const search = h.node("Input").props.onPressEnter as () => Promise<void>; const pending = search(); assert.match(h.requests[0].path, /id%20%2F%201$/);
  h.navigate(navigation.failureTraceLocation(h.params(), failure(), h.context())); h.render(); assert.equal(h.requests[0].signal.aborted, true); h.requests[0].resolve(trace("old")); await pending; h.render(); assert.equal(h.node("Input").props.disabled, true); await (h.node("Input").props.onPressEnter as () => Promise<void>)(); assert.equal(h.requests.length, 1); assert.equal(h.all().some(node => node.props.trace), false);
});
test("credential A-B-A invalidates retained row actions and in-flight query before rerender", async () => {
  const h = host(); h.render("failure"); h.result({ items: [failure()] }); h.render("failure"); const column = (h.node("Table").props.columns as Array<{ key: string; render: (value: unknown, item: unknown) => Tree }>).find(value => value.key === "inspect")!; const click = nodes(column.render(null, failure())).find(node => node.type === "Button")!.props.onClick as () => void;
  const pending = h.read(); const rejection = assert.rejects(pending, /scope changed/); h.auth({ token: "b" }); h.auth({ token: "a" }); click(); assert.equal(h.params().get("tab"), "reliability"); h.requests[0].resolve({ items: [failure()] }); await rejection; h.render("failure"); assert.equal((h.node("Table").props.dataSource as unknown[]).length, 0);
});
test("reset invalidates a pending ID lookup without issuing a second request", async () => {
  const h = host(); h.render(); (h.node("Input").props.onChange as (event: unknown) => void)({ target: { value: "id" } }); h.render(); const pending = (h.node("Input").props.onPressEnter as () => Promise<void>)(); h.requests[0].reject(new Error("offline")); await pending; h.render();
  const reset = h.all().find(node => node.type === "Button" && node.props.children === "common.reset")!; (reset.props.onClick as () => void)(); h.render(); assert.equal(h.node("Input").props.value, ""); assert.equal(h.requests.length, 1);
});

test("normalized unknown groups cannot silently become an incomplete or broadened trace query", () => {
  const h = host(); h.render("failure"); const item = { ...failure(), error_type: "unknown" }; h.result({ items: [item] }); h.render("failure");
  const column = (h.node("Table").props.columns as Array<{ key: string; render: (value: unknown, item: unknown) => Tree }>).find(value => value.key === "inspect")!;
  const button = nodes(column.render(null, item)).find(node => node.type === "Button")!; assert.equal(button.props.disabled, true); (button.props.onClick as () => void)(); assert.equal(h.params().get("tab"), "reliability");
  assert.equal(navigation.failureTraceLocation(h.params(), item, h.context()).toString(), h.params().toString()); assert.equal(h.requests.length, 0);
});

test("production dashboard canonicalizes saved tracing tab with no URL tab before a drilldown", () => {
  const source = readFileSync(new URL("./index.tsx", import.meta.url), "utf8"); const file = ts.createSourceFile("index.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "DashboardContent") as ts.FunctionDeclaration;
  const effect = component.body!.statements.find(node => ts.isExpressionStatement(node) && node.getText(file).startsWith("useLayoutEffect(")); assert.ok(effect);
  let params = new URLSearchParams("keep=yes"); const run = ts.transpileModule(effect.getText(file), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  vm.runInNewContext(run, { URLSearchParams, requestedTab: null, activeTab: "tracing", useLayoutEffect: (fn: () => void) => fn(), setSearchParams: (fn: (value: URLSearchParams) => URLSearchParams, options: { replace: boolean }) => { assert.equal(options.replace, true); params = fn(params); } });
  const drill = navigation.failureTraceLocation(params, failure(), { dateRange: ["2026-10-01", "2026-10-13"], serviceId: "all", userId: "all" }); assert.equal(navigation.failureTraceReturn(drill).get("tab"), "tracing"); assert.equal(drill.get("keep"), "yes");
});


test("real trace-panel route branch loads exact encoded ID and browser-back retires old lookup", async () => {
  const h = host(); h.navigate(new URLSearchParams("tab=tracing&request_id=req%2F%3F%26&task_id=task-a&task_status=failed&task_offset=50")); h.render();
  assert.equal(h.requests.length, 1); assert.equal(h.requests[0].path, "/api/v1/usage/traces/req%2F%3F%26");
  assert.equal(h.all().some(node => node.type === "Table"), false);
  const back = h.node("Link"); assert.equal(back.props.to, "/tasks?tab=query&task_id=task-a&task_status=failed&task_offset=50");
  h.navigate(new URLSearchParams("tab=tracing&request_id=next&task_id=task-b")); h.render(); assert.equal(h.requests[0].signal.aborted, true); assert.equal(h.requests.length, 2);
  h.requests[0].resolve(trace("req/?&")); await Promise.resolve(); await Promise.resolve(); assert.equal(h.all().some(node => node.props.children === "req/?&"), false);
  h.back(); h.render(); assert.equal(h.requests.length, 3); assert.equal(h.requests[2].path, "/api/v1/usage/traces/req%2F%3F%26");
});

test("actual trace route auth subscription retires late response and stale return action", async () => {
  const h = host(); h.navigate(new URLSearchParams("tab=tracing&request_id=req&task_id=task-a")); h.render(); const back = h.node("Link");
  h.auth({ token: "b" }); h.requests[0].resolve(trace("req")); await Promise.resolve(); await Promise.resolve();
  let prevented = false; (back.props.onClick as (event: unknown) => void)({ preventDefault: () => { prevented = true; } }); assert.equal(prevented, true);
  h.render(); assert.equal(h.requests[0].signal.aborted, true); assert.equal(h.requests.length, 2);
});
