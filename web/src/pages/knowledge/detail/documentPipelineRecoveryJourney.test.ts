import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  const node = value as Node;
  return [...(node.type === type ? [node] : []), ...Object.values(value).flatMap((item) => nodes(item, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (!value || typeof value !== "object") return "";
  return Object.values(value).map(text).join(" ");
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const document = (status = "error", dataset = "a") => ({ document_id: "doc-a", dataset_id: dataset,
  title: "source.txt", status, enabled: true, archived: false,
  processing_diagnostics: { next_action: "retry_document", retry_blocked_reason: null } });

// Real DocumentsTab/API/helpers with controlled hook scheduling, storage and HTTP.
// This is component-logic evidence, not React DOM/browser or real backend acceptance.
function host(storage = new Map<string, string>()) {
  const slots: unknown[] = [], cleanups = new Map<number, () => void>();
  let cursor = 0, dirty = true, effects: (() => void)[] = [], tree: unknown;
  let confirm = false, storageBroken = false;
  const calls: { method: string; url?: string; key?: unknown }[] = [];
  const toasts: { kind: string; args: unknown[] }[] = [];
  const streams: unknown[][] = [];
  const auth = { user: { user_id: "owner-a" } };
  let read = async (): Promise<unknown> => document();
  let write = async (): Promise<unknown> => { throw new Error("controlled response lost after acceptance"); };
  let invalidate = async (): Promise<void> => {};
  const same = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const react = {
    useState(value: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = typeof value === "function" ? value() : value;
      return [slots[i], (next: unknown) => { const v = typeof next === "function" ? next(slots[i]) : next; if (!Object.is(v, slots[i])) { slots[i] = v; dirty = true; } }]; },
    useRef(value: unknown) { const i = cursor++; return slots[i] ?? (slots[i] = { current: value }); },
    useMemo(f: () => unknown, deps: unknown[]) { const i = cursor++; const old = slots[i] as { deps: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) slots[i] = { deps, value: f() }; return (slots[i] as { value: unknown }).value; },
    useEffect(f: () => void | (() => void), deps: unknown[]) { const i = cursor++; if (!same(slots[i] as unknown[], deps)) {
      slots[i] = deps; effects.push(() => { cleanups.get(i)?.(); const fn = f(); if (fn) cleanups.set(i, fn); else cleanups.delete(i); }); } },
  };
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-router-dom": { useSearchParams: () => [new URLSearchParams()] },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@tanstack/react-query": { useQueryClient: () => ({ invalidateQueries: async (value: { queryKey: unknown }) => {
      calls.push({ method: "INVALIDATE", key: value.queryKey }); await invalidate(); } }) },
    "@/store/useAuthStore": { useAuthStore: (f: (state: typeof auth) => unknown) => f(auth) },
    "@/hooks/use-toast": { toast: new Proxy({}, { get: (_target, key) => (...args: unknown[]) => toasts.push({ kind: String(key), args }) }) },
    "@/hooks/useKnowledge": { useDebouncedValue: (value: unknown) => value,
      useDocumentProgressStream: (...args: unknown[]) => streams.push(args),
      useSelectedDocument: () => ({}), useSegments: () => ({ data: { items: [], total: 0 } }) },
    "./selectedDocumentQuery": { refreshSelectedDocument: async (_client: unknown, ...identity: unknown[]) => { calls.push({ method: "DETAIL", key: identity }); } },
    "./documentStages": { lastStartedDocumentStage: () => null },
    "@/lib/sse": {},
    "@/lib/api": { api: {
      async get(url: string) { calls.push({ method: "GET", url }); return { data: await read() }; },
      async post(url: string) { calls.push({ method: "POST", url }); return { data: await write() }; },
    } },
  };
  function load(file: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText;
    vm.runInNewContext(code, { exports, require: (id: string) => modules[id] ?? named, Error, Set, Map,
      console: { error: () => {} }, crypto: { randomUUID }, URLSearchParams,
      sessionStorage: {
        getItem: (key: string) => { if (storageBroken) throw Error("storage unavailable"); return storage.get(key) ?? null; },
        setItem: (key: string, value: string) => { if (storageBroken) throw Error("storage unavailable"); storage.set(key, value); },
        removeItem: (key: string) => { if (storageBroken) throw Error("storage unavailable"); storage.delete(key); },
      }, window: { confirm: () => confirm },
    });
    return exports;
  }
  modules["@/types/knowledge"] = load("../../../types/knowledge.ts");
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./datasetAgentImpact"] = load("./datasetAgentImpact.ts");
  modules["./documentPipelineRecovery"] = load("./documentPipelineRecovery.ts");
  const { DocumentsTab } = load("./DocumentsTab.tsx") as { DocumentsTab: (props: Data) => unknown };
  let props: Data = { datasetId: "a", docs: [document()], docsQuery: {}, totalDocuments: 1,
    documentLimit: 20, documentOffset: 0, onDocumentOffsetChange: () => {}, permission: "owner",
    fileInputRef: { current: null }, uploading: false };
  function render(next = props) { props = next; cursor = 0; dirty = false; tree = DocumentsTab(props); const queue = effects; effects = []; queue.forEach((f) => f()); }
  async function flush() { for (let i = 0; i < 5; i++) { await new Promise<void>((resolve) => setImmediate(resolve)); if (dirty) render(); } }
  const row = () => nodes(tree, "DocumentRow")[0];
  function button(key: string) { const result = nodes(tree, "Button").find((node) => text(node).includes(`knowledge.detail.${key}`)); assert.ok(result, key); return result; }
  async function click(key: string) { const b = button(key); assert.equal(Boolean(b.props.disabled), false, key); await (b.props.onClick as () => Promise<void>)(); await flush(); }
  return { storage, calls, toasts, streams, auth, render, flush, row, button, click, props: () => props,
    invoke: (action = "Retry") => (row().props[`on${action}`] as () => Promise<void>)(),
    current: () => tree, dispose: () => cleanups.forEach((f) => f()),
    setRead: (fn: typeof read) => { read = fn; }, setWrite: (fn: typeof write) => { write = fn; },
    setInvalidate: (fn: typeof invalidate) => { invalidate = fn; },
    setConfirm: (value: boolean) => { confirm = value; }, breakStorage: () => { storageBroken = true; },
  };
}
function posts(h: ReturnType<typeof host>) { return h.calls.filter((call) => call.method === "POST"); }
function receipts(h: ReturnType<typeof host>) { return [...h.storage].filter(([key]) => key.startsWith("kb-pipeline-attempt:")); }

for (const action of ["Retry", "Recover", "Reprocess", "Reembed"]) {
  test(`${action}: lost response stays guarded and only reads original document`, async () => {
    const h = host(); h.render(); await h.flush(); await h.invoke(action); await h.flush();
    assert.equal(posts(h).length, 1); assert.equal(receipts(h).length, 1); assert.equal(h.row().props.busyLifecycle, true);
    assert.ok(h.calls.some((call) => call.method === "GET" && call.url === "/api/v1/knowledge/a/documents/doc-a"));
    assert.equal(text(h.toasts).includes("controlled response lost"), false);
    for (const next of ["Retry", "Recover", "Reprocess", "Reembed"]) await h.invoke(next);
    assert.equal(posts(h).length, 1);
  });
}
test("old/new terminal and active GETs never settle a request without execution identity", async () => {
  const h = host(); h.render(); await h.flush(); await h.invoke(); await h.flush();
  for (const status of ["error", "completed", "waiting"]) {
    h.setRead(async () => document(status)); await h.click("pipelineOutcomeRecheck");
    assert.equal(receipts(h).length, 1); assert.equal(posts(h).length, 1);
  }
  h.setRead(async () => { throw { response: { status: 403 } }; }); await h.click("pipelineOutcomeRecheck");
  assert.ok(text(h.current()).includes("pipelineOutcomeReadFailed")); assert.equal(receipts(h).length, 1);
});
test("refresh restores guard; explicit acknowledgement unlocks but never submits", async () => {
  const h = host(); h.render(); await h.flush(); await h.invoke(); await h.flush(); h.dispose();
  const next = host(h.storage); next.render(); await next.flush();
  assert.equal(next.row().props.busyLifecycle, true); await next.invoke(); assert.equal(posts(next).length, 0);
  await next.click("pipelineOutcomeAcknowledge"); assert.equal(receipts(next).length, 1);
  next.setConfirm(true); await next.click("pipelineOutcomeAcknowledge");
  assert.equal(receipts(next).length, 0); assert.equal(posts(next).length, 0); assert.equal(next.row().props.busyLifecycle, false);
});
test("late old POST cannot remove a newer remounted attempt receipt", async () => {
  const h = host(); const old = deferred<unknown>(); h.setWrite(() => old.promise); h.render(); await h.flush();
  const first = h.invoke(); await h.flush(); const original = receipts(h)[0][1]; h.dispose();
  const next = host(h.storage); const pending = deferred<unknown>(); next.setWrite(() => pending.promise); next.render(); await next.flush();
  next.setConfirm(true); await next.click("pipelineOutcomeAcknowledge"); const second = next.invoke(); await next.flush();
  const newer = receipts(next)[0][1]; assert.notEqual(newer, original);
  old.resolve({ status: "waiting", execution_id: "old-execution" }); await first; await next.flush();
  assert.equal(receipts(next)[0][1], newer);
  pending.reject(Error("new response lost")); await second; await next.flush(); assert.equal(receipts(next)[0][1], newer);
});
test("account/dataset switches and A-B-A fence stale document GET completion", async () => {
  const h = host(); h.render(); await h.flush(); await h.invoke(); await h.flush();
  const old = deferred<unknown>(); h.setRead(() => old.promise); const checking = h.click("pipelineOutcomeRecheck"); await h.flush();
  h.auth.user.user_id = "owner-b"; h.render(); await h.flush(); assert.equal(text(h.current()).includes("pipelineOutcomeUnknown"), false);
  h.auth.user.user_id = "owner-a"; h.render({ ...h.props(), datasetId: "b", docs: [document("error", "b")] }); await h.flush();
  assert.equal(text(h.current()).includes("pipelineOutcomeUnknown"), false);
  h.render({ ...h.props(), datasetId: "a", docs: [document()] }); await h.flush();
  old.resolve(document("waiting")); await checking; await h.flush();
  assert.equal(text(h.current()).includes("pipelineOutcomeProcessing"), false); assert.equal(Boolean(h.button("pipelineOutcomeRecheck").props.disabled), false); assert.equal(receipts(h).length, 1); assert.equal(posts(h).length, 1);
});
test("definite rejection clears attempt; 5xx retains it; no error body rendered", async () => {
  for (const status of [400, 401, 403, 404, 409, 422, 500, 503]) {
    const h = host(); h.setWrite(async () => { throw { response: { status, data: { detail: "private upstream trace" } } }; });
    h.render(); await h.flush(); await h.invoke(); await h.flush();
    assert.equal(receipts(h).length, status >= 500 ? 1 : 0); assert.equal(posts(h).length, 1);
    assert.equal(text(h.current()).includes("private upstream trace"), false);
  }
});
test("storage failure blocks dispatch and known acceptance is not rewritten as rejection by read failure", async () => {
  const blocked = host(); blocked.breakStorage(); blocked.render(); await blocked.flush(); await blocked.invoke(); assert.equal(posts(blocked).length, 0);
  const h = host(); h.setWrite(async () => ({ status: "waiting", execution_id: "exec-a" }));
  h.setInvalidate(async () => { throw Error("read failure"); }); h.render(); await h.flush(); await h.invoke(); await h.flush();
  assert.equal(posts(h).length, 1); assert.equal(receipts(h).length, 1);
  assert.equal(JSON.parse(receipts(h)[0][1])[0].executionId, "exec-a");
  assert.equal(JSON.parse(receipts(h)[0][1])[0].mutationAccepted, true);
  await h.invoke(); assert.equal(posts(h).length, 1);
  assert.ok(text(h.toasts).includes("pipelineOutcomeAcceptedReadFailed")); assert.equal(text(h.toasts).includes("documentActionFailed"), false);
});
test("rapid repeated callbacks dispatch once while first POST is pending", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise); h.render(); await h.flush();
  const first = h.invoke(); await h.invoke(); assert.equal(posts(h).length, 1); await h.flush();
  assert.equal(h.button("pipelineOutcomeAcknowledge").props.disabled, true);
  pending.reject(Error("lost")); await first; await h.flush(); assert.equal(receipts(h).length, 1);
});
test("confirmed acceptance plus successful refresh releases the persisted guard", async () => {
  const h = host(); h.setWrite(async () => ({ status: "waiting", execution_id: "exec-a" }));
  h.render(); await h.flush(); await h.invoke(); await h.flush();
  assert.equal(posts(h).length, 1); assert.equal(receipts(h).length, 0);
  assert.ok(h.calls.some((call) => call.method === "INVALIDATE"));
});
test("batch reembed cannot bypass an unresolved single-document request", async () => {
  const h = host(); h.render(); await h.flush(); await h.invoke(); await h.flush();
  const enter = nodes(h.current(), "DropdownMenuItem").find((node) => text(node).includes("knowledge.detail.enterBatchMode"));
  assert.ok(enter); (enter.props.onClick as () => void)(); await h.flush();
  const openAll = nodes(h.current(), "DropdownMenuItem").find((node) => text(node).includes("knowledge.detail.batchReindexAll"));
  assert.ok(openAll); (openAll.props.onClick as () => void)(); await h.flush();
  await h.click("confirmReindex"); assert.equal(posts(h).length, 1);
  assert.ok(text(h.toasts).includes("pipelineOutcomeUnknown"));
});
test("wrong-document status read stays failed and cannot remove receipt", async () => {
  const h = host(); h.setRead(async () => document("completed", "other-dataset"));
  h.render(); await h.flush(); await h.invoke(); await h.flush();
  assert.ok(text(h.current()).includes("pipelineOutcomeReadFailed")); assert.equal(receipts(h).length, 1);
  assert.equal(h.calls.some((call) => call.method === "INVALIDATE"), false);
});

test("A-B-A starts a current read even while an obsolete generation GET remains pending", async () => {
  const h = host(); h.render(); await h.flush(); await h.invoke(); await h.flush();
  const old = deferred<unknown>(); h.setRead(() => old.promise); const obsolete = h.click("pipelineOutcomeRecheck"); await h.flush();
  h.render({ ...h.props(), datasetId: "b", docs: [document("error", "b")] }); await h.flush();
  h.render({ ...h.props(), datasetId: "a", docs: [document()] }); await h.flush();
  const before = h.calls.filter((call) => call.method === "GET").length;
  h.setRead(async () => document("completed")); await h.click("pipelineOutcomeRecheck");
  assert.equal(h.calls.filter((call) => call.method === "GET").length, before + 1);
  assert.ok(text(h.current()).includes("pipelineOutcomeStillUnknown"));
  old.resolve(document("waiting")); await obsolete; await h.flush();
  assert.equal(text(h.current()).includes("pipelineOutcomeProcessing"), false);
  assert.ok(text(h.current()).includes("pipelineOutcomeStillUnknown")); assert.equal(posts(h).length, 1);
});
