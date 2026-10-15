import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { createInstance } from "i18next";
import ts from "typescript";
import type { QueryHistoryItem, QueryFeedback } from "../../../types/knowledge.ts";
import { queryDiagnosticHandoff, queryDiagnosticTraceUrl } from "./queryDiagnosticHandoff.ts";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
const query = (patch: Partial<QueryHistoryItem> = {}): QueryHistoryItem => ({
  id: "query-a", dataset_id: "dataset-a", content: "Why was the Chinese paragraph missing?", source: "hit_test",
  created_at: "2026-10-13T08:00:00Z", metadata: { private_text: "unused metadata" }, trace_id: "original-trace-a",
  query_fingerprint: "original-fingerprint", mode: "bm25", top_k: 3, hit_count: 0, stage_timings: {}, ...patch,
});
const negative = (patch: Partial<QueryFeedback> = {}): QueryFeedback => ({
  feedback_id: "feedback-a", dataset_id: "dataset-a", tenant_id: "tenant-a", trace_id: "feedback-trace-a",
  query_fingerprint: "fingerprint", target_type: "qa_answer", target_id: "answer-a", rating: "negative",
  reason_code: "missing_context", created_by: "user-a", query_content: "Missing answer?",
  created_at: "2026-10-13T08:00:00Z", updated_at: "2026-10-13T08:00:00Z", ...patch,
});
const page = (rows = [query()], cursor: string | null = null) => ({ queries: rows, next_cursor: cursor, has_more: !!cursor });
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function nodes(value: unknown, type?: string): Node[] {
  if (!value || typeof value !== "object") return [];
  const node = value as Node;
  return [...(node.props && (!type || node.type === type) ? [node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (!value || typeof value !== "object") return "";
  return ("props" in value ? [(value as Node).props.children] : Object.values(value)).map(text).join(" ");
}
function environment(locale = "en-US") {
  const resources = JSON.parse(readFileSync(new URL(`../../../i18n/locales/knowledge-${locale}.json`, import.meta.url), "utf8"));
  const i18n = createInstance();
  void i18n.init({ lng: locale, fallbackLng: false, initAsync: false, resources: { [locale]: { translation: { knowledge: resources } } } });
  const auth = { token: "synthetic-session-a", user: { user_id: "user-a", permissions: ["console:eval:view", "console:eval:run"], roles: ["member"], tier: "normal" },
    hasPermission: (permission: string) => auth.user.permissions.includes(permission) };
  const subscribers = new Set<() => void>();
  const calls: { method: string; url: string; body?: Data; options?: Data }[] = [];
  const confirmations: string[] = [], navigation: string[] = [], toasts: unknown[] = [];
  const state = { datasetId: "dataset-a", permission: "owner", datasetError: false, confirm: true, params: new URLSearchParams("tab=queries") };
  let read = async (url: string, _options?: Data): Promise<unknown> => {
    void _options;
    if (url.endsWith("/queries")) return page();
    if (url.endsWith("/feedback")) return { feedback: [negative()], has_more: false };
    if (url.includes("presets")) return { presets: [] };
    if (url === "/api/v1/eval/datasets") return { datasets: [{ dataset_id: "eval-a", metadata: { kb_dataset_id: "dataset-a" } }], total: 1 };
    if (url.includes("/kb-failures/")) return null;
    if (url.includes("trace")) return { trace: { trace_id: "fresh-trace" } };
    throw new Error(`Unconfigured controlled GET ${url}`);
  };
  let write = async (url: string, _body: Data): Promise<unknown> => {
    void _body;
    if (url.endsWith("/hit_test")) return { results: [], trace_id: "fresh-trace", query_fingerprint: "fresh-fingerprint", metadata: {} };
    if (url.endsWith("kb-failures:save")) return { created: true, revision: 1 };
    if (url.includes("ragas")) return { results: [], judge_model: "judge-a" };
    throw new Error(`Unconfigured controlled POST ${url}`);
  };
  const api = {
    async get(url: string, options?: Data) { calls.push({ method: "GET", url, options }); return { data: await read(url, options) }; },
    async post(url: string, body: Data, options?: Data) { calls.push({ method: "POST", url, body, options }); return { data: await write(url, body) }; },
  };
  return { auth, subscribers, state, calls, confirmations, navigation, toasts, api, i18n,
    setRead(fn: typeof read) { read = fn; }, defaultRead: read, setWrite(fn: typeof write) { write = fn; }, defaultWrite: write,
    changeSession(token: string) { auth.token = token; subscribers.forEach(fn => fn()); },
  };
}
// Reconstructed ordinary regression host: production code; controlled scheduling/HTTP.
// These source tests are not live acceptance and must be rerun after restoration.
function host(file: string, exported: string, env: ReturnType<typeof environment>, initial: Data = {}) {
  const slots: unknown[] = [], cleanups = new Map<number, () => void>();
  let cursor = 0, dirty = true, tree: unknown, props = initial, effects: (() => void)[] = [];
  const same = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const react = {
    useState(value: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = typeof value === "function" ? value() : value;
      return [slots[i], (next: unknown) => { const v = typeof next === "function" ? next(slots[i]) : next; if (!Object.is(v, slots[i])) { slots[i] = v; dirty = true; } }]; },
    useRef(value: unknown) { const i = cursor++; return slots[i] ?? (slots[i] = { current: value }); },
    useMemo(fn: () => unknown, deps: unknown[]) { const i = cursor++, old = slots[i] as { deps: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) slots[i] = { deps, value: fn() }; return (slots[i] as { value: unknown }).value; },
    useEffect(fn: () => void | (() => void), deps: unknown[]) { const i = cursor++; if (!same(slots[i] as unknown[], deps)) {
      slots[i] = deps; effects.push(() => { cleanups.get(i)?.(); const cleanup = fn(); if (cleanup) cleanups.set(i, cleanup); else cleanups.delete(i); }); } },
  };
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, p: Data) => ({ type, props: p });
  const useAuthStore = Object.assign((select: (state: typeof env.auth) => unknown) => select(env.auth), {
    getState: () => env.auth, subscribe: (fn: () => void) => { env.subscribers.add(fn); return () => env.subscribers.delete(fn); },
  });
  const toast = Object.assign((value: unknown) => env.toasts.push(value), { error: (...args: unknown[]) => env.toasts.push(args) });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" }, "lucide-react": named,
    "react-i18next": { useTranslation: () => ({ t: env.i18n.t.bind(env.i18n), i18n: env.i18n }) },
    "react-router-dom": { useParams: () => ({ datasetId: env.state.datasetId }), useNavigate: () => (url: string) => env.navigation.push(url),
      useSearchParams: () => [env.state.params, (params: URLSearchParams) => { env.state.params = params; env.navigation.push(params.toString()); }] },
    "@/store/useAuthStore": { useAuthStore, usePermission: (permission: string) => env.auth.hasPermission(permission) },
    "@tanstack/react-query": { useQueryClient: () => ({ invalidateQueries() {} }) },
    "@/hooks/useKnowledge": { useDataset: () => ({ isError: env.state.datasetError, data: { dataset_id: env.state.datasetId, my_permission: env.state.permission } }),
      useDocuments: () => ({ data: { items: [], total: 0 } }), useDatasetSources: () => ({}) },
    "@/pages/knowledge/detail/useDatasetUploadDialog": { useDatasetUploadDialog: () => ({}) },
    "@/lib/api": { api: env.api }, "@/hooks/use-toast": { toast }, "axios": { isAxiosError: () => false },
  };
  function load(path: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, require: (id: string) => modules[id] ?? named, URLSearchParams, AbortController, console, setTimeout,
      window: { confirm: (message: string) => { env.confirmations.push(message); return env.state.confirm; } } });
    return exports;
  }
  for (const [id, path] of [
    ["@/types/knowledge", "../../../types/knowledge.ts"], ["@/lib/sdkAuthorityScope", "../../../lib/sdkAuthorityScope.ts"],
    ["./queryDiagnosticHandoff", "./queryDiagnosticHandoff.ts"], ["@/api/knowledge", "../../../api/knowledge.ts"],
    ["@/api/eval", "../../../api/eval.ts"], ["./evalCaseStore", "./evalCaseStore.ts"],
    ["./knowledgeSaveAuthority", "./knowledgeSaveAuthority.ts"], ["./kbEvalDataset", "./kbEvalDataset.ts"],
    ["@/pages/knowledge/detail/useHitTestConsole", "./useHitTestConsole.ts"],
  ]) modules[id!] = load(path!);
  modules["./detail/queryDiagnosticHandoff"] = modules["./queryDiagnosticHandoff"];
  modules["@/pages/knowledge/detail/evalCaseStore"] = modules["./evalCaseStore"];
  const component = load(file)[exported] as (props: Data) => unknown;
  function render(next = props, runEffects = true) { props = next; cursor = 0; dirty = false; tree = component(props);
    if (runEffects) { const queue = effects; effects = []; queue.forEach(fn => fn()); } return tree; }
  async function flush() { for (let i = 0; i < 10; i++) { await new Promise<void>(resolve => setImmediate(resolve));
    if (effects.length) { const queue = effects; effects = []; queue.forEach(fn => fn()); } if (dirty) render(); } }
  function find(id: string) { const found = nodes(tree).find(n => n.props["data-testid"] === id); assert.ok(found, id); return found; }
  function button(label: string) { const found = nodes(tree, "Button").find(n => text(n).trim() === label || n.props["aria-label"] === label); assert.ok(found, label); return found; }
  return { render, flush, find, button, tree: () => tree, props: () => props, dispose: () => cleanups.forEach(fn => fn()),
    child: (name: string) => { const found = nodes(tree, name)[0]; assert.ok(found, name); return found.props; } };
}
async function click(node: Node) { assert.equal(Boolean(node.props.disabled), false); await (node.props.onClick as () => unknown)(); }
const observationProps = { datasetId: "dataset-a", permission: "owner", onTestQuestion(_v: unknown) { void _v; }, onInspect(_v: unknown) { void _v; } };

for (const locale of ["en-US", "zh-CN"]) test(`${locale}: actual translations, observation actions and unknown counts`, async () => {
  const e = environment(locale), handed: unknown[] = [];
  e.setRead(async (url, options) => url.endsWith("/queries") ? page([query({ hit_count: null })]) : e.defaultRead(url, options));
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, { ...observationProps, onTestQuestion: (v: unknown) => handed.push(v) });
  h.render(); await h.flush(); assert.equal(text(h.find("query-test-query-a")).trim(), e.i18n.t("knowledge.observability.testQuestion"));
  assert.ok(text(h.tree()).includes(e.i18n.t("knowledge.observability.hitsUnknown"))); assert.ok(!text(h.tree()).includes("knowledge.observability."));
  await click(h.find("query-test-query-a")); await click(h.find("query-test-feedback-a")); assert.equal(handed.length, 2);
  assert.equal(e.calls.filter(c => c.method === "POST").length, 0);
  assert.equal(new URL(h.find("query-trace-query-a").props.href as string, "https://fixture.invalid").searchParams.get("trace_id"), "original-trace-a"); h.dispose();
});

test("missing question, trace and viewing permission are distinct", async () => {
  const e = environment(); e.auth.user.permissions = [];
  e.setRead(async url => url.endsWith("/queries") ? page([query({ trace_id: null })]) : { feedback: [negative({ query_content: null })], has_more: false });
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, observationProps); h.render(); await h.flush();
  assert.equal(h.find("query-test-feedback-a").props.disabled, true); assert.equal(nodes(h.tree(), "a").length, 0);
  for (const key of ["questionMissing", "traceMissing", "tracePermission"]) assert.ok(text(h.tree()).includes(e.i18n.t(`knowledge.observability.${key}`))); h.dispose();
});

for (const pane of ["queries", "feedback"]) for (const status of [401, 403, 404]) test(`${pane} access-unavailable response retires both panes (${status})`, async () => {
  const e = environment(), access: boolean[] = [], handed: unknown[] = [];
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, { ...observationProps, onAccessChange: (v: boolean) => access.push(v), onTestQuestion: (v: unknown) => handed.push(v) });
  h.render(); await h.flush(); const old = h.find("query-test-feedback-a");
  e.setRead(async (url, options) => { if (url.endsWith(`/${pane}`)) throw Object.assign(Error("not available"), { response: { status } }); return e.defaultRead(url, options); });
  await click(h.button(e.i18n.t(`knowledge.observability.${pane === "queries" ? "refreshQueries" : "refreshFeedback"}`))); await h.flush();
  assert.equal(nodes(h.tree(), "article").length, 0); assert.equal(access.at(-1), false); await click(old); assert.equal(handed.length, 0);
  e.setRead(e.defaultRead); await click(h.button(e.i18n.t("knowledge.observability.recheckAccess"))); await h.flush(); assert.equal(access.at(-1), true);
  await click(h.find("query-test-query-a")); assert.equal(handed.length, 1); h.dispose();
});

test("viewer has no history reads and navigation retires delayed observations", async () => {
  const e = environment(), pending = deferred<unknown>(), handed: unknown[] = [];
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, { ...observationProps, permission: "viewer" }); h.render(); await h.flush(); assert.equal(e.calls.length, 0);
  e.setRead(async url => url.endsWith("/queries") ? pending.promise : { feedback: [], has_more: false });
  h.render({ ...observationProps, onTestQuestion: (v: unknown) => handed.push(v) }); await h.flush();
  e.setRead(async () => ({ queries: [], feedback: [], has_more: false })); h.render({ ...observationProps, datasetId: "dataset-b" }); await h.flush();
  pending.resolve(page()); await h.flush(); assert.equal(nodes(h.tree(), "article").length, 0); assert.equal(handed.length, 0); h.dispose();
});

test("cancelled replacement preserves draft; accepted handoff waits for explicit Run and explicit save", async () => {
  const e = environment(), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e); shell.render(); await shell.flush();
  let hit = shell.child("RetrievalTab").hitTest as Data; (hit.setQuery as (v: string) => void)("edited draft"); await shell.flush();
  e.state.confirm = false; (shell.child("QueryObservabilityTab").onTestQuestion as (v: unknown) => void)(queryDiagnosticHandoff("dataset-a", query())); await shell.flush();
  assert.equal((shell.child("RetrievalTab").hitTest as Data).query, "edited draft"); assert.equal(e.navigation.length, 0);
  e.state.confirm = true; (shell.child("QueryObservabilityTab").onTestQuestion as (v: unknown) => void)(queryDiagnosticHandoff("dataset-a", query())); await shell.flush();
  hit = shell.child("RetrievalTab").hitTest as Data; assert.equal(hit.query, query().content); assert.equal(hit.mode, "hybrid");
  assert.equal(e.calls.filter(c => c.method === "POST").length, 0); assert.equal(e.state.params.toString(), "tab=retrieval");
  await (hit.runHitTest as () => Promise<void>)(); await shell.flush(); hit = shell.child("RetrievalTab").hitTest as Data;
  const retrieval = host("./RetrievalTab.tsx", "RetrievalTab", e, { datasetId: "dataset-a", hitTest: hit }); retrieval.render();
  const save = host("./SaveKnowledgeFailureDialog.tsx", "SaveKnowledgeFailureDialog", e, retrieval.child("SaveKnowledgeFailureDialog")); save.render(); await save.flush();
  await click(save.find("send-hits-to-eval")); await save.flush();
  (nodes(save.tree(), "Textarea")[0]!.props.onChange as (event: unknown) => void)({ target: { value: "Expected answer for review" } }); await save.flush();
  assert.equal(e.calls.filter(c => c.url.endsWith("kb-failures:save")).length, 0);
  await click(save.find("save-knowledge-failure")); await save.flush();
  const sent = e.calls.find(c => c.url.endsWith("kb-failures:save")); assert.ok(sent); assert.equal(sent.body?.source_trace_id, "fresh-trace");
  assert.equal(sent.body?.expected_answer, "Expected answer for review"); assert.equal(e.subscribers.size, 1); shell.dispose(); retrieval.dispose(); save.dispose(); assert.equal(e.subscribers.size, 0);
});

for (const operation of ["runHitTest", "runRagasScore"]) for (const outcome of ["success", "failure"]) test(`${operation}: retired dataset view ignores late ${outcome}`, async () => {
  const e = environment(), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e); shell.render(); await shell.flush();
  let hit = shell.child("RetrievalTab").hitTest as Data; (hit.setQuery as (v: string) => void)("current question"); await shell.flush();
  if (operation === "runRagasScore") {
    e.setWrite(async () => ({ results: [{ document_id: "doc-a", segment_id: "segment-a", text: "context", score: 0.3 }], trace_id: "trace-a" }));
    await ((shell.child("RetrievalTab").hitTest as Data).runHitTest as () => Promise<void>)(); await shell.flush();
  }
  const pending = deferred<unknown>(); e.setWrite(() => pending.promise); hit = shell.child("RetrievalTab").hitTest as Data;
  const running = (hit[operation] as () => Promise<void>)(); await shell.flush(); e.state.datasetId = "dataset-b";
  shell.render({}, false); const masked = shell.child("RetrievalTab").hitTest as Data;
  assert.equal(masked.query, ""); assert.equal((masked.hitResults as unknown[]).length, 0); await shell.flush();
  if (outcome === "success") pending.resolve({ results: [{ text: "retired result" }], trace_id: "retired-trace" }); else pending.reject(Error("retired error"));
  await running; await shell.flush(); const now = shell.child("RetrievalTab").hitTest as Data;
  assert.ok(!JSON.stringify(now).includes("retired")); shell.dispose();
});

test("access recheck success followed by denial still clears the current handoff", async () => {
  const e = environment(), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e); shell.render(); await shell.flush();
  let status = shell.child("QueryObservabilityTab").onAccessChange as (value: boolean) => void;
  status(false); await shell.flush(); status = shell.child("QueryObservabilityTab").onAccessChange as (value: boolean) => void;
  status(true); await shell.flush();
  (shell.child("QueryObservabilityTab").onTestQuestion as (value: unknown) => void)(queryDiagnosticHandoff("dataset-a", query())); await shell.flush();
  status(false); await shell.flush(); assert.equal((shell.child("RetrievalTab").hitTest as Data).query, "");
  assert.equal(nodes(shell.tree()).some(n => n.props["data-testid"] === "query-diagnostic-handoff"), false); shell.dispose();
});

test("pagination coalesces the pending read and exposes the next accepted page", async () => {
  const e = environment(), pending = deferred<unknown>();
  e.setRead(async url => url.endsWith("/queries") ? page([query()], "cursor-a") : { feedback: [], has_more: false });
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, observationProps); h.render(); await h.flush();
  const more = h.button(e.i18n.t("knowledge.observability.loadMore")); e.setRead(() => pending.promise);
  await click(more); await click(more); assert.equal(e.calls.filter(c => c.url.endsWith("/queries")).length, 2);
  pending.resolve(page([query({ id: "query-b" })])); await h.flush(); h.find("query-test-query-b"); assert.equal(nodes(h.tree(), "article").length, 2); h.dispose();
});

test("an unmounted save view does not publish a late success notice or retry its accepted write", async () => {
  const e = environment(), pending = deferred<unknown>();
  const save = host("./SaveKnowledgeFailureDialog.tsx", "SaveKnowledgeFailureDialog", e, {
    kbDatasetId: "dataset-a", query: "current question", observedHits: [], source: "kb-hit-test", sourceTraceId: "fresh-trace", testId: "open-save",
  });
  save.render(); await save.flush(); await click(save.find("open-save")); await save.flush();
  (nodes(save.tree(), "Textarea")[0]!.props.onChange as (event: unknown) => void)({ target: { value: "Expected answer" } }); await save.flush();
  e.setWrite((url, body) => url.endsWith("kb-failures:save") ? pending.promise : e.defaultWrite(url, body));
  const saving = click(save.find("save-knowledge-failure")); await save.flush();
  assert.equal(e.calls.filter(c => c.url.endsWith("kb-failures:save")).length, 1); save.dispose();
  pending.resolve({ created: true, revision: 1 }); await saving;
  assert.equal(e.toasts.length, 0); assert.equal(e.calls.filter(c => c.url.endsWith("kb-failures:save")).length, 1); assert.equal(e.subscribers.size, 0);
});

test("navigation contains only exact supported identity fields", () => {
  assert.equal(queryDiagnosticHandoff("dataset-b", query()), null);
  const observation = queryDiagnosticHandoff("dataset-a", query())!;
  const url = queryDiagnosticTraceUrl(observation)!; assert.ok(!url.includes(query().content) && !url.includes("metadata"));
  assert.deepEqual([...new URL(url, "https://fixture.invalid").searchParams.keys()].sort(), ["dataset_id", "family", "tab", "trace_id"]);
});

test("batched current-view session changes retire the original observation read", async () => {
  const e = environment(), pending = deferred<unknown>();
  e.setRead(async url => url.endsWith("/queries") ? pending.promise : { feedback: [], has_more: false });
  const h = host("./QueryObservabilityTab.tsx", "QueryObservabilityTab", e, observationProps); h.render(); await h.flush();
  e.changeSession("synthetic-session-b"); e.changeSession("synthetic-session-a");
  e.setRead(async () => ({ queries: [], feedback: [], has_more: false })); await h.flush();
  pending.resolve(page([query({ content: "retired observation" })])); await h.flush();
  assert.ok(!text(h.tree()).includes("retired observation")); assert.equal(nodes(h.tree(), "article").length, 0); h.dispose();
});

test("batched session changes retire the prior workbench callback", async () => {
  const e = environment(), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e); shell.render(); await shell.flush();
  let hit = shell.child("RetrievalTab").hitTest as Data; (hit.setQuery as (value: string) => void)("current question"); await shell.flush();
  hit = shell.child("RetrievalTab").hitTest as Data;
  e.changeSession("synthetic-session-b"); e.changeSession("synthetic-session-a");
  await (hit.runHitTest as () => Promise<void>)(); assert.equal(e.calls.filter(c => c.method === "POST").length, 0);
  await shell.flush(); assert.equal((shell.child("RetrievalTab").hitTest as Data).query, ""); shell.dispose();
});

for (const stage of ["datasets", "trace", "latest"]) test(`cancelling a save view during pending ${stage} read stops the remaining save`, async () => {
  const e = environment(), pending = deferred<unknown>();
  const matches = (url: string) => stage === "datasets" ? url === "/api/v1/eval/datasets"
    : stage === "trace" ? url.includes("/traces/") : url.includes("/kb-failures/");
  e.setRead((url, options) => matches(url) ? pending.promise : e.defaultRead(url, options));
  const save = host("./SaveKnowledgeFailureDialog.tsx", "SaveKnowledgeFailureDialog", e, {
    kbDatasetId: "dataset-a", query: "current question", observedHits: [], source: "kb-hit-test", sourceTraceId: "fresh-trace", testId: "open-save",
  });
  save.render(); await save.flush(); await click(save.find("open-save")); await save.flush();
  (nodes(save.tree(), "Textarea")[0]!.props.onChange as (event: unknown) => void)({ target: { value: "Expected answer" } }); await save.flush();
  const saving = click(save.find("save-knowledge-failure")); await save.flush();
  const pendingRead = e.calls.find(call => call.method === "GET" && matches(call.url)); assert.ok(pendingRead);
  const callsAtCancellation = e.calls.length; save.dispose();
  pending.resolve(await e.defaultRead(pendingRead.url, pendingRead.options)); await saving;
  assert.equal(e.calls.length, callsAtCancellation); assert.equal(e.calls.filter(call => call.method === "POST").length, 0);
  assert.equal(e.toasts.length, 0); assert.equal(e.subscribers.size, 0);
});

for (const locale of ["en-US", "zh-CN"]) test(`${locale}: retrieval separates failure from empty success and unknown counts`, async () => {
  const e = environment(locale), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e);
  shell.render(); await shell.flush();
  const retrieval = host("./RetrievalTab.tsx", "RetrievalTab", e, shell.child("RetrievalTab"));
  retrieval.render();
  assert.ok(!text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.vectorHitsCount", { vector: 0, keyword: 0 })));
  (nodes(retrieval.tree(), "Textarea")[0]!.props.onChange as (event: unknown) => void)({ target: { value: "current question" } });
  await shell.flush(); retrieval.render(shell.child("RetrievalTab"));
  e.setWrite(async () => { throw Error("synthetic retrieval unavailable"); });
  await click(retrieval.button(e.i18n.t("knowledge.detail.test"))); await shell.flush();
  retrieval.render(shell.child("RetrievalTab"));
  assert.ok(text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.retrievalFailed")), "failed query must have a failure outcome");
  assert.ok(!text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.noResults")), "failure must not claim a completed empty result");
  assert.ok(!text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.retrievalStats")), "failure must not invent effective retrieval statistics");
  assert.ok(!text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.vectorHitsCount", { vector: 0, keyword: 0 })));
  assert.equal((nodes(retrieval.tree(), "Textarea")[0]!.props.value), "current question");
  assert.equal(e.calls.filter(c => c.method === "POST").length, 1, "failure never retries itself");
  assert.equal(nodes(retrieval.tree(), "SaveKnowledgeFailureDialog").length, 0);

  for (const [metadata, vector, keyword] of [
    [{}, "not-reported", "not-reported"],
    [{ vector_hits_count: 0, keyword_hits_count: 4 }, 0, 4],
    [{ vector_hits_count: null, keyword_hits_count: -1 }, "not-reported", "not-reported"],
    [{ vector_hits_count: 1.5, keyword_hits_count: "2" }, "not-reported", "not-reported"],
  ] as const) {
    e.setWrite(async () => ({ results: [], metadata, trace_id: "accepted-trace", query_fingerprint: "accepted-fingerprint" }));
    await click(retrieval.button(e.i18n.t("knowledge.detail.test"))); await shell.flush();
    retrieval.render(shell.child("RetrievalTab"));
    const rendered = text(retrieval.tree());
    assert.ok(rendered.includes(e.i18n.t("knowledge.detail.noResults")));
    assert.ok(rendered.includes(e.i18n.t("knowledge.detail.retrievalEmptyHint")));
    assert.ok(!rendered.includes(e.i18n.t("knowledge.detail.retrievalFailed")));
    assert.ok(!rendered.includes("synthetic retrieval unavailable"));
    assert.ok(rendered.includes(e.i18n.t("knowledge.detail.vectorHitsCount", {
      vector: vector === "not-reported" ? e.i18n.t("knowledge.detail.hitCountUnknown") : vector,
      keyword: keyword === "not-reported" ? e.i18n.t("knowledge.detail.hitCountUnknown") : keyword,
    })));
    assert.ok(!nodes(retrieval.tree(), "div").some(node => node.props.className === "text-lg font-bold text-primary/90"
      && (node.props.children === 1.5 || node.props.children === -1)), "malformed vector counts must not appear in adjacent statistics");
    assert.ok(!nodes(retrieval.tree(), "div").some(node => node.props.className === "text-lg font-bold text-amber-700 dark:text-amber-400"
      && node.props.children === -1), "malformed keyword counts must not appear in adjacent statistics");
    assert.equal(retrieval.child("SaveKnowledgeFailureDialog").sourceTraceId, "accepted-trace");
  }
  (nodes(retrieval.tree(), "Textarea")[0]!.props.onChange as (event: unknown) => void)({ target: { value: "edited question" } });
  await shell.flush(); retrieval.render(shell.child("RetrievalTab"));
  assert.ok(text(retrieval.tree()).includes(e.i18n.t("knowledge.detail.retrievalReady")));
  assert.equal(nodes(retrieval.tree(), "SaveKnowledgeFailureDialog").length, 0);
  assert.equal(e.calls.filter(c => c.method === "POST").length, 5, "editing never executes a query");
  assert.ok(!text(retrieval.tree()).includes("knowledge.detail."));
  shell.dispose(); retrieval.dispose();
});

test("RAGAS explicit retry clears only prior scoring failure and preserves accepted retrieval", async () => {
  const e = environment(), shell = host("../DatasetDetail.tsx", "KnowledgeDatasetDetailPage", e);
  shell.render(); await shell.flush();
  ((shell.child("RetrievalTab").hitTest as Data).setQuery as (value: string) => void)("current question");
  await shell.flush();
  const hitResult = { document_id: "doc-a", segment_id: "segment-a", text: "accepted context", score: 0.3 };
  e.setWrite(async () => ({ results: [hitResult], metadata: { mode: "hybrid", vector_hits_count: 1 }, trace_id: "accepted-trace", query_fingerprint: "accepted-fingerprint" }));
  await ((shell.child("RetrievalTab").hitTest as Data).runHitTest as () => Promise<void>)(); await shell.flush();
  const retrieval = host("./RetrievalTab.tsx", "RetrievalTab", e, shell.child("RetrievalTab")); retrieval.render();
  e.setWrite(async () => { throw Error("old scoring failure"); });
  await click(retrieval.button(e.i18n.t("knowledge.detail.ragasScore"))); await shell.flush(); retrieval.render(shell.child("RetrievalTab"));
  assert.ok(text(retrieval.tree()).includes("old scoring failure"));
  const pending = deferred<unknown>(); e.setWrite(() => pending.promise);
  const running = click(retrieval.button(e.i18n.t("knowledge.detail.ragasScore"))); await shell.flush(); retrieval.render(shell.child("RetrievalTab"));
  assert.ok(!text(retrieval.tree()).includes("old scoring failure"), "retry must retire the preceding score error");
  assert.equal(retrieval.button(e.i18n.t("knowledge.detail.ragasScoring")).props.disabled, true);
  let hit = shell.child("RetrievalTab").hitTest as Data;
  assert.equal((hit.hitMeta as Data).mode, "hybrid"); assert.equal((hit.hitMeta as Data).vector_hits_count, 1);
  assert.equal(hit.hitTraceId, "accepted-trace"); assert.equal(hit.hitQueryFingerprint, "accepted-fingerprint");
  assert.equal((hit.hitResults as unknown[])[0], hitResult);
  pending.resolve({ results: [{ metric: "context_relevancy", score: 0.9, explanation: "accepted score", label: "pass" }], judge_model: "synthetic-judge" });
  await running; await shell.flush(); retrieval.render(shell.child("RetrievalTab")); hit = shell.child("RetrievalTab").hitTest as Data;
  assert.ok(text(retrieval.tree()).includes("accepted score")); assert.ok(!text(retrieval.tree()).includes("old scoring failure"));
  assert.equal(hit.ragasLoading, false); assert.equal(hit.ragasJudgeModel, "synthetic-judge");
  assert.equal(e.calls.filter(c => c.method === "POST").length, 3, "only explicit test and scoring attempts are sent");
  e.setWrite(async () => { throw Error("fresh scoring failure"); });
  await click(retrieval.button(e.i18n.t("knowledge.detail.ragasScore"))); await shell.flush(); retrieval.render(shell.child("RetrievalTab"));
  assert.ok(text(retrieval.tree()).includes("fresh scoring failure")); assert.ok(!text(retrieval.tree()).includes("accepted score"));
  assert.equal(e.calls.filter(c => c.method === "POST").length, 4);
  shell.dispose(); retrieval.dispose();
});
