import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  return [...((value as Node).type === type ? [value as Node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  return value && typeof value === "object" ? Object.values(value).map(text).join(" ") : "";
}
const snapshot = (status = "available", bindings: unknown[] = []) => ({
  file_uploads: { count: 2 }, url_imports: { count: 1 }, confluence_bindings: bindings,
  connector_status: status, total_documents: 3,
});
// Production SourcesTab, query hook and API; presentation hosts and HTTP are controlled.
// This intentionally neither creates a browser/listener nor exercises provider credentials.
function host() {
  const calls: string[] = [];
  let response: unknown = snapshot(), failed = false, fetching = false, sources: unknown = null;
  let query: { queryFn: () => Promise<unknown> };
  let release: (() => void) | undefined;
  let wait: Promise<void> | undefined;
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: named, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en-US" } }) },
    "lucide-react": named, "@/components/ui/button": { Button: "Button" }, "@/components/ui/card": { Card: "Card" },
    "@tanstack/react-query": { useQuery: (options: typeof query) => { query = options; return options; } },
    "@/lib/api": { api: { async get(url: string) { calls.push(url); await wait;
      if (response instanceof Error) throw response; return { data: response }; } } },
  };
  function load(file: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    vm.runInNewContext(code, { exports, require: (id: string) => modules[id] ?? named });
    return exports;
  }
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./sourcePresentation"] = load("./sourcePresentation.ts");
  const { useDatasetSources: sourceQueryOptions } = load("../../../hooks/useKnowledge.ts") as { useDatasetSources: (id: string) => void };
  sourceQueryOptions("dataset-a");
  const { SourcesTab } = load("./SourcesTab.tsx") as { SourcesTab: (props: Data) => unknown };
  async function refresh() { fetching = true; try { sources = await query.queryFn(); failed = false; } catch { failed = true; } finally { fetching = false; } }
  function tree() { return SourcesTab({ sources, sourcesLoading: fetching, sourcesFailed: failed, onRetrySources: refresh,
    onUploadClick: () => { throw Error("Upload must not be called by status recovery"); },
    onUrlClick: () => { throw Error("Import must not be called by status recovery"); } }); }
  function refreshButton() { const buttons = nodes(tree(), "Button"); const found = buttons.find(n =>
    text(n).includes("knowledge.sources.refreshStatus") || text(n).includes("common.retry"));
    assert.ok(found, "Source status must be recheckable without reloading the page"); return found; }
  return { calls, tree, refresh, refreshButton, queryFn: () => query.queryFn(), setResponse(value: unknown) { response = value; },
    defer() { wait = new Promise<void>(resolve => { release = resolve; }); }, release() { release?.(); wait = undefined; } };
}

for (const status of ["unavailable", "not_configured", "unknown", "available"]) {
  test(`${status}: operator repair can be observed by a GET-only local refresh`, async () => {
    const h = host(); h.setResponse(snapshot(status)); await h.refresh();
    assert.ok(text(h.tree()).includes(`knowledge.sources.connectorState.${status}`));
    h.setResponse(snapshot("available", [{ binding_id: "a", space_name: "Recovered source", status: "completed", last_success_at: "2026-10-12T00:00:00Z", page_count: 3, has_problem: false }]));
    await (h.refreshButton().props.onClick as () => Promise<void>)();
    assert.ok(text(h.tree()).includes("Recovered source"));
    assert.deepEqual(h.calls, ["/api/v1/knowledge/dataset-a/sources", "/api/v1/knowledge/dataset-a/sources"]);
  });
}
test("binding failure is not successful zero-update sync and can be rechecked", async () => {
  const h = host(); h.setResponse(snapshot("available", [{ binding_id: "a", status: "failed", has_problem: true, page_count: 0, last_success_at: "2026-10-11T00:00:00Z" }])); await h.refresh();
  assert.ok(text(h.tree()).includes("knowledge.sources.bindingState.problem"));
  assert.ok(!text(h.tree()).includes("knowledge.sources.lastKnownPages"));
  await (h.refreshButton().props.onClick as () => Promise<void>)(); assert.equal(h.calls.length, 2);
});
test("failed refresh hides the retained snapshot and Retry restores the source read", async () => {
  const h = host(); h.setResponse(snapshot("available", [{ binding_id: "a", space_name: "Old source", status: "completed", page_count: 3, last_success_at: "2026-10-11T00:00:00Z" }])); await h.refresh();
  h.setResponse(Error("offline")); await (h.refreshButton().props.onClick as () => Promise<void>)();
  assert.ok(text(h.tree()).includes("knowledge.sources.connectorState.load_failed"));
  assert.ok(!text(h.tree()).includes("Old source"));
  assert.ok(text(h.tree()).includes("knowledge.sources.countUnknown"));
  h.setResponse(snapshot()); await (h.refreshButton().props.onClick as () => Promise<void>)();
  assert.ok(text(h.tree()).includes("knowledge.sources.connectorState.available")); assert.equal(h.calls.length, 3);
});
test("refresh is disabled while an existing snapshot is being rechecked", async () => {
  const h = host(); await h.refresh(); h.defer(); const pending = (h.refreshButton().props.onClick as () => Promise<void>)();
  assert.equal(h.refreshButton().props.disabled, true); h.release(); await pending;
  assert.equal(h.refreshButton().props.disabled, false);
});
test("initial HTTP failure retains the existing Retry behavior", async () => {
  const h = host(); h.setResponse(Error("offline")); await h.refresh();
  assert.ok(text(h.tree()).includes("knowledge.sources.connectorState.load_failed"));
  h.setResponse(snapshot()); await (h.refreshButton().props.onClick as () => Promise<void>)();
  assert.ok(text(h.tree()).includes("knowledge.sources.connectorState.available"));
});
function sourcePage(sourceQuery: Data) {
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    "@/components/deferComponent": { deferComponent: (_load: unknown, name: string) => name },
    react: { useState: (initial: unknown) => [typeof initial === "function" ? initial() : initial, () => {}], useMemo: (f: () => unknown) => f(), useRef: (current: unknown) => ({ current }), useEffect: () => {} },
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "react-router-dom": { useParams: () => ({ datasetId: "dataset-a" }), useNavigate: () => () => {}, useSearchParams: () => [new URLSearchParams("tab=sources"), () => {}] },
    "@tanstack/react-query": { useQueryClient: () => ({}) },
    "@/store/useAuthStore": { useAuthStore: (select: (state: unknown) => unknown) => select({ user: null, token: null }), usePermission: () => false },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: () => "anonymous" },
    "@/hooks/useKnowledge": { useDataset: () => ({ data: { dataset_id: "dataset-a" } }), useDocuments: () => ({ data: { items: [], total: 0 } }), useDatasetSources: () => sourceQuery },
    "@/pages/knowledge/detail/useDatasetUploadDialog": { useDatasetUploadDialog: () => ({}) },
    "@/pages/knowledge/detail/useHitTestConsole": { useHitTestConsole: () => ({}) },
  };
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../DatasetDetail.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: (id: string) => modules[id] ?? named, URLSearchParams });
  const tree = (exports.KnowledgeDatasetDetailPage as () => unknown)();
  const source = nodes(tree, "SourcesTab")[0]; assert.ok(source);
  return source;
}
test("production page shell passes background-fetch state and the same read-only refetch callback", () => {
  const calls: unknown[] = [];
  const query = { data: snapshot(), isPending: false, isFetching: true, isError: false, refetch: (options: unknown) => { calls.push(options); } };
  const source = sourcePage(query);
  assert.equal(source.props.sourcesLoading, true); assert.equal(source.props.sources, query.data);
  (source.props.onRetrySources as () => void)(); assert.equal(calls.length, 1);
  assert.equal((calls[0] as { cancelRefetch: boolean }).cancelRefetch, false);
});
test("real query observer coalesces repeated production refresh callbacks before rerender", async () => {
  const h = host(); h.defer();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const key = ["kb-dataset-sources", "dataset-a"];
  client.setQueryData(key, snapshot("unavailable"));
  const observer = new QueryObserver(client, { queryKey: key, queryFn: h.queryFn, staleTime: Infinity });
  const unsubscribe = observer.subscribe(() => {});
  try {
    const source = sourcePage({ ...observer.getCurrentResult() });
    const refresh = source.props.onRetrySources as () => void;
    refresh(); refresh(); refresh();
    assert.equal(h.calls.length, 1, "Repeated clicks must share the pending GET rather than cancel and restart it");
    const pending = observer.refetch({ cancelRefetch: false }); h.release(); await pending;
    assert.equal(h.calls.length, 1);
    assert.equal((observer.getCurrentResult().data as ReturnType<typeof snapshot>).connector_status, "available");
  } finally { h.release(); unsubscribe(); client.clear(); }
});
