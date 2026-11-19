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
  let confirm = false, storageBroken = false, writeStorageBroken = false;
  const calls: { method: string; url?: string; key?: unknown; body?: unknown }[] = [];
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
      async post(url: string, body: unknown) { calls.push({ method: "POST", url, body }); return { data: await write() }; },
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
        setItem: (key: string, value: string) => { if (storageBroken || writeStorageBroken) throw Error("storage unavailable"); storage.set(key, value); },
        removeItem: (key: string) => { if (storageBroken) throw Error("storage unavailable"); storage.delete(key); },
      }, window: { confirm: () => confirm },
    });
    return exports;
  }
  modules["@/types/knowledge"] = load("../../../types/knowledge.ts");
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./batchOperations"] = load("./batchOperations.ts");
  modules["./BatchReceiptPanel"] = load("./BatchReceiptPanel.tsx");
  modules["./datasetAgentImpact"] = load("./datasetAgentImpact.ts");
  modules["./documentPipelineRecovery"] = load("./documentPipelineRecovery.ts");
  const { DocumentsTab } = load("./DocumentsTab.tsx") as { DocumentsTab: (props: Data) => unknown };
  let props: Data = { datasetId: "a", tenantId: "tenant-a", docs: [document()], docsQuery: {}, totalDocuments: 1,
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
    setConfirm: (value: boolean) => { confirm = value; }, breakStorage: () => { storageBroken = true; }, breakStorageWrites: () => { writeStorageBroken = true; }, restoreStorageWrites: () => { writeStorageBroken = false; },
  };
}
function posts(h: ReturnType<typeof host>) { return h.calls.filter((call) => call.method === "POST"); }


const batchReceipts = (h: ReturnType<typeof host>) => [...h.storage].filter(([key]) => key.startsWith("kb-batch-receipt:"));
function panel(h: ReturnType<typeof host>): Node | undefined {
  function find(v: unknown): Node | undefined {
    if (!v || typeof v !== "object") return;
    const n = v as Node;
    if (typeof n.type === "function" && n.type.name === "BatchReceiptPanel") return n;
    for (const value of Object.values(v)) { const result = find(value); if (result) return result; }
  }
  return find(h.current());
}
async function menu(h: ReturnType<typeof host>, key: string) {
  const item = nodes(h.current(), "DropdownMenuItem").find((node) => text(node).includes(`knowledge.detail.${key}`));
  assert.ok(item, key); await (item.props.onClick as () => unknown)(); await h.flush();
}
async function openBatch(h: ReturnType<typeof host>, all = true) {
  await menu(h, "enterBatchMode");
  if (!all) { (h.row().props.onCheck as () => void)(); await h.flush(); }
  await menu(h, all ? "batchReindexAll" : "batchReindex");
}
const operation = (status = "completed") => ({ operation_id: "op-a", dataset_id: "a", tenant_id: "tenant-a", operation: "reembed",
  status, total_count: 1, queued_count: 1, skipped_count: 0, failed_count: 0, problem_items: [], problem_items_truncated: false });

test("refresh while POST is unresolved retains unknown batch guard before any response", async () => {
  const h = host(); const accepted = deferred<unknown>(); h.setWrite(() => accepted.promise); h.render(); await h.flush(); await openBatch(h);
  const first = h.click("confirmReindex"); await h.flush();
  assert.equal(posts(h).length, 1);
  assert.equal(batchReceipts(h).length, 1, "dispatch must durably retain uncertainty before acceptance response");
  h.dispose(); const reopened = host(h.storage); reopened.render(); await reopened.flush();
  assert.ok(panel(reopened)); await openBatch(reopened); await reopened.click("confirmReindex");
  assert.equal(posts(reopened).length, 0, "reopened tab must not replay uncertain batch");
  accepted.reject(Error("response lost")); await first;
});

test("settled lost POST response persists no-ID receipt and reopening never replays", async () => {
  const h = host(); h.render(); await h.flush(); await openBatch(h); await h.click("confirmReindex");
  assert.equal(posts(h).length, 1); assert.equal(batchReceipts(h).length, 1); h.dispose();
  const reopened = host(h.storage); reopened.render(); await reopened.flush();
  const p = panel(reopened); assert.ok(p);
  await (p.props.onRecheck as () => Promise<void>)(); await reopened.flush();
  assert.equal(reopened.calls.filter((c) => c.method === "GET").length, 0);
  await openBatch(reopened); await reopened.click("confirmReindex"); assert.equal(posts(reopened).length, 0);
});

test("accepted-ID poll failure can reopen and query the same operation without POST", async () => {
  const h = host(); h.setWrite(async () => operation("running")); h.setRead(async () => { throw Error("read lost"); });
  h.render(); await h.flush(); await openBatch(h); await h.click("confirmReindex");
  assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].operation.operation_id, "op-a"); h.dispose();
  const reopened = host(h.storage); reopened.setRead(async () => operation()); reopened.render(); await reopened.flush();
  const p = panel(reopened); assert.ok(p); await (p.props.onRecheck as () => Promise<void>)(); await reopened.flush();
  assert.equal(posts(reopened).length, 0);
  assert.deepEqual(reopened.calls.filter((c) => c.method === "GET").map((c) => c.url), ["/api/v1/knowledge/a/document-batches/op-a"]);
  assert.equal(JSON.parse(batchReceipts(reopened)[0][1]).attempts[0].outcomeUnknown, false);
});

test("partial success preserves failed selection and visible per-item receipt across reopen", async () => {
  const partial = { ...operation("partial"), queued_count: 0, failed_count: 1, problem_items: [{ document_id: "doc-a", status: "failed", error_code: "controlled_failure" }] };
  const h = host(); h.setWrite(async () => partial); h.setRead(async () => partial); h.render(); await h.flush();
  await openBatch(h, false); await h.click("confirmReindex");
  assert.equal(h.row().props.checked, true); assert.equal(posts(h).length, 1); h.dispose();
  const reopened = host(h.storage); reopened.render(); await reopened.flush(); const p = panel(reopened); assert.ok(p);
  const rendered = (p.type as (props: Data) => unknown)(p.props);
  assert.ok(text(rendered).includes("knowledge.detail.batchReceiptStatus.failed"));
  assert.ok(text(rendered).includes("op-a")); assert.equal(posts(reopened).length, 0);
});

test("write-only storage failure and malformed receipts prevent new batch dispatch", async () => {
  for (const mode of ["write-denied", "malformed"]) {
    const h = host(); h.render(); await h.flush(); await openBatch(h);
    if (mode === "write-denied") h.breakStorageWrites();
    else h.storage.set("kb-batch-receipt:owner-a:a", "null");
    await h.click("confirmReindex"); assert.equal(posts(h).length, 0, mode);
    assert.ok(text(h.toasts).includes("pipelineOutcomeStorageFailed"));
  }
});

test("unknown batch blocks same-document pipeline actions but preserves unaffected document", async () => {
  const h = host(); h.render(); await h.flush(); await openBatch(h, false); await h.click("confirmReindex");
  for (const action of ["Retry", "Recover", "Reprocess", "Reembed"]) await h.invoke(action);
  assert.equal(posts(h).length, 1);
  h.render({ ...h.props(), docs: [{ ...document(), document_id: "doc-other" }] }); await h.flush();
  await h.invoke("Reembed"); assert.equal(posts(h).length, 2);
});

test("all-document unknown batch guards every row and manual acknowledgement never submits", async () => {
  const h = host(); h.render(); await h.flush(); await openBatch(h); await h.click("confirmReindex");
  h.render({ ...h.props(), docs: [{ ...document(), document_id: "new-doc" }] }); await h.flush();
  await h.invoke("Reembed"); assert.equal(posts(h).length, 1);
  const p = panel(h); assert.ok(p); h.setConfirm(true); await (p.props.onAcknowledge as () => void)(); await h.flush();
  assert.equal(posts(h).length, 1); await h.invoke("Reembed"); assert.equal(posts(h).length, 2);
});

test("scope navigation retires old submission and cannot populate another account receipt", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise); h.render(); await h.flush(); await openBatch(h);
  const first = h.click("confirmReindex"); await h.flush(); const old = batchReceipts(h)[0][1];
  h.auth.user.user_id = "owner-b"; h.render({ ...h.props(), datasetId: "b", docs: [document("error", "b")] }); await h.flush();
  pending.resolve(operation()); await first; await h.flush();
  assert.equal(panel(h), undefined); assert.equal(batchReceipts(h).length, 1); assert.equal(batchReceipts(h)[0][1], old);
  assert.equal(h.calls.filter((c) => c.method === "INVALIDATE").length, 0);
});

test("late old response cannot overwrite an acknowledged and newly submitted remount", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise); h.render(); await h.flush(); await openBatch(h);
  const first = h.click("confirmReindex"); await h.flush(); h.dispose();
  const reopened = host(h.storage); reopened.render(); await reopened.flush(); reopened.setConfirm(true);
  const p = panel(reopened); assert.ok(p); await (p.props.onAcknowledge as () => void)(); await reopened.flush();
  await openBatch(reopened); await reopened.click("confirmReindex"); const newer = batchReceipts(reopened)[0][1];
  pending.resolve(operation()); await first; await reopened.flush();
  assert.equal(batchReceipts(reopened)[0][1], newer); assert.equal(posts(reopened).length, 1);
});

async function selectMany(h: ReturnType<typeof host>) {
  h.render({ ...h.props(), docs: Array.from({ length: 101 }, (_, i) => ({ ...document(), document_id: `doc-${i}` })), totalDocuments: 101 });
  await h.flush(); await menu(h, "enterBatchMode"); await menu(h, "selectCurrentPage"); await menu(h, "batchReindex");
}
test("each chunk is persisted before dispatch and second pending chunk survives reopen", async () => {
  const h = host(); const pending = deferred<unknown>(); let count = 0;
  const first = { ...operation(), total_count: 100, queued_count: 100 };
  h.setWrite(async () => {
    count++; const receipt = JSON.parse(batchReceipts(h)[0][1]);
    assert.equal(receipt.attempts.length, count); assert.equal(receipt.attempts[count - 1].outcomeUnknown, true);
    return count === 1 ? first : pending.promise;
  }); h.setRead(async () => first); h.render(); await h.flush(); await selectMany(h);
  const submit = h.click("confirmReindex"); await h.flush(); assert.equal(posts(h).length, 2);
  h.dispose(); const reopened = host(h.storage); reopened.render(); await reopened.flush();
  const receipt = JSON.parse(batchReceipts(reopened)[0][1]); assert.equal(receipt.attempts[0].outcomeUnknown, false);
  assert.equal(receipt.attempts[1].outcomeUnknown, true); assert.deepEqual(receipt.attempts[1].requestedIds, ["doc-100"]);
  await openBatch(reopened); await reopened.click("confirmReindex"); assert.equal(posts(reopened).length, 0);
  pending.reject(Error("lost")); await submit;
});

test("navigation while first chunk is pending prevents submission of later chunks", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise); h.setRead(async () => ({ ...operation(), total_count: 100, queued_count: 100 }));
  h.render(); await h.flush(); await selectMany(h); const submit = h.click("confirmReindex"); await h.flush();
  h.render({ ...h.props(), datasetId: "b", docs: [document("error", "b")] });
  assert.equal(panel(h), undefined, "first navigation render must hide previous scope"); await h.flush();
  pending.resolve(operation()); await submit; await h.flush(); assert.equal(posts(h).length, 1);
});

test("failed persistence after known acceptance retains pre-dispatch guard and reports storage failure", async () => {
  const h = host(); h.setWrite(async () => { h.breakStorageWrites(); return operation(); }); h.setRead(async () => operation());
  h.render(); await h.flush(); await openBatch(h); await h.click("confirmReindex");
  const receipt = JSON.parse(batchReceipts(h)[0][1]); assert.equal(receipt.attempts[0].outcomeUnknown, true);
  assert.ok(text(h.toasts).includes("pipelineOutcomeStorageFailed"));
  assert.equal(h.toasts.some((t) => t.kind === "success"), false);
  await h.invoke("Reembed"); assert.equal(posts(h).length, 1);
  const p = panel(h); assert.ok(p);
  const visible = p.props.receipt as { attempts: { outcomeUnknown: boolean; operation?: { operation_id: string } }[] };
  assert.equal(visible.attempts[0].outcomeUnknown, true);
  assert.equal(visible.attempts[0].operation?.operation_id, "op-a");
  h.restoreStorageWrites(); await (p.props.onRecheck as () => Promise<void>)(); await h.flush();
  assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, false);
  assert.equal(posts(h).length, 1);
});

const impact = { dataset_id: "a", hidden_agent_count: 0, visible_agents: [], counts: {
  current_draft: { visible: 0, hidden: 0 }, active_publication: { visible: 0, hidden: 0 }, historical_version: { visible: 0, hidden: 0 } } };
async function openDelete(h: ReturnType<typeof host>) {
  await menu(h, "enterBatchMode"); (h.row().props.onCheck as () => void)(); await h.flush();
  h.setRead(async () => impact); await menu(h, "batchDelete");
}
test("delete preserves impact check and persists pending receipt before POST", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise); h.render(); await h.flush(); await openDelete(h);
  const submit = h.click("confirmBatchDelete"); await h.flush(); assert.equal(posts(h).length, 1);
  assert.equal(JSON.parse(batchReceipts(h)[0][1]).action, "delete");
  h.dispose(); const reopened = host(h.storage); reopened.render(); await reopened.flush(); await openDelete(reopened);
  await reopened.click("confirmBatchDelete"); assert.equal(posts(reopened).length, 0);
  pending.reject(Error("lost")); await submit;
});
test("delete cannot dispatch without storage or a valid impact receipt", async () => {
  const h = host(); h.render(); await h.flush(); await openDelete(h); h.breakStorageWrites();
  await h.click("confirmBatchDelete"); assert.equal(posts(h).length, 0);
  const invalid = host(); invalid.render(); await invalid.flush(); await menu(invalid, "enterBatchMode");
  (invalid.row().props.onCheck as () => void)(); await invalid.flush(); await menu(invalid, "batchDelete");
  assert.equal(invalid.button("confirmBatchDelete").props.disabled, true);
  await (invalid.button("confirmBatchDelete").props.onClick as () => Promise<void>)(); assert.equal(posts(invalid).length, 0);
});

test("active first chunk also guards pipeline actions for documents reserved in later chunks", async () => {
  const h = host(); const pending = deferred<unknown>(); h.setWrite(() => pending.promise);
  h.render(); await h.flush(); await selectMany(h); const submit = h.click("confirmReindex"); await h.flush();
  const last = nodes(h.current(), "DocumentRow").find((node) => (node.props.doc as { document_id: string }).document_id === "doc-100");
  assert.ok(last); const retry = (last.props.onRetry as () => Promise<void>)(); await h.flush();
  const count = posts(h).length; pending.reject(Error("lost")); await Promise.all([submit, retry]); assert.equal(count, 1);
});

test("delete terminal persistence failure keeps known-ID recovery controls until storage is restored", async () => {
  const h = host(); const accepted = { ...operation(), operation: "delete" };
  h.render(); await h.flush(); await openDelete(h);
  h.setWrite(async () => { h.breakStorageWrites(); return accepted; }); h.setRead(async () => accepted);
  await h.click("confirmBatchDelete");
  assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, true);
  const p = panel(h); assert.ok(p);
  const visible = p.props.receipt as { attempts: { outcomeUnknown: boolean; operation?: { operation_id: string } }[] };
  assert.equal(visible.attempts[0].outcomeUnknown, true); assert.equal(visible.attempts[0].operation?.operation_id, "op-a");
  assert.ok(text(h.toasts).includes("pipelineOutcomeStorageFailed")); assert.equal(h.toasts.some((t) => t.kind === "success"), false);
  h.restoreStorageWrites(); await (p.props.onRecheck as () => Promise<void>)(); await h.flush();
  assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, false); assert.equal(posts(h).length, 1);
});

test("terminal-only storage failure during recheck preserves unknown controls for both batch actions", async () => {
  for (const action of ["reembed", "delete"]) {
    const h = host(); h.render(); await h.flush();
    if (action === "delete") await openDelete(h); else await openBatch(h);
    h.setWrite(async () => ({ ...operation("running"), operation: action })); h.setRead(async () => { throw Error("status unavailable"); });
    await h.click(action === "delete" ? "confirmBatchDelete" : "confirmReindex");
    h.setRead(async () => ({ ...operation(), operation: action })); h.breakStorageWrites();
    let p = panel(h); assert.ok(p); await (p.props.onRecheck as () => Promise<void>)(); await h.flush();
    p = panel(h); assert.ok(p);
    const visible = p.props.receipt as { attempts: { outcomeUnknown: boolean }[] };
    assert.equal(visible.attempts[0].outcomeUnknown, true, action);
    assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, true);
    assert.equal(h.toasts.some((t) => t.kind === "success"), false);
    h.restoreStorageWrites(); await (p.props.onRecheck as () => Promise<void>)(); await h.flush();
    assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, false); assert.equal(posts(h).length, 1);
  }
});

for (const action of ["reembed", "delete"] as const) {
  test(`independent review: ${action} terminal-only storage failure retains recovery then read-only recheck resolves`, async () => {
    const h = host();
    h.setWrite(async () => ({ ...operation("running"), operation: action }));
    h.render(); await h.flush();
    if (action === "delete") await openDelete(h); else await openBatch(h);
    h.setRead(async () => { h.breakStorageWrites(); return { ...operation(), operation: action }; });
    await h.click(action === "delete" ? "confirmBatchDelete" : "confirmReindex");
    const disk = JSON.parse(batchReceipts(h)[0][1]);
    assert.equal(disk.attempts[0].outcomeUnknown, true);
    assert.equal(disk.attempts[0].operation.operation_id, "op-a");
    const p = panel(h); assert.ok(p);
    const shown = p.props.receipt as { attempts: { outcomeUnknown: boolean; operation?: { operation_id: string } }[] };
    assert.equal(shown.attempts[0].outcomeUnknown, true);
    assert.equal(shown.attempts[0].operation?.operation_id, "op-a");
    assert.equal(h.toasts.some((entry) => entry.kind === "success"), false);
    h.restoreStorageWrites(); h.setRead(async () => ({ ...operation(), operation: action }));
    await (p.props.onRecheck as () => Promise<void>)(); await h.flush();
    assert.equal(JSON.parse(batchReceipts(h)[0][1]).attempts[0].outcomeUnknown, false);
    assert.equal(posts(h).length, 1);
  });
}


function seedSelectionReceipt(h: ReturnType<typeof host>, action = "reembed") {
  const receipt = { attemptId: "selection-attempt", action, requestedCount: 4,
    requestedIds: ["done", "doc-a", "off-page-failed", "not-submitted"], attempts: [{
      requestedIds: ["done", "doc-a", "off-page-failed"], outcomeUnknown: false,
      operation: { ...operation("partial"), operation: action, total_count: 3, skipped_count: 1, failed_count: 1,
        problem_items: [{ document_id: "doc-a", status: "skipped" }, { document_id: "off-page-failed", status: "failed" }] },
    }] };
  h.storage.set("kb-batch-receipt:owner-a:a", JSON.stringify(receipt)); return receipt;
}

for (const action of ["reembed", "delete"]) test(`reopened ${action} receipt restores only remaining cross-page selection without HTTP`, async () => {
  const h = host(); seedSelectionReceipt(h, action); h.render(); await h.flush();
  const p = panel(h); assert.ok(p); const initial = batchReceipts(h)[0][1];
  const rendered = (p.type as (props: Data) => unknown)(p.props);
  const button = nodes(rendered, "Button").find((node) => text(node).includes("batchReceiptSelectRemaining")); assert.ok(button);
  await (button.props.onClick as () => void)(); await h.flush();
  assert.equal(h.row().props.checked, true); assert.equal(h.calls.length, 0);
  assert.equal(batchReceipts(h)[0][1], initial);
  const confirmKey = action === "delete" ? "confirmBatchDelete" : "confirmReindex";
  if (action === "delete") h.setRead(async () => impact);
  await menu(h, action === "delete" ? "batchDelete" : "batchReindex");
  const confirmation = h.button(confirmKey); assert.ok(confirmation);
  assert.equal(posts(h).length, 0, "opening existing review is still separate from submit");
  if (action === "delete") assert.ok(h.calls.some((call) => call.method === "GET" && call.url?.endsWith("/datasets/a/impact")));
  h.setWrite(async () => { throw Error("controlled stop"); }); await h.click(confirmKey);
  const dispatch = JSON.parse(batchReceipts(h)[0][1]);
  assert.deepEqual(dispatch.requestedIds, ["doc-a", "off-page-failed", "not-submitted"]);
  assert.equal(posts(h).length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(posts(h)[0].body)).document_ids, dispatch.requestedIds);
  assert.ok(posts(h)[0].url?.endsWith(action === "delete" ? "/batch-delete" : "/batch-reindex"));
});

test("stale receipt callback cannot replace selection after receipt, account, dataset, tenant or permission changes", async () => {
  for (const change of ["receipt", "account", "dataset", "tenant", "permission", "disposed"]) {
    const h = host(); const receipt = seedSelectionReceipt(h); h.render(); await h.flush();
    const p = panel(h); assert.ok(p); const stale = p.props.onSelectRemaining as () => void;
    if (change === "receipt") { receipt.attempts[0].operation.problem_items[0].status = "failed"; h.storage.set("kb-batch-receipt:owner-a:a", JSON.stringify(receipt)); }
    if (change === "account") { h.auth.user = { user_id: "owner-b" }; h.render(); }
    if (change === "dataset") h.render({ ...h.props(), datasetId: "b" });
    if (change === "tenant") h.render({ ...h.props(), tenantId: "tenant-b" });
    if (change === "permission") h.render({ ...h.props(), permission: "viewer" });
    if (change === "disposed") h.dispose();
    stale(); await h.flush(); assert.equal(Boolean(h.row().props.checked), false, change); assert.equal(posts(h).length, 0);
  }
});
