import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { MutationObserver, QueryClient } from "@tanstack/react-query";
import * as navigation from "./traceNavigation.ts";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

// Execute the complete production page with deterministic React/router/query transports.
// This tests its real closures and effect wiring without a browser or service listener.
type Row = { trace_id: string; trace_family: string; scores_count: number };
type Tree = { type: unknown; props: Record<string, unknown> };
type Query = { queryKey: unknown[]; queryFn: () => unknown; enabled?: boolean };
type Mutation = {
  mutationFn: (input: unknown) => Promise<unknown>;
  onSuccess?: (result: unknown, input: unknown) => void;
  onError?: (error: unknown, input: unknown) => void;
};
const row = (id: string, family = "assistant"): Row => ({ trace_id: id, trace_family: family, scores_count: 0 });
function find(tree: unknown, type: string): Tree {
  if (tree && typeof tree === "object") {
    if ((tree as Tree).type === type) return tree as Tree;
    for (const value of Object.values(tree)) {
      try { return find(value, type); } catch { /* Continue across rendered props. */ }
    }
  }
  throw new Error(`Missing rendered ${type}`);
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

function host(initial = "tab=traces&family=assistant&trace_id=linked", options: {
  evaluationFixture?: boolean; realMutations?: boolean; cryptoMode?: "missing" | "throwing";
} = {}) {
  const optionsForHost = options;
  const slots: unknown[] = [];
  let cursor = 0;
  let dirty = false;
  let effects: (() => void)[] = [];
  const cleanups = new Map<number, () => void>();
  let search = new URLSearchParams(initial);
  let location = { key: "location-1" };
  let sequence = 1;
  let scopeSequence = 0;
  let queries: Query[] = [];
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false, gcTime: Infinity } } });
  const mutationObservers = new Map<number, MutationObserver<unknown, Error, unknown>>();
  const mutationUnsubscribers: (() => void)[] = [];
  const notices: unknown[] = [];
  const reads: unknown[][] = [];
  const exports: unknown[][] = [];
  const refreshes: unknown[][] = [];
  const authSubscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const auth = { token: "fixture-session-A", isAuthenticated: true, user: { user_id: "owner-A", roles: ["member"], permissions: ["console:eval:run"], tier: "normal" } };
  const runRows = ["candidate-A", "candidate-B", "baseline"].map((run_id) => ({ run_id, status: "succeeded", created_at: "2026-10-12T12:00:00Z" }));
  const experiment = { experiment_id: "experiment", baseline_run_id: "baseline", runs: runRows };
  const experimentList = { experiments: [experiment] };
  let compareRead: (...args: unknown[]) => Promise<unknown> = async (baseline_run_id, candidate_run_id) => ({ baseline_run_id, candidate_run_id });
  const comparisons: unknown[][] = [];
  let rows = [row("unrelated")];
  let listLoading = false;
  let detailError: Error | null = null;
  let exportRead: (...args: unknown[]) => Promise<unknown> = async (id) => ({ trace_id: id, format: "openinference", payload: { summary: id }, redaction_policy: {} });
  const depsEqual = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const react = {
    useState(initialValue: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initialValue === "function" ? initialValue() : initialValue;
      return [slots[index], (next: unknown) => {
        const value = typeof next === "function" ? next(slots[index]) : next;
        if (!Object.is(value, slots[index])) { slots[index] = value; dirty = true; }
      }];
    },
    useRef(initialValue: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initialValue }); },
    useMemo(compute: () => unknown, deps: unknown[]) {
      const index = cursor++;
      const old = slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!old || !depsEqual(old.deps, deps)) slots[index] = { deps, value: compute() };
      return (slots[index] as { value: unknown }).value;
    },
    useEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      const old = slots[index] as unknown[] | undefined;
      if (!depsEqual(old, deps)) {
        slots[index] = deps;
        effects.push(() => {
          cleanups.get(index)?.();
          const cleanup = effect();
          if (cleanup) cleanups.set(index, cleanup);
          else cleanups.delete(index);
        });
      }
    },
  };
  const realMutations = options.realMutations;
  Object.assign(react, { useCallback: (fn: unknown, deps: unknown[]) => react.useMemo(() => fn, deps) });
  const cryptoFixture = options.cryptoMode === "missing" ? {} : {
    randomUUID: () => {
      if (options.cryptoMode === "throwing") throw new Error("insecure context");
      return `opaque-access-${++scopeSequence}`;
    },
  };
  const uuidModule: { generateUUID?: () => string } = {};
  const uuidSource = readFileSync(new URL("../../lib/utils.ts", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(uuidSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
    exports: uuidModule, require: () => ({}), crypto: cryptoFixture, Math,
  });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    "@/components/deferComponent": { deferComponent: (_loader: unknown, name: string) => name },
    react: { ...react, Suspense: "Suspense", useLayoutEffect: react.useEffect }, "react/jsx-runtime": { jsx, jsxs: jsx },
    antd: { ...Object.fromEntries(["Alert", "Button", "Descriptions", "Input", "InputNumber", "Pagination", "Progress", "Select", "Space", "Tabs"].map((key) => [key, key])), App: { useApp: () => ({ message: { success: (v: unknown) => notices.push(v), error: (v: unknown) => notices.push(v) }, modal: {} }) } },
    "lucide-react": named,
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "react-router-dom": {
      Link: "Link", useLocation: () => location,
      useSearchParams: () => [search, (next: URLSearchParams) => { search = next; location = { key: `location-${++sequence}` }; dirty = true; }],
    },
    "@/store/useAuthStore": { usePermission: () => true, useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (callback: (state: typeof auth, previous: typeof auth) => void) => { authSubscribers.add(callback); return () => authSubscribers.delete(callback); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/utils": uuidModule,
    "@tanstack/react-query": {
      useQueryClient: () => ({ invalidateQueries: async () => {} }),
      useQuery(options: Query) {
        cursor++; queries.push(options);
        if (optionsForHost.evaluationFixture) {
          if (options.queryKey[1] === "experiments") return { data: experimentList };
          if (options.queryKey[1] === "experiment") return { data: options.enabled ? experiment : undefined };
          if (options.queryKey[1] === "run") return { data: runRows.find((run) => run.run_id === options.queryKey[2]) };
          if (options.queryKey[1] === "run-results") return { data: { run_id: options.queryKey[2], cases: [], total: 0 } };
        }
        if (options.queryKey[1] === "traces") return { data: listLoading ? undefined : { traces: rows, total: rows.length }, isLoading: listLoading, isFetching: false, refetch: () => { refreshes.push(options.queryKey); } };
        if (options.queryKey[1] === "trace-detail") return { error: detailError, data: undefined, isLoading: false, isFetching: false, refetch: () => { refreshes.push(options.queryKey); } };
        return {};
      },
      useMutation(options: Mutation) {
        const index = cursor++; slots[index] = options;
        if (realMutations) {
          let observer = mutationObservers.get(index);
          if (!observer) {
            observer = new MutationObserver(queryClient, options);
            mutationObservers.set(index, observer);
            mutationUnsubscribers.push(observer.subscribe(() => { dirty = true; }));
          }
          observer.setOptions(options);
          return {
            mutateAsync: (input: unknown) => observer.mutate(input),
            mutate: (input: unknown) => { void observer.mutate(input).catch(() => {}); },
            isPending: observer.getCurrentResult().isPending,
          };
        }
        const mutateAsync = async (input: unknown) => {
          try {
            const result = await options.mutationFn(input);
            (slots[index] as Mutation).onSuccess?.(result, input);
            return result;
          } catch (error) { (slots[index] as Mutation).onError?.(error, input); }
        };
        return { mutateAsync, mutate: (input: unknown) => { void mutateAsync(input); }, isPending: false };
      },
    },
    "@/api/eval": new Proxy({}, { get: (_target, key) => {
      if (key === "compareEvalExperimentRuns") return (...args: unknown[]) => { comparisons.push(args); return compareRead(...args); };
      if (key === "getAgentTraceDetail") return (...args: unknown[]) => { reads.push(args); if (detailError) throw detailError; return { trace: row(String(args[0]), String(args[1])) }; };
      if (key === "exportAgentTrace") return (...args: unknown[]) => { exports.push(args); return exportRead(...args); };
      return async () => ({});
    } }),
    "@/api/agents": named,
    "./traceNavigation": navigation,
    "./reviewQueue": { kbDeepLinkNavigationKey: (key: string, id: string) => `${key}:${id}`, latestReviewCandidates: () => [] },
    "./agentVersionTarget": { agentVersionTarget: () => ({}), buildExperimentTargetConfig: () => ({}) },
    "./styles.css": {},
  };
  for (const [id, path] of [
    ["@/hooks/useAuthenticatedReadScope", "../../hooks/useAuthenticatedReadScope.ts"],
    ["./useEvalRunActions", "./useEvalRunActions.ts"],
  ]) {
    const exported = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
    }).outputText, { exports: exported, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; } });
    modules[id] = exported;
  }
  const source = readFileSync(new URL("./index.tsx", import.meta.url), "utf8");
  const compiled: { EvalPage?: () => Tree } = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
    exports: compiled,
    require: (id: string) => { if (id.startsWith("./components/")) return named; assert.ok(id in modules, id); return modules[id]; },
    URLSearchParams, crypto: cryptoFixture, Date, Set, Map, Error,
  });
  return {
    auth, notices, reads, exports, refreshes, comparisons,
    changeAuth(token: string) { const previous = { ...auth }; auth.token = token; authSubscribers.forEach(callback => callback(auth, previous)); },
    comparison() { return find(this.render(), "ExperimentRunComparison").props.comparison; },
    control(type: string, property: string, value: unknown) {
      function search(tree: unknown): Tree | undefined {
        if (!tree || typeof tree !== "object") return;
        const node = tree as Tree;
        if (node.type === type && node.props[property] === value) return node;
        for (const item of Object.values(tree)) { const found = search(item); if (found) return found; }
      }
      const found = search(this.render()); assert.ok(found, `Missing ${type} ${property}=${value}`); return found.props;
    },
    clickCompare() {
      const button = this.control("Button", "children", "eval.workbench.compareRuns");
      assert.equal(button.disabled, false, "Compare must be enabled");
      assert.equal(button.loading, false, "Compare must not retain abandoned request loading");
      (button.onClick as () => void)();
    },
    setCompare(fn: typeof compareRead) { compareRead = fn; },
    unmount() {
      cleanups.forEach((cleanup) => cleanup()); cleanups.clear();
      mutationUnsubscribers.forEach((unsubscribe) => unsubscribe());
    },
    render() {
      let tree!: Tree;
      for (let i = 0; i < 20; i++) {
        dirty = false; cursor = 0; queries = []; effects = [];
        tree = compiled.EvalPage!();
        effects.forEach((effect) => effect());
        if (!dirty) return tree;
      }
      throw new Error("Production page did not settle");
    },
    shell() { return find(this.render(), "TraceExplorerShell").props; },
    results() { return find(this.render(), "ExperimentRunResults").props; },
    navigate(query: string, key = `location-${++sequence}`) { search = new URLSearchParams(query); location = { key }; },
    params: () => new URLSearchParams(search),
    queries: () => queries,
    setRows(next: Row[], loading = false) { rows = next; listLoading = loading; },
    setDetailError(error: Error | null) { detailError = error; },
    setExport(fn: typeof exportRead) { exportRead = fn; },
  };
}

test("the production page reads the linked trace even before or outside the list page", () => {
  const h = host(); h.setRows([], true);
  assert.equal(h.shell().selectedTraceId, "linked");
  h.queries().find((q) => q.queryKey[1] === "trace-detail")!.queryFn();
  assert.deepEqual(h.reads, [["linked", "assistant"]]);
  h.setRows([row("unrelated")]);
  assert.equal(h.shell().selectedTraceId, "linked");
});

for (const status of [403, 404]) {
  test(`the production page retains exact ${status} failure without selecting another trace`, () => {
    const h = host(); const failure = new Error(`HTTP ${status}`); h.setDetailError(failure);
    const shell = h.shell();
    assert.equal(shell.selectedTraceId, "linked"); assert.equal(shell.detailError, failure);
    assert.equal(shell.detail, undefined);
    assert.throws(() => h.queries().find((q) => q.queryKey[1] === "trace-detail")!.queryFn(), failure);
    assert.deepEqual(h.reads, [["linked", "assistant"]]);
  });
}

test("manual row selection survives rerenders and Back/Forward restores the exact URL target", () => {
  const h = host("tab=traces&family=rag&trace_id=linked&dataset_id=kb&session_id=session&offset=100");
  (h.shell().onSelectTrace as (id: string) => void)("manual");
  assert.equal(h.shell().selectedTraceId, "manual");
  assert.equal(h.params().get("trace_id"), "manual");
  assert.equal(h.params().get("family"), "rag");
  assert.equal(h.params().get("dataset_id"), "kb");
  assert.equal(h.params().get("session_id"), "session");
  assert.equal(h.params().get("offset"), "100");
  h.navigate("tab=traces&family=assistant&trace_id=original", "back");
  assert.equal(h.shell().selectedTraceId, "original"); assert.equal(h.shell().activeTraceFamily, "assistant");
  h.navigate("tab=traces&family=rag&trace_id=manual", "forward");
  assert.equal(h.shell().selectedTraceId, "manual"); assert.equal(h.shell().activeTraceFamily, "rag");
});

test("cross-family result navigation writes one coherent target and keeps source parameters", () => {
  const h = host("tab=runs&family=assistant&trace_id=old&dataset_id=kb");
  const results = h.results();
  assert.equal(typeof results.onOpenTrace, "function");
  (results.onOpenTrace as (item: unknown) => void)({ candidate_trace_id: "rag-result", trace: { trace_family: "rag" } });
  const shell = h.shell();
  assert.equal(shell.activeTraceFamily, "rag"); assert.equal(shell.selectedTraceId, "rag-result");
  assert.equal(h.params().get("tab"), "traces"); assert.equal(h.params().get("dataset_id"), "kb");
});

test("family changes discard the previous exact target but keep unrelated URL context", () => {
  const h = host("tab=traces&family=assistant&trace_id=old&dataset_id=kb");
  (h.shell().onTraceFamilyChange as (family: string) => void)("rag");
  assert.equal(h.params().has("trace_id"), false);
  assert.equal(h.shell().activeTraceFamily, "rag");
  assert.equal(h.params().get("dataset_id"), "kb");
});

test("late export cannot replace another selected trace's preview", async () => {
  const h = host(undefined, { realMutations: true }); const first = deferred<unknown>(); h.setExport(() => first.promise);
  (h.shell().onExport as () => void)(); await settle();
  (h.shell().onSelectTrace as (id: string) => void)("second");
  h.setExport(async (id) => ({ trace_id: id, format: "openinference", payload: "second-safe-preview" }));
  (h.shell().onExport as () => void)(); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "second-safe-preview");
  first.resolve({ trace_id: "linked", format: "openinference", payload: "first-private-preview" }); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "second-safe-preview");
  assert.deepEqual(h.exports, [["linked", "openinference", "assistant"], ["second", "openinference", "assistant"]]);
});

for (const change of ["account", "session", "permissions", "navigation", "family", "response"]) {
  test(`export source guard rejects ${change} changes in the actual page`, async () => {
    const h = host(); const response = deferred<unknown>(); h.setExport(() => response.promise);
    const firstScope = h.shell().traceAccessKey;
    (h.shell().onExport as () => void)();
    if (change === "account") h.auth.user.user_id = "owner-B";
    if (change === "session") h.auth.token = "fixture-session-B";
    if (change === "permissions") h.auth.user.permissions = [];
    if (change === "navigation") h.navigate("tab=traces&family=assistant&trace_id=linked", "return-visit");
    if (change === "family") h.navigate("tab=traces&family=rag&trace_id=linked");
    h.render();
    response.resolve({ trace_id: change === "response" ? "wrong-trace" : "linked", format: "openinference", payload: "private-preview" }); await settle();
    assert.equal(h.shell().exportPreview, null);
    if (["account", "session", "permissions"].includes(change)) {
      assert.notEqual(h.shell().traceAccessKey, firstScope);
      for (const query of h.queries().filter((q) => ["traces", "trace-detail", "summary", "dashboard"].includes(String(q.queryKey[1])))) {
        assert.ok(query.queryKey.includes(h.shell().traceAccessKey));
        assert.ok(!query.queryKey.includes(h.auth.token));
      }
    }
  });
}

test("already rendered export is cleared synchronously when selected trace or account changes", async () => {
  const h = host(); (h.shell().onExport as () => void)(); await settle();
  assert.ok(h.shell().exportPreview);
  (h.shell().onSelectTrace as (id: string) => void)("second");
  assert.equal(h.shell().exportPreview, null);
  (h.shell().onExport as () => void)(); await settle(); assert.ok(h.shell().exportPreview);
  h.auth.token = "different-session";
  assert.equal(h.shell().exportPreview, null);
});

function renderThread(selectedTraceId: string | undefined) {
  let query!: Query;
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx }, react: { useMemo: (fn: () => unknown) => fn() },
    antd: named, "lucide-react": named,
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en" } }) },
    "@tanstack/react-query": { useQuery: (options: Query) => { query = options; return { data: { traces: [row("different-turn")] } }; } },
    "@/api/eval": { getTraceThread: (id: string) => id },
    "./tracePresentation": { formatDate: () => "date", formatDuration: () => "duration", locatorText: () => "locator", traceLocator: () => ({}), traceTurn: () => 1 },
  };
  const source = readFileSync(new URL("./components/ThreadView.tsx", import.meta.url), "utf8");
  const compiled: { ThreadView?: (props: unknown) => Tree } = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
    exports: compiled, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; },
  });
  const tree = compiled.ThreadView!({ selectedTraceId, threadId: "thread", traceAccessKey: "safe-account-key", fallbackTraces: [] });
  return { tree, query };
}

test("thread inspection never substitutes a different turn for an explicit unavailable selection", () => {
  const explicit = renderThread("missing-turn");
  assert.equal(find(explicit.tree, "Empty").type, "Empty");
  assert.throws(() => find(explicit.tree, "Button"), /Missing rendered Button/);
  assert.deepEqual(Array.from(explicit.query.queryKey), ["eval", "thread", "safe-account-key", "thread"]);
  assert.equal(explicit.query.queryFn(), "thread");
  assert.equal(find(renderThread(undefined).tree, "Button").type, "Button");
  assert.equal(find(renderThread("different-turn").tree, "Button").type, "Button");
});


test("manual refresh retries the exact diagnostic read and never refreshes an empty target", () => {
  const h = host(); h.setDetailError(new Error("temporary unavailable"));
  (h.shell().onRefresh as () => void)();
  assert.equal(h.refreshes.length, 2);
  assert.equal(h.refreshes[1][1], "trace-detail");
  assert.equal(h.refreshes[1].at(-1), "linked");
  assert.equal(h.shell().selectedTraceId, "linked");
  h.navigate("tab=traces"); h.setRows([]);
  (h.shell().onRefresh as () => void)();
  assert.equal(h.refreshes.length, 3);
  assert.equal(h.refreshes[2][1], "traces");
});

test("late export failures do not report another selected trace's error", async () => {
  const h = host(); const response = deferred<unknown>();
  h.setExport(async () => { await response.promise; throw new Error("old trace error"); });
  (h.shell().onExport as () => void)();
  (h.shell().onSelectTrace as (id: string) => void)("second"); h.render();
  response.resolve(null); await settle();
  assert.deepEqual(h.notices, []);
  h.setExport(async () => { throw new Error("current trace error"); });
  (h.shell().onExport as () => void)(); await settle();
  assert.deepEqual(h.notices, ["current trace error"]);
});


test("POP restoring the original history key restores the target without reviving its old export", async () => {
  const original = "tab=traces&family=assistant&trace_id=linked";
  const h = host(original, { realMutations: true }); (h.shell().onExport as () => void)(); await settle();
  assert.ok(h.shell().exportPreview);
  h.navigate("tab=traces&family=assistant&trace_id=second", "location-2");
  assert.equal(h.shell().exportPreview, null);
  h.navigate(original, "location-1");
  assert.equal(h.shell().selectedTraceId, "linked");
  assert.equal(h.shell().exportPreview, null);
  h.setExport(async () => ({ trace_id: "linked", format: "openinference", payload: "fresh-return-export" }));
  (h.shell().onExport as () => void)(); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "fresh-return-export");
});

test("POP with the original history key rejects an earlier visit's pending export and success notice", async () => {
  const original = "tab=traces&family=assistant&trace_id=linked";
  const h = host(original, { realMutations: true }); const response = deferred<unknown>(); h.setExport(() => response.promise);
  (h.shell().onExport as () => void)(); await settle();
  h.navigate("tab=traces&family=assistant&trace_id=second", "location-2"); h.render();
  h.navigate(original, "location-1"); h.render();
  response.resolve({ trace_id: "linked", format: "openinference", payload: "previous-visit-export" }); await settle();
  assert.equal(h.shell().selectedTraceId, "linked");
  assert.equal(h.shell().exportPreview, null);
  assert.deepEqual(h.notices, []);
  h.setExport(async () => ({ trace_id: "linked", format: "openinference", payload: "fresh-return-export" }));
  (h.shell().onExport as () => void)(); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "fresh-return-export");
  assert.deepEqual(h.notices, ["eval.workbench.exportReady"]);
});

for (const fail of [false, true]) {
  test(`unmounted production page ignores late export ${fail ? "failure" : "success"}`, async () => {
    const h = host(undefined, { realMutations: true }); const response = deferred<unknown>();
    h.setExport(async () => {
      await response.promise;
      if (fail) throw new Error("old-page-export-error");
      return { trace_id: "linked", format: "openinference", payload: "old-page-export" };
    });
    const staleExport = h.shell().onExport as () => void;
    staleExport(); await settle(); h.unmount();
    response.resolve(null); await settle();
    assert.deepEqual(h.notices, []);
    staleExport(); await settle();
    assert.equal(h.exports.length, 1);
    assert.deepEqual(h.notices, []);
  });
}


test("detached actual MutationObserver requests cannot publish a stale callback on another trace", async () => {
  const h = host(undefined, { realMutations: true });
  const first = deferred<unknown>(); const second = deferred<unknown>();
  h.setExport(() => first.promise); (h.shell().onExport as () => void)(); await settle();
  // A second mutation detaches the first instance from observer option updates.
  h.setExport(() => second.promise); (h.shell().onExport as () => void)(); await settle();
  h.navigate("tab=traces&family=assistant&trace_id=second", "location-2"); h.render();
  h.setExport(async () => ({ trace_id: "second", format: "openinference", payload: "current" }));
  (h.shell().onExport as () => void)(); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "current");
  assert.deepEqual(h.notices, ["eval.workbench.exportReady"]);
  first.resolve({ trace_id: "linked", format: "openinference", payload: "old-detached-result" }); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "current");
  assert.deepEqual(h.notices, ["eval.workbench.exportReady"]);
  second.resolve({ trace_id: "linked", format: "openinference", payload: "other-old-result" }); await settle();
  assert.equal((h.shell().exportPreview as { payload: string }).payload, "current");
  assert.deepEqual(h.notices, ["eval.workbench.exportReady"]);
  h.unmount();
});

for (const cryptoMode of ["missing", "throwing"] as const) {
  test(`initial production page and export work with ${cryptoMode} randomUUID`, async () => {
    const h = host(undefined, { cryptoMode });
    assert.equal(h.shell().selectedTraceId, "linked");
    assert.match(String(h.shell().traceAccessKey), /^[a-f0-9-]{36}$/);
    (h.shell().onExport as () => void)(); await settle();
    assert.ok(h.shell().exportPreview);
    h.unmount();
  });
}


for (const selection of ["candidate", "baseline", "reopened"] as const) {
  test(`a pending comparison cannot restore results after changing the ${selection} run`, async () => {
    const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
    const pending = deferred<unknown>(); h.setCompare(() => pending.promise);
    const compare = h.control("Button", "children", "eval.workbench.compareRuns");
    assert.equal(compare.disabled, false);
    h.clickCompare(); await settle();
    assert.equal(h.control("Button", "children", "eval.workbench.compareRuns").loading, true);
    assert.deepEqual(h.comparisons, [["baseline", "candidate-A"]]);
    if (selection === "reopened") {
      h.navigate("tab=runs&run_id=candidate-B"); h.render();
      assert.equal((h.results().run as { run_id: string }).run_id, "candidate-B");
      assert.equal((h.results().results as { run_id: string }).run_id, "candidate-B");
      assert.equal(h.queries().find((query) => query.queryKey[1] === "run-results")?.queryKey[2], "candidate-B");
    } else {
      const selector = h.control("Select", "placeholder", selection === "candidate" ? "eval.workbench.selectCandidate" : "eval.workbench.selectBaseline");
      (selector.onChange as (id: string) => void)("candidate-B"); h.render();
    }
    assert.equal(h.comparison(), null);
    assert.equal(h.control("Button", "children", "eval.workbench.compareRuns").loading, false);
    pending.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-A", metrics: { marker: "obsolete" } });
    await settle();
    assert.equal(h.comparison(), null, "old pair must not overwrite the currently selected pair");
    h.setCompare(async (baseline_run_id, candidate_run_id) => ({ baseline_run_id, candidate_run_id }));
    h.clickCompare(); await settle();
    const result = h.comparison() as { baseline_run_id: string; candidate_run_id: string };
    assert.equal(result.baseline_run_id, selection === "baseline" ? "candidate-B" : "baseline");
    assert.equal(result.candidate_run_id, selection !== "baseline" ? "candidate-B" : "candidate-A");
    h.unmount();
  });
}

for (const outcome of ["success", "failure"] as const) {
  test(`older same-pair comparison cannot replace the newer ${outcome}`, async () => {
    const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
    const old = deferred<unknown>(); h.setCompare(() => old.promise);
    h.clickCompare(); await settle();
    for (const id of ["candidate-B", "candidate-A"]) {
      (h.control("Select", "placeholder", "eval.workbench.selectCandidate").onChange as (id: string) => void)(id); h.render();
    }
    h.setCompare(async () => {
      if (outcome === "failure") throw new Error("current comparison unavailable");
      return { baseline_run_id: "baseline", candidate_run_id: "candidate-A", marker: "fresh" };
    });
    h.clickCompare(); await settle();
    old.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-A", marker: "obsolete" }); await settle();
    if (outcome === "failure") {
      assert.equal(h.comparison(), null);
      assert.equal(h.control("Alert", "title", "eval.workbench.compareFailed").description, "current comparison unavailable");
    } else assert.equal((h.comparison() as { marker: string }).marker, "fresh");
    h.unmount();
  });
}

test("comparison completion from an earlier navigation visit stays discarded after Back", async () => {
  const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
  const pending = deferred<unknown>(); h.setCompare(() => pending.promise);
  h.clickCompare(); await settle();
  h.navigate("tab=traces", "other-visit"); h.render();
  h.navigate("tab=runs", "location-1"); h.render();
  pending.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-A" }); await settle();
  assert.equal(h.comparison(), null);
  h.unmount();
});

test("late failure for an abandoned pair cannot replace the current comparison or show its error", async () => {
  const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
  const pending = deferred<void>();
  h.setCompare(async () => { await pending.promise; throw new Error("abandoned failure"); });
  h.clickCompare(); await settle();
  (h.control("Select", "placeholder", "eval.workbench.selectCandidate").onChange as (id: string) => void)("candidate-B"); h.render();
  h.setCompare(async (baseline_run_id, candidate_run_id) => ({ baseline_run_id, candidate_run_id }));
  h.clickCompare(); await settle();
  pending.resolve(); await settle();
  assert.equal((h.comparison() as { candidate_run_id: string }).candidate_run_id, "candidate-B");
  assert.deepEqual(h.notices, []);
  assert.throws(() => h.control("Alert", "title", "eval.workbench.compareFailed"));
  h.unmount();
});

test("Compare loading belongs to the current request, not an abandoned pending GET", async () => {
  const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
  const abandoned = deferred<unknown>(); h.setCompare(() => abandoned.promise);
  h.clickCompare(); await settle();
  assert.equal(h.control("Button", "children", "eval.workbench.compareRuns").loading, true);
  (h.control("Select", "placeholder", "eval.workbench.selectCandidate").onChange as (id: string) => void)("candidate-B"); h.render();
  const current = deferred<unknown>(); h.setCompare(() => current.promise);
  h.clickCompare(); await settle();
  abandoned.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-A" }); await settle();
  assert.equal(h.control("Button", "children", "eval.workbench.compareRuns").loading, true, "old completion must not end current loading");
  assert.equal(h.comparison(), null);
  current.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-B" }); await settle();
  assert.equal(h.control("Button", "children", "eval.workbench.compareRuns").loading, false);
  assert.equal((h.comparison() as { candidate_run_id: string }).candidate_run_id, "candidate-B");
  h.unmount();
});


for (const restore of [false, true]) {
  test(`comparison read is discarded after credential change, restore=${restore}`, async () => {
    const h = host("tab=runs", { evaluationFixture: true, realMutations: true });
    const old = deferred<unknown>(); h.setCompare(() => old.promise); h.clickCompare(); await settle();
    h.changeAuth("fixture-session-B"); if (restore) h.changeAuth("fixture-session-A");
    old.resolve({ baseline_run_id: "baseline", candidate_run_id: "candidate-A", marker: "private" }); await settle();
    assert.equal(h.comparison(), null); assert.deepEqual(h.notices, []); h.unmount();
  });
}
test("old compared Trace callback cannot navigate after replacing the same pair", async () => {
  const h = host("tab=runs", { evaluationFixture: true, realMutations: true }); h.clickCompare(); await settle();
  const callback = find(h.render(), "ExperimentRunComparison").props.onOpenTrace as (id: string) => void;
  h.clickCompare(); await settle(); const before = h.params().toString(); callback("old-trace"); assert.equal(h.params().toString(), before); h.unmount();
});
