import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import * as navigation from "./taskNavigation.ts";

interface Tree { type: string | ((props: Record<string, unknown>) => Tree); props: Record<string, unknown>; key?: string }
interface Receipt { task_id: string; status: string; service_id?: string; created_at?: string; error?: string }
type QueryState = { data?: unknown; isLoading?: boolean; isFetching?: boolean; isError?: boolean; error?: Error; refetch?: () => void };
type Options = { queryKey: unknown[]; queryFn: (context: { signal: AbortSignal }) => Promise<unknown>; enabled?: boolean };
function nodes(tree: unknown): Tree[] {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree;
  return [node, ...nodes(node.props?.children)];
}
function strings(tree: unknown): string {
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (Array.isArray(tree)) return tree.map(strings).join(" ");
  return tree && typeof tree === "object" ? strings((tree as Tree).props?.children) : "";
}
function button(tree: Tree, label: string) {
  const node = nodes(tree).find((node) => node.type === "button" && strings(node).trim() === label);
  assert.ok(node, label); return node.props;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function settle() { await new Promise((resolve) => setTimeout(resolve, 0)); }

// Execute production modules and component event handlers with explicit renderer,
// auth and HTTP doubles. QueryObserver tests below use the actual query library.
function host(initial = "tab=inbox&task_status=completed&task_offset=50") {
  let params = new URLSearchParams(initial);
  const history: string[] = [params.toString()];
  const auth: { token: string | null; user: { user_id: string } | null } = { token: "synthetic-session", user: { user_id: "user-a" } };
  let transport: (path: string, config: { params?: unknown; signal?: AbortSignal }) => Promise<unknown> = async () => [];
  const calls: { path: string; params?: unknown; signal?: AbortSignal }[] = [];
  let list: QueryState = { data: [], refetch() {} };
  let detail: QueryState = { refetch() {} };
  let result: QueryState = { refetch() {} };
  // Parent tests observe this explicit read-scope boundary double. The actual
  // authority hook and output browser are exercised in taskOutputBrowser.test.ts.
  const readScope = { id: "controlled-read-scope", isCurrent: () => Boolean(auth.token && auth.user) };
  const detailScopes: unknown[] = []; const resultScopes: unknown[] = [];
  const selected: Array<string | undefined> = [];
  const resultSelected: Array<string | undefined> = [];
  const lists: unknown[][] = [];
  let slots: unknown[] = []; let cursor = 0; let contentKey: string | undefined;
  const jsx = (type: Tree["type"], props: Tree["props"], key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    react: {
      useState(initial: unknown) { const index = cursor++; if (!(index in slots)) slots[index] = initial; return [slots[index], (value: unknown) => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }]; },
      useCallback: (callback: unknown) => callback,
      useMemo(factory: () => unknown, dependencies: unknown[]) {
        const index = cursor++; const previous = slots[index] as { dependencies: unknown[]; value: unknown } | undefined;
        if (!previous || dependencies.some((value, i) => !Object.is(value, previous.dependencies[i]))) {
          slots[index] = { dependencies, value: factory() };
        }
        return (slots[index] as { value: unknown }).value;
      },
    },
    "react-router-dom": { Link: "a", useSearchParams: () => [params, (update: (current: URLSearchParams) => URLSearchParams) => { params = update(params); history.push(params.toString()); }] },
    "react-i18next": { useTranslation: () => ({ t: (key: string, values?: unknown) => values && typeof values === "object" ? `${key} ${JSON.stringify(values)}` : key, i18n: { language: "en-US" } }) },
    "lucide-react": new Proxy({}, { get: (_, key) => String(key) }),
    "@/components/ui/badge": { Badge: "badge" }, "@/components/ui/button": { Button: "button" },
    "@/components/ui/input": { Input: "input" },
    "@/components/ui/card": { Card: "card", CardContent: "card-content", CardHeader: "card-header", CardTitle: "card-title" },
    "@/lib/clipboard": { copyToClipboard() {} },
    "./TaskTraceLink": { TaskTraceLink: "task-trace-link" },
    "./TaskDiagnostics": { TaskDiagnostics: "task-diagnostics" },
    "./TaskOutputBrowser": { TaskOutputBrowser: "task-output-browser" },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => readScope },
    "./TaskCancellation": { TaskCancellation: "task-cancellation" },
    "../taskNavigation": navigation, "@/pages/tasks/taskNavigation": navigation,
    "@/store/useAuthStore": { useAuthStore: (selector: (state: typeof auth) => unknown) => selector(auth) },
    "@tanstack/react-query": { useQuery: (options: Options) => options },
    "@/lib/api": { api: { get: async (path: string, config: { params?: unknown; signal?: AbortSignal }) => { calls.push({ path, ...config }); return { data: await transport(path, config) }; } } },
  };
  function load(relative: string) {
    const exports: Record<string, (...args: unknown[]) => unknown> = {};
    const source = readFileSync(new URL(relative, import.meta.url), "utf8");
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
      { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, URLSearchParams, setTimeout, Error });
    return exports;
  }
  const api = load("../../api/gateway.ts"); modules["@/api/gateway"] = api;
  const hooks = load("../../hooks/useTasks.ts");
  modules["@/hooks/useTasks"] = {
    useTasks: (...args: unknown[]) => { lists.push(args); return list; },
    useTask: (id?: string, scope?: unknown) => { selected.push(id); detailScopes.push(scope); return detail; },
    useTaskResult: (id?: string, scope?: unknown) => { resultSelected.push(id); resultScopes.push(scope); return result; },
  };
  const inbox = load("./components/TaskInboxTab.tsx");
  const query = load("./components/TaskQueryTab.tsx");
  return { auth, calls, history, lists, selected, resultSelected, readScope, detailScopes, resultScopes,
    params: () => params,
    navigate(value: string) { params = new URLSearchParams(value.replace(/^\?/, "")); },
    setTransport(fn: typeof transport) { transport = fn; },
    setList(value: QueryState) { list = { isLoading: false, isFetching: false, isError: false, ...value }; }, setDetail(value: QueryState) { detail = { isLoading: false, isFetching: false, isError: false, ...value }; }, setResult(value: QueryState) { result = { isLoading: false, isFetching: false, isError: false, ...value }; },
    options(kind: "list" | "detail" | "result", id?: string, offset = 0) {
      return (kind === "list" ? hooks.useTasks(id, offset) : kind === "detail" ? hooks.useTask(id) : hooks.useTaskResult(id)) as Options;
    },
    inbox() { return inbox.TaskInboxTab() as Tree; },
    row(node: Tree) { assert.equal(typeof node.type, "function"); return (node.type as (props: Tree["props"]) => Tree)(node.props); },
    query() { const outer = query.TaskQueryTab() as Tree; if (outer.key !== contentKey) { slots = []; contentKey = outer.key; } cursor = 0; return (outer.type as (props: Tree["props"]) => Tree)(outer.props); },
  };
}
const receipt = (id: string, status = "completed"): Receipt => ({ task_id: id, status, service_id: "existing-service", created_at: "2026-10-12T00:00:00Z" });

test("URL parser accepts supported filters and canonical page windows, rejects malformed offsets", () => {
  for (const invalid of ["-50", "Infinity", "1e3", "50.2", "NaN", "9007199254740992"]) {
    assert.deepEqual(navigation.taskInboxLocation(new URLSearchParams({ task_status: "invented", task_offset: invalid })), { filter: "all", offset: 0 });
  }
  assert.deepEqual(navigation.taskInboxLocation(new URLSearchParams("task_status=failed&task_offset=149")), { filter: "failed", offset: 100 });
});

test("existing API returns an array, sends lookahead limit and offset, and forwards read cancellation", async () => {
  const h = host(); const rows = Array.from({ length: 51 }, (_, i) => receipt(`task-${i}`));
  h.setTransport(async () => rows); const signal = new AbortController().signal;
  const result = await h.options("list", "completed", 50).queryFn({ signal });
  assert.equal(result, rows); assert.equal(h.calls.length, 1); assert.equal(h.calls[0].path, "/api/v1/tasks");
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].params)), { status: "completed", limit: 51, offset: 50 });
  assert.equal(h.calls[0].signal, signal);
});

test("page lookahead never invents a total or exposes the lookahead as an extra row", () => {
  const rows = Array.from({ length: 51 }, (_, i) => i);
  const page = navigation.taskPageWindow(rows, 50);
  assert.deepEqual(page, { rows: rows.slice(0, 50), hasPrevious: true, hasNext: true, start: 51, end: 100 });
  assert.equal(navigation.taskPageWindow(rows.slice(0, 50), 0).hasNext, false);
  assert.equal(navigation.taskPageWindow(rows.slice(0, 1), 100).end, 101);
});

test("actual inbox opens row 51, reads its exact result, and returns to the same filter and offset", () => {
  const h = host("tab=inbox&task_status=completed&task_offset=50&unrelated=kept");
  h.setList({ data: Array.from({ length: 51 }, (_, i) => receipt(`task-${i + 51}`)) });
  const tree = h.inbox(); assert.deepEqual(h.lists.at(-1), ["completed", 50, h.readScope]);
  const rows = nodes(tree).filter((node) => typeof node.type === "function" && node.type.name === "TaskRow");
  assert.equal(rows.length, 50);
  const link = nodes(h.row(rows[0])).find((node) => node.type === "a"); assert.ok(link);
  h.navigate(link.props.to as string); h.setDetail({ data: receipt("task-51") }); h.setResult({ data: { output: "original result" } });
  const detail = h.query(); assert.equal(h.selected.at(-1), "task-51"); assert.equal(h.resultSelected.at(-1), "task-51");
  const output = outputBrowsers(detail); assert.equal(output.length, 1);
  assert.deepEqual(output[0].props.result, { output: "original result" });
  assert.equal(output[0].props.taskId, "task-51"); assert.equal(output[0].props.scope, h.readScope);
  const back = nodes(detail).find((node) => node.type === "a"); assert.ok(back); h.navigate(back.props.to as string);
  assert.equal(h.params().get("tab"), "inbox"); assert.equal(h.params().get("task_id"), null);
  assert.equal(h.params().get("unrelated"), "kept"); h.inbox(); assert.deepEqual(h.lists.at(-1), ["completed", 50, h.readScope]);
});

test("actual filter and next/previous handlers reset only the appropriate window", () => {
  const h = host(); h.setList({ data: Array.from({ length: 51 }, (_, i) => receipt(`task-${i}`)) });
  let tree = h.inbox(); (button(tree, "tasks.inbox.nextPage").onClick as () => void)();
  assert.equal(h.params().get("task_offset"), "100");
  tree = h.inbox(); (button(tree, "tasks.inbox.previousPage").onClick as () => void)();
  assert.equal(h.params().get("task_offset"), "50");
  tree = h.inbox(); (button(tree, "tasks.inbox.filters.failed").onClick as () => void)();
  assert.equal(h.params().get("task_offset"), "0"); assert.equal(h.params().get("task_status"), "failed");
});

test("empty/deleted last page and failed page retain Previous without enabling a guessed next page", () => {
  const h = host(); h.setList({ data: [] }); let tree = h.inbox();
  assert.equal(button(tree, "tasks.inbox.previousPage").disabled, false);
  assert.equal(button(tree, "tasks.inbox.nextPage").disabled, true);
  assert.match(strings(tree), /tasks.inbox.empty/); assert.match(strings(tree), /"start":0,"end":0,"count":0/);
  h.setList({ data: [], isError: true }); tree = h.inbox();
  assert.equal(button(tree, "tasks.inbox.previousPage").disabled, false);
  assert.equal(button(tree, "tasks.inbox.nextPage").disabled, true);
  (button(tree, "tasks.inbox.previousPage").onClick as () => void)(); assert.equal(h.params().get("task_offset"), "0");
});

test("browser navigation replaces selection/input and never fetches an old task's completed result", () => {
  const h = host("tab=query&task_id=task-a&task_status=failed&task_offset=50");
  h.setDetail({ data: receipt("task-a") }); let tree = h.query();
  assert.equal(nodes(tree).find((node) => node.type === "input")?.props.value, "task-a");
  h.navigate("tab=query&task_id=task-b&task_status=failed&task_offset=50"); tree = h.query();
  assert.equal(nodes(tree).find((node) => node.type === "input")?.props.value, "task-b");
  assert.equal(h.resultSelected.at(-1), undefined);
  h.navigate("tab=query&task_id=task-a&task_status=failed&task_offset=50"); h.query();
  assert.equal(h.selected.at(-1), "task-a"); assert.equal(h.resultSelected.at(-1), "task-a");
});

test("manual search writes the same URL selection and preserves inbox return context", () => {
  const h = host("tab=query&task_status=processing&task_offset=100"); let tree = h.query();
  const input = nodes(tree).find((node) => node.type === "input"); assert.ok(input);
  (input.props.onChange as (event: unknown) => void)({ target: { value: " task-manual " } }); tree = h.query();
  const search = nodes(tree).find((node) => node.type === "button" && strings(node).includes("tasks.query")); assert.ok(search);
  (search.props.onClick as () => void)(); assert.equal(h.params().get("task_id"), "task-manual");
  assert.equal(h.params().get("task_status"), "processing"); assert.equal(h.params().get("task_offset"), "100");
});

test("failed or unavailable detail never unlocks a result or displays stale protected task data", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: { ...receipt("task-a"), service_id: "stale-private-service" }, isError: true, error: new Error("Task unavailable") });
  h.setResult({ data: { secret: "stale-private-output" } }); const tree = h.query();
  assert.equal(h.resultSelected.at(-1), undefined); assert.doesNotMatch(strings(tree), /stale-private/); assert.match(strings(tree), /Task unavailable/);
  assert.equal(outputBrowsers(tree).length, 0);
});

test("result-read failure exposes a GET retry and hides an old result without resubmitting work", () => {
  const h = host("tab=query&task_id=task-a"); let retried = 0;
  h.setDetail({ data: receipt("task-a") }); h.setResult({ data: { stale: "old-output" }, isError: true, refetch() { retried++; } });
  const tree = h.query(); assert.doesNotMatch(strings(tree), /old-output/);
  assert.equal(outputBrowsers(tree).length, 0);
  (button(tree, "tasks.states.retry").onClick as () => void)(); assert.equal(retried, 1); assert.equal(h.calls.length, 0);
});

test("opaque task IDs are encoded as a single path segment for detail and result", async () => {
  const h = host(); const id = "opaque/id?query#fragment"; const signal = new AbortController().signal;
  await h.options("detail", id).queryFn({ signal }); await h.options("result", id).queryFn({ signal });
  assert.equal(h.calls[0].path, `/api/v1/tasks/${encodeURIComponent(id)}`);
  assert.equal(h.calls[1].path, `/api/v1/tasks/${encodeURIComponent(id)}/result`);
});

for (const kind of ["list", "detail", "result"] as const) {
  test(`${kind} query isolates accounts and missing authentication without placing credentials in keys`, () => {
    const h = host(); const first = h.options(kind, "task-a");
    h.auth.user = { user_id: "user-b" }; const second = h.options(kind, "task-a");
    assert.notDeepEqual(first.queryKey, second.queryKey); assert.doesNotMatch(JSON.stringify(first.queryKey), /synthetic-session/);
    h.auth.token = null; assert.equal(h.options(kind, "task-a").enabled, false);
    h.auth.token = "synthetic-session"; h.auth.user = null; assert.equal(h.options(kind, "task-a").enabled, false);
  });

  test(`${kind} production query options keep late prior-account responses out of the current QueryObserver`, async () => {
    const h = host(); const old = deferred<unknown>(); const current = deferred<unknown>();
    h.setTransport(() => old.promise); const options = h.options(kind, "task-a");
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
    const observer = new QueryObserver(client, options); const stop = observer.subscribe(() => {});
    try {
      h.auth.user = { user_id: "user-b" }; h.setTransport(() => current.promise);
      observer.setOptions(h.options(kind, "task-a")); assert.equal(observer.getCurrentResult().data, undefined);
      current.resolve({ account: "user-b" }); await settle(); old.resolve({ account: "user-a" }); await settle();
      assert.deepEqual(observer.getCurrentResult().data, { account: "user-b" });
    } finally { stop(); client.clear(); }
  });
}

test("late prior-page reads cannot replace a newer filter/page under the actual query library", async () => {
  const h = host(); const old = deferred<unknown>(); const current = deferred<unknown>(); h.setTransport(() => old.promise);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const observer = new QueryObserver(client, h.options("list", "completed", 50)); const stop = observer.subscribe(() => {});
  try {
    h.setTransport(() => current.promise); observer.setOptions(h.options("list", "failed", 0));
    current.resolve([receipt("failed-current", "failed")]); await settle(); old.resolve([receipt("completed-stale")]); await settle();
    assert.deepEqual(observer.getCurrentResult().data, [receipt("failed-current", "failed")]);
  } finally { stop(); client.clear(); }
});

// These fixtures are the public JSON projection: bytes are base64, not the
// private storage integer array; null/false/zero/empty strings remain explicit.
const projectedResponse = {
  request_id: "fixture-request", task_id: "task-a", status: "success",
  outputs: ["", null, false, 0, "AP8="].map((data, index) => ({
    type: index === 4 ? "image" : "text", data, url: null,
    mime_type: index === 4 ? "image/png" : null, metadata: null,
  })),
  session_id: null, usage: null, error: null, metadata: null,
  timestamp: "2026-10-12T00:00:00Z",
};
function outputBrowsers(tree: Tree) {
  return nodes(tree).filter((node) => node.type === "task-output-browser");
}
function resultText(tree: Tree) {
  return nodes(tree).filter((node) => node.type === "pre").map((node) => strings(node));
}

test("completed canonical response preserves empty, null, false, zero and base64 outputs through the existing result endpoint", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: { ...receipt("task-a"), request_id: projectedResponse.request_id, result: projectedResponse } });
  h.setResult({ data: projectedResponse });
  const tree = h.query();
  assert.equal(h.resultSelected.at(-1), "task-a");
  const output = outputBrowsers(tree); assert.equal(output.length, 1);
  assert.equal(output[0].props.result, projectedResponse);
  assert.equal(output[0].props.taskId, "task-a");
  assert.equal(output[0].props.requestId, projectedResponse.request_id);
  assert.equal(output[0].props.scope, h.readScope);
  assert.deepEqual(resultText(tree), []);
  assert.doesNotMatch(strings(tree), /tasks.partialResult/);
});

for (const status of ["failed", "cancelled"]) {
  for (const [label, value] of Object.entries({ response: projectedResponse, empty: "", false: false, zero: 0, object: { partial: "saved output" }, array: [] })) {
    test(`${status} receipt displays retained ${label} public output without inferring success or fetching a typed result`, () => {
      const h = host("tab=query&task_id=task-a");
      h.setDetail({ data: { ...receipt("task-a", status), result: value, error: "safe-terminal-error" } });
      h.setResult({ data: { stale: "unrelated-success" } });
      let tree = h.query();
      assert.deepEqual(resultText(tree), []);
      (button(tree, "tasks.viewPartialResult").onClick as () => void)(); tree = h.query();
      assert.deepEqual(resultText(tree), [JSON.stringify(value, null, 2)]);
      assert.match(strings(tree), /tasks.partialResult/);
      assert.match(strings(tree), /safe-terminal-error/);
      const badge = nodes(tree).find((node) => typeof node.type === "function" && node.type.name === "StatusBadge");
      assert.equal(badge?.props.status, status);
      assert.equal(h.resultSelected.at(-1), undefined);
      assert.doesNotMatch(strings(tree), /unrelated-success/);
      assert.equal(outputBrowsers(tree).length, 0);
      assert.equal(h.calls.length, 0);
    });
  }
  for (const value of [undefined, null]) {
    test(`${status} receipt with ${String(value)} has no invented partial result`, () => {
      const h = host("tab=query&task_id=task-a");
      h.setDetail({ data: { ...receipt("task-a", status), result: value } });
      const tree = h.query(); assert.deepEqual(resultText(tree), []);
      assert.doesNotMatch(strings(tree), /tasks.partialResult/);
    });
  }
}

for (const status of ["pending", "processing", "unknown"]) {
  test(`${status} receipt never exposes retained data as a terminal partial result`, () => {
    const h = host("tab=query&task_id=task-a");
    h.setDetail({ data: { ...receipt("task-a", status), result: { private: "unfinished" } } });
    const tree = h.query();
    assert.deepEqual(resultText(tree), []); assert.equal(h.resultSelected.at(-1), undefined);
    assert.equal(outputBrowsers(tree).length, 0);
  });
}

test("partial output cannot survive an unavailable or wrong-ID detail, or bleed into a newly selected task", () => {
  const h = host("tab=query&task_id=task-a");
  const data = { ...receipt("task-a", "failed"), result: { private: "protected-output" } };
  h.setDetail({ data });
  (button(h.query(), "tasks.viewPartialResult").onClick as () => void)();
  assert.equal(resultText(h.query()).length, 1);
  h.setDetail({ data, isError: true }); assert.deepEqual(resultText(h.query()), []);
  h.setDetail({ data }); h.navigate("tab=query&task_id=task-b"); assert.deepEqual(resultText(h.query()), []);
  h.setDetail({ data: receipt("task-b", "pending") }); assert.deepEqual(resultText(h.query()), []);
});

test("completed result-read errors do not fall back to a stale detail result", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: { ...receipt("task-a"), request_id: projectedResponse.request_id, result: projectedResponse } });
  h.setResult({ data: projectedResponse, isError: true });
  const tree = h.query(); assert.deepEqual(resultText(tree), []);
  assert.match(strings(tree), /tasks.states.retry/);
  assert.equal(outputBrowsers(tree).length, 0);
});


test("partial output serializes only on reveal and memoizes repeated renders of the same result", () => {
  const h = host("tab=query&task_id=task-a"); let serializations = 0;
  // toJSON is an instrumentation seam; HTTP JSON itself never supplies functions.
  const result = { toJSON() { serializations++; return { retained: "large-output-fixture" }; } };
  h.setDetail({ data: { ...receipt("task-a", "failed"), result } });
  let tree = h.query(); h.query(); assert.equal(serializations, 0);
  (button(tree, "tasks.viewPartialResult").onClick as () => void)();
  tree = h.query(); h.query(); assert.equal(serializations, 1);
  assert.equal(button(tree, "tasks.hidePartialResult")["aria-expanded"], true);
  (button(tree, "tasks.hidePartialResult").onClick as () => void)();
  tree = h.query(); assert.deepEqual(resultText(tree), []); assert.equal(serializations, 1);
  h.navigate("tab=query&task_id=task-b");
  h.setDetail({ data: { ...receipt("task-b", "cancelled"), result } });
  tree = h.query(); assert.deepEqual(resultText(tree), []); assert.equal(serializations, 1);
  assert.equal(button(tree, "tasks.viewPartialResult")["aria-expanded"], false);
});

for (const [label, value] of Object.entries({ false: false, zero: 0, empty: "", array: [], object: {}, shaped: { outputs: [], status: "success" }, nullString: "null", nested: { data: null, bytes: "AP8=" } })) {
  test(`completed ${label} detail output remains independently inspectable when typed result is unavailable`, () => {
    const h = host("tab=query&task_id=task-a"); let retries = 0;
    h.setDetail({ data: { ...receipt("task-a"), result: value } });
    h.setResult({ data: { stale: "old-result" }, isError: true, error: new Error("404"), refetch() { retries++; } });
    let tree = h.query(); assert.deepEqual(resultText(tree), []);
    assert.match(strings(tree), /tasks.detailResultSnapshot/);
    assert.match(strings(tree), /tasks.detailResultProvenance/);
    (button(tree, "tasks.viewDetailResult").onClick as () => void)(); tree = h.query();
    assert.deepEqual(resultText(tree), [JSON.stringify(value, null, 2)]);
    assert.equal(h.resultSelected.at(-1), "task-a");
    assert.equal(nodes(tree).filter((node) => node.props.role === "alert").length, 1);
    (button(tree, "tasks.states.retry").onClick as () => void)(); assert.equal(retries, 1);
    assert.doesNotMatch(strings(tree), /old-result/); assert.equal(h.calls.length, 0);
    assert.equal(outputBrowsers(tree).length, 0);
  });
}

for (const state of ["loading", "success", "403", "404", "500", "network"]) {
  test(`completed snapshot provenance does not infer type or availability from typed result ${state}`, () => {
    const h = host("tab=query&task_id=task-a");
    const value = { outputs: [{ data: "receipt-observation" }] };
    h.setDetail({ data: { ...receipt("task-a"), result: value } });
    const typed = { output: "typed-observation" };
    h.setResult(state === "success" ? { data: typed }
      : state === "loading" ? { isLoading: true } : { isError: true, error: new Error(state) });
    let tree = h.query(); (button(tree, "tasks.viewDetailResult").onClick as () => void)(); tree = h.query();
    assert.match(strings(tree), /tasks.detailResultProvenance/);
    assert.deepEqual(resultText(tree), [JSON.stringify(value, null, 2)]);
    const output = outputBrowsers(tree);
    assert.equal(output.length, state === "success" ? 1 : 0);
    if (state === "success") assert.equal(output[0].props.result, typed);
    if (state === "loading") assert.ok(nodes(tree).some((node) => node.props.role === "status"));
    if (!["loading", "success"].includes(state)) assert.ok(nodes(tree).some((node) => node.props.role === "alert"));
  });
}

for (const value of [undefined, null]) {
  test(`completed ${String(value)} detail result does not invent an inspectable value`, () => {
    const h = host("tab=query&task_id=task-a");
    h.setDetail({ data: { ...receipt("task-a"), result: value } });
    h.setResult({ isError: true }); const tree = h.query();
    assert.doesNotMatch(strings(tree), /tasks.detailResultSnapshot/); assert.deepEqual(resultText(tree), []);
  });
}

test("completed detail output reveal is lazy, memoized, refresh-aware and reset by task navigation", () => {
  const h = host("tab=query&task_id=task-a"); let serializations = 0;
  const value = { toJSON() { serializations++; return { snapshot: "observed" }; } };
  h.setDetail({ data: { ...receipt("task-a"), result: value } }); h.setResult({ isError: true });
  let tree = h.query(); h.query(); assert.equal(serializations, 0);
  (button(tree, "tasks.viewDetailResult").onClick as () => void)(); tree = h.query(); h.query();
  assert.equal(serializations, 1); assert.equal(button(tree, "tasks.hideDetailResult")["aria-expanded"], true);
  h.setDetail({ data: { ...receipt("task-a"), result: { newer: false } } });
  assert.deepEqual(resultText(h.query()), [JSON.stringify({ newer: false }, null, 2)]);
  h.navigate("tab=query&task_id=task-b"); h.setDetail({ data: { ...receipt("task-b"), result: value } });
  tree = h.query(); assert.deepEqual(resultText(tree), []); assert.equal(serializations, 1);
  assert.equal(button(tree, "tasks.viewDetailResult")["aria-expanded"], false);
});

test("completed snapshot is suppressed for errored, unavailable and wrong-ID detail, including account transition", () => {
  const h = host("tab=query&task_id=task-a"); const data = { ...receipt("task-a"), result: { secret: "first-account" } };
  h.setDetail({ data }); h.setResult({ isError: true });
  (button(h.query(), "tasks.viewDetailResult").onClick as () => void)(); assert.equal(resultText(h.query()).length, 1);
  h.setDetail({ data, isError: true }); assert.deepEqual(resultText(h.query()), []);
  h.setDetail({ data: { ...data, task_id: "task-other" } }); assert.deepEqual(resultText(h.query()), []);
  // The actual QueryObserver account-key isolation is exercised above; here the
  // component receives its no-data transition rather than an invented cached receipt.
  h.auth.user = { user_id: "user-b" }; h.setDetail({ isLoading: true });
  assert.deepEqual(resultText(h.query()), []); assert.equal(h.resultSelected.at(-1), undefined);
  h.setDetail({ data: { ...receipt("task-a"), result: { current: "second-account" } } });
  assert.doesNotMatch(strings(h.query()), /first-account/);
});

test("diagnostic handoff requires a successful exact-identity receipt and refreshes only that read", () => {
  const h = host("tab=query&task_id=task-a"); let reads = 0;
  h.setResult({ isError: true });
  for (const detail of [{}, { data: receipt("task-b") }, { data: receipt("task-a"), isError: true }, { data: receipt("task-a"), isLoading: true }]) {
    h.setDetail(detail); assert.ok(!nodes(h.query()).some(node => node.type === "task-diagnostics"));
  }
  h.setDetail({ data: receipt("task-a"), refetch() { reads++; } });
  const panel = nodes(h.query()).find(node => node.type === "task-diagnostics"); assert.ok(panel);
  assert.equal(panel.props.result, "unavailable"); assert.equal((panel.props.receipt as Receipt).status, "completed");
  (panel.props.refresh as () => void)(); assert.equal(reads, 1); assert.equal(h.calls.length, 0);
});

test("diagnostic preview ownership resets when task status or result observation changes", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: receipt("task-a", "processing") }); h.setResult({});
  const before = nodes(h.query()).find(node => node.type === "task-diagnostics"); assert.ok(before);
  h.setDetail({ data: receipt("task-a", "completed") }); h.setResult({ isLoading: true });
  const completed = nodes(h.query()).find(node => node.type === "task-diagnostics"); assert.ok(completed);
  assert.notEqual(before.key, completed.key);
  h.setResult({ isError: true }); const failure = nodes(h.query()).find(node => node.type === "task-diagnostics"); assert.ok(failure);
  assert.notEqual(completed.key, failure.key);
});

test("cancellation visit persists while only exact successful detail unlocks status", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: receipt("task-a", "processing") });
  const control = nodes(h.query()).find(node => node.type === "task-cancellation");
  assert.ok(control); assert.equal(control.props.taskId, "task-a"); assert.equal(control.props.status, "processing");
  for (const detail of [{ data: receipt("other", "processing") }, { data: receipt("task-a", "processing"), isError: true }]) {
    h.setDetail(detail); const retained = nodes(h.query()).find(node => node.type === "task-cancellation");
    assert.ok(retained); assert.equal(retained.props.taskId, "task-a"); assert.equal(retained.props.status, undefined);
  }
});

test("the same current read scope reaches both queries and the typed output boundary", () => {
  const h = host("tab=query&task_id=task-a");
  h.setDetail({ data: { ...receipt("task-a"), request_id: "fixture-request" } });
  h.setResult({ data: projectedResponse }); const tree = h.query();
  assert.equal(h.detailScopes.at(-1), h.readScope);
  assert.equal(h.resultScopes.at(-1), h.readScope);
  const output = outputBrowsers(tree); assert.equal(output.length, 1);
  assert.equal(output[0].props.taskId, "task-a");
  assert.equal(output[0].props.requestId, "fixture-request");
  assert.equal(output[0].props.result, projectedResponse);
  assert.equal(output[0].props.scope, h.readScope);
});


test("actual task detail mounts trace entry only for exact successful detail observation", () => {
  const h = host(); h.navigate("?tab=query&task_id=task-a&task_status=failed&task_offset=50");
  h.setDetail({ data: { ...receipt("task-a"), request_id: "request-exact" } });
  const entry = nodes(h.query()).find(node => node.type === "task-trace-link"); assert.ok(entry); assert.equal(entry.props.taskId, "task-a"); assert.equal(entry.props.requestId, "request-exact");
  for (const detail of [{ data: receipt("other") }, { data: receipt("task-a"), isError: true }, { data: receipt("task-a"), isLoading: true }]) {
    h.setDetail(detail); assert.equal(nodes(h.query()).some(node => node.type === "task-trace-link"), false);
  }
});
