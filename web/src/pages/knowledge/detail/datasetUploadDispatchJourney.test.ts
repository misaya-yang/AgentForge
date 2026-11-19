import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
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
function files(count: number) { return Array.from({ length: count }, (_, i) => new File([`fixture ${i}`], `file-${i}.txt`, { type: "text/plain" })); }

// Complete production dialog/controller/model/API modules; only React scheduling
// and network/storage collaborators are controlled. No browser/backend acceptance.
function host(count = 1, saved = new Map<string, string>()) {
  const slots: unknown[] = [], cleanups = new Map<number, () => void>();
  let cursor = 0, dirty = true, effects: (() => void)[] = [], tree: unknown;
  const selected = files(count), toasts: unknown[] = [];
  const auth = { token: "synthetic-token-a", isAuthenticated: true,
    user: { user_id: "owner-a", roles: ["member"], permissions: ["knowledge:write"], tier: "normal" } };
  const subscribers = new Set<(current: typeof auth, previous: typeof auth) => void>();
  let storageFailure = false;
  const storage = { get length() { return saved.size; }, key: (index: number) => [...saved.keys()][index] ?? null,
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => { if (storageFailure) throw Error("controlled storage full"); saved.set(key, value); },
    removeItem: (key: string) => { saved.delete(key); },
  };
  const calls: { method: string; url: string; payload?: unknown; files?: File[] }[] = [];
  const pending: ReturnType<typeof deferred<unknown>>[] = [];
  let put = async (): Promise<unknown> => ({ status: "success" });
  let post = async (): Promise<unknown> => { const next = deferred<unknown>(); pending.push(next); return next.promise; };
  let invalidationCalls = 0;
  let invalidate = async (): Promise<void> => {};
  let props: Data = { datasetId: "dataset-a", dataset: { dataset_id: "dataset-a", tenant_id: "tenant-a", my_permission: "owner", embedding_provider: "dashscope", embedding_model: "text-embedding-v4", statistics: { document_count: 0 } },
    open: true, pendingFiles: selected, uploading: false,
    onOpenChange: (value: boolean) => { props.open = value; dirty = true; },
    onPendingFilesChange: (value: File[]) => { props.pendingFiles = value; dirty = true; },
    onUploadingChange: (value: boolean) => { if (props.uploading !== value) { props.uploading = value; dirty = true; } },
    onFilesSelected: () => {}, onOpenFilePicker: () => {} };
  const same = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  function memo<T>(factory: () => T, deps: unknown[]): T {
    const i = cursor++; const previous = slots[i] as { value: T; deps: unknown[] } | undefined;
    if (!previous || !same(previous.deps, deps)) slots[i] = { value: factory(), deps };
    return (slots[i] as { value: T }).value;
  }
  const react = {
    useMemo: memo, useCallback: <T>(value: T, deps: unknown[]) => memo(() => value, deps),
    useLayoutEffect: (effect: () => void | (() => void), deps: unknown[]): void => { react.useEffect(effect, deps); },
    useState(value: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = typeof value === "function" ? value() : value;
      return [slots[i], (next: unknown) => { const v = typeof next === "function" ? next(slots[i]) : next; if (!Object.is(v, slots[i])) { slots[i] = v; dirty = true; } }]; },
    useRef(value: unknown) { const i = cursor++; return slots[i] ?? (slots[i] = { current: value }); },
    useEffect(f: () => void | (() => void), deps: unknown[]) { const i = cursor++; if (!same(slots[i] as unknown[], deps)) {
      slots[i] = deps; effects.push(() => { cleanups.get(i)?.(); const fn = f(); if (fn) cleanups.set(i, fn); else cleanups.delete(i); }); } },
  };
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, data: Data) => ({ type, props: data });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@tanstack/react-query": { useQueryClient: () => ({ invalidateQueries: () => { invalidationCalls++; return invalidate(); } }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((f: (value: typeof auth) => unknown) => f(auth), {
      getState: () => auth, subscribe: (fn: (current: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); },
    }) },
    "@/hooks/use-toast": { toast: new Proxy({}, { get: (_target, key) => (...args: unknown[]) => toasts.push({ kind: key, args }) }) },
    "@/lib/sse": {},
    "@/lib/api": { api: {
      async put(url: string, payload: unknown) { calls.push({ method: "PUT", url, payload: structuredClone(payload) }); return { data: await put() }; },
      async post(url: string, body: { file?: File; files?: File[] }) { calls.push({ method: "POST", url, files: body.file ? [body.file] : Array.from(body.files ?? []) }); return { data: await post() }; },
    } },
  };
  function load(file: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText;
    vm.runInNewContext(code, { exports, require: (id: string) => modules[id] ?? named, Error, Set, Map,
      console: { error: () => {} }, FormData, File, Blob, TextEncoder, crypto,
      sessionStorage: storage, window: { confirm: () => true } });
    return exports;
  }
  modules["@/lib/utils"] = { generateUUID: () => crypto.randomUUID() };
  modules["@/lib/sdkAuthorityScope"] = load("../../../lib/sdkAuthorityScope.ts");
  modules["@/lib/knowledgeUploadRecovery"] = load("../../../lib/knowledgeUploadRecovery.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = load("../../../hooks/useAuthenticatedReadScope.ts");
  modules["@/types/knowledge"] = load("../../../types/knowledge.ts");
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["@/pages/knowledge/create/datasetCreateModel"] = load("../create/datasetCreateModel.ts");
  modules["@/pages/knowledge/detail/datasetUploadModel"] = load("./datasetUploadModel.ts");
  modules["@/pages/knowledge/detail/useDatasetUploadController"] = load("./useDatasetUploadController.ts");
  const { DatasetUploadDialog } = load("./DatasetUploadDialog.tsx") as { DatasetUploadDialog: (props: Data) => unknown };
  function render(next = props) {
    if (next.datasetId !== props.datasetId && next.dataset === props.dataset) next = { ...next, dataset: { ...(next.dataset as Data), dataset_id: next.datasetId } };
    props = next; cursor = 0; dirty = false; tree = DatasetUploadDialog(props); const queue = effects; effects = []; queue.forEach((f) => f()); }
  async function flush() { for (let i = 0; i < 4; i++) { await new Promise<void>((resolve) => setImmediate(resolve)); if (dirty) render(); } }
  function confirm() { const result = nodes(tree, "Button").find((node) => text(node).includes("knowledge.detail.uploadConfirm")); assert.ok(result); return result; }
  function invoke() { return (confirm().props.onClick as () => Promise<void>)(); }
  return { calls, selected, pending, auth, render, flush, confirm, invoke, toasts, saved,
    invalidations: () => invalidationCalls,
    failStorage: () => { storageFailure = true; },
    changeAuth: (next: Partial<typeof auth>) => { const previous = structuredClone(auth); Object.assign(auth, next); subscribers.forEach((fn) => fn(auth, previous)); },
    recovery: () => [...saved.values()].flatMap((raw) => JSON.parse(raw) as Array<{ files: Array<{ state: string; documentId?: string }> }>),
    props: () => props, current: () => tree, dispose: () => cleanups.forEach((f) => f()),
    setPut: (fn: typeof put) => { put = fn; }, setPost: (fn: typeof post) => { post = fn; },
    setInvalidate: (fn: typeof invalidate) => { invalidate = fn; },
  };
}
const posts = (h: ReturnType<typeof host>) => h.calls.filter((call) => call.method === "POST");
const puts = (h: ReturnType<typeof host>) => h.calls.filter((call) => call.method === "PUT");
const receipt = (count: number) => count >= 3 ? { accepted: count, errors: [] } : { document_id: "doc-a", status: "waiting" };

for (const count of [1, 3]) {
  for (const rerender of [false, true]) {
    test(`${count} file(s): repeat callback ${rerender ? "after render" : "same turn"} dispatches once`, async () => {
      const h = host(count); h.render(); await h.flush();
      const action = h.confirm().props.onClick as () => Promise<void>;
      const first = action();
      if (rerender) { await h.flush(); await h.invoke(); } else await action();
      await h.flush();
      assert.equal(puts(h).length, 1); assert.equal(posts(h).length, 1);
      assert.equal(h.props().uploading, true); assert.equal(h.confirm().props.disabled, true);
      assert.deepEqual(posts(h)[0].files?.map((file) => file.name), h.selected.map((file) => file.name));
      h.pending[0].resolve(receipt(count)); await first; await h.flush();
      assert.equal(h.props().uploading, false); assert.equal(h.props().open, false); assert.deepEqual(h.props().pendingFiles, []);
    });
  }
}
test("config failure releases the guard; a later explicit retry can run", async () => {
  const h = host(); h.setPut(async () => { throw Error("controlled config failure"); }); h.render(); await h.flush();
  await h.invoke(); await h.flush(); assert.equal(posts(h).length, 0); assert.equal(h.props().uploading, false);
  assert.equal((h.props().pendingFiles as File[]).length, 1);
  h.setPut(async () => ({ status: "success" })); const second = h.invoke(); await h.flush();
  assert.equal(puts(h).length, 2); assert.equal(posts(h).length, 1); h.pending[0].resolve(receipt(1)); await second; await h.flush();
  assert.equal(h.props().uploading, false);
});
test("definite upload rejection allows only manual retry; network uncertainty is not retry-safe", async () => {
  for (const safe of [true, false]) {
    const h = host(); h.setPost(async () => { throw safe ? { response: { status: 422 } } : Error("controlled response loss"); });
    h.render(); await h.flush(); await h.invoke(); await h.flush();
    assert.equal(posts(h).length, 1); assert.equal(h.props().uploading, false);
    assert.equal((h.props().pendingFiles as File[]).length, safe ? 1 : 0);
    if (!safe) { assert.equal(h.confirm().props.disabled, true); assert.ok(text(h.current()).includes("uploadInspectBeforeRetry")); }
    if (safe) { await h.invoke(); await h.flush(); assert.equal(posts(h).length, 2); }
  }
});
test("later file additions survive original settlement and are not automatically uploaded", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  const added = new File(["later source"], "later.txt"); h.render({ ...h.props(), pendingFiles: [...h.selected, added] }); await h.flush();
  h.pending[0].resolve(receipt(1)); await first; await h.flush();
  assert.deepEqual(h.props().pendingFiles, [added]); assert.equal(h.props().open, true); assert.equal(h.props().uploading, false); assert.equal(posts(h).length, 1);
  const next = h.invoke(); await h.flush(); assert.equal(posts(h).length, 2); assert.equal(posts(h)[1].files?.[0].name, "later.txt");
  h.pending[1].resolve(receipt(1)); await next; await h.flush();
});
test("batch partial success preserves only safe failed submitted files and later additions", async () => {
  const h = host(3); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  const added = new File(["new"], "new.txt"); h.render({ ...h.props(), pendingFiles: [...h.selected, added] }); await h.flush();
  h.pending[0].resolve({ accepted: 1, errors: [
    { filename: h.selected[1].name, file_index: 1, retry_safe: true, error: "rejected" },
    { filename: h.selected[2].name, file_index: 2, retry_safe: false, document_id: "already-created", error: "queue failed" },
  ] }); await first; await h.flush();
  assert.deepEqual(h.props().pendingFiles, [h.selected[1], added]); assert.equal(posts(h).length, 1);
  assert.ok(text(h.current()).includes("uploadKnownDocumentId"));
});
test("retired config completion cannot start an upload or clear a new owner", async () => {
  const h = host(); const old = deferred<unknown>(); h.setPut(() => old.promise); h.render(); await h.flush();
  const first = h.invoke(); await h.flush();
  const replacement = files(1); h.setPut(async () => ({ status: "success" }));
  h.render({ ...h.props(), datasetId: "dataset-b", pendingFiles: replacement }); await h.flush();
  const second = h.invoke(); await h.flush(); assert.equal(posts(h).length, 1); assert.ok(posts(h)[0].url.includes("dataset-b"));
  old.resolve({ status: "success" }); await first; await h.flush();
  assert.equal(posts(h).length, 1); assert.equal(h.props().uploading, true); assert.deepEqual(h.props().pendingFiles, replacement);
  h.pending[0].resolve(receipt(1)); await second; await h.flush();
});
test("old upload completion cannot clear new scope files, busy state or dialog", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  const replacement = files(1); h.auth.user.user_id = "owner-b";
  h.render({ ...h.props(), pendingFiles: replacement }); await h.flush(); const second = h.invoke(); await h.flush();
  h.pending[0].resolve(receipt(1)); await first; await h.flush();
  assert.equal(h.props().uploading, true); assert.equal(h.props().open, true); assert.deepEqual(h.props().pendingFiles, replacement);
  h.pending[1].resolve(receipt(1)); await second; await h.flush(); assert.equal(h.props().uploading, false);
});
test("unmount retires a pending config; A-B-A retires the earlier generation", async () => {
  for (const unmount of [true, false]) {
    const h = host(); const old = deferred<unknown>(); h.setPut(() => old.promise); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
    if (unmount) h.dispose();
    else { h.render({ ...h.props(), datasetId: "b" }); await h.flush(); h.render({ ...h.props(), datasetId: "dataset-a" }); await h.flush(); }
    old.resolve({ status: "success" }); await first; await h.flush(); assert.equal(posts(h).length, 0);
  }
});
test("retiring a two-file sequential upload prevents the second file POST", async () => {
  const h = host(2); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  h.render({ ...h.props(), datasetId: "b", pendingFiles: files(1) }); await h.flush();
  h.pending[0].resolve(receipt(1)); await first; await h.flush(); assert.equal(posts(h).length, 1);
});
test("original config/file snapshot is submitted despite later edits", async () => {
  const h = host(); const gate = deferred<unknown>(); h.setPut(() => gate.promise); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  const originalPayload = structuredClone(puts(h)[0].payload);
  const mode = nodes(h.current(), "div").find((node) => typeof node.props.onClick === "function" && text(node).includes("knowledge.detail.uploadChunkModes.paragraph"));
  assert.ok(mode, "actual paragraph mode control"); (mode.props.onClick as () => void)(); await h.flush();
  assert.equal((puts(h)[0].payload as { chunking_config: { mode: string } }).chunking_config.mode, "automatic");
  h.render({ ...h.props(), pendingFiles: files(3) }); await h.flush();
  gate.resolve({ status: "success" }); await h.flush(); assert.equal(posts(h).length, 1);
  assert.deepEqual(puts(h)[0].payload, originalPayload); assert.equal(posts(h)[0].files?.length, 1);
  assert.equal(await posts(h)[0].files?.[0].text(), await h.selected[0].text());
  h.pending[0].resolve(receipt(1)); await first; await h.flush(); assert.equal((h.props().pendingFiles as File[]).length, 3);
});

test("late invalidation settlement cannot close or clear the replacement owner's upload", async () => {
  const h = host(); const oldRead = deferred<void>(); h.setInvalidate(() => oldRead.promise); h.render(); await h.flush();
  const first = h.invoke(); await h.flush(); h.pending[0].resolve(receipt(1)); await h.flush();
  h.setInvalidate(async () => {}); const replacement = files(1);
  h.render({ ...h.props(), datasetId: "b", pendingFiles: replacement }); await h.flush();
  const second = h.invoke(); await h.flush(); oldRead.resolve(); await first; await h.flush();
  assert.equal(h.props().uploading, true); assert.equal(h.props().open, true); assert.deepEqual(h.props().pendingFiles, replacement);
  h.pending[1].resolve(receipt(1)); await second; await h.flush(); assert.equal(h.props().uploading, false);
});
test("explicitly removed submitted files are not restored by safe-failure settlement", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  h.render({ ...h.props(), pendingFiles: [] }); await h.flush();
  h.pending[0].reject({ response: { status: 422 } }); await first; await h.flush();
  assert.deepEqual(h.props().pendingFiles, []); assert.equal(h.props().uploading, false); assert.equal(posts(h).length, 1);
});


test("mixed upload outcomes survive dismissal and route remount without reselecting files", async () => {
  const h = host(3); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  h.pending[0].resolve({ accepted: 1, documents: [{ document_id: "accepted-doc" }], errors: [
    { filename: h.selected[1].name, file_index: 1, retry_safe: true, error: "private parser text is not persisted" },
    { filename: h.selected[2].name, file_index: 2, retry_safe: false, document_id: "created-doc", error: "queue failed" },
  ] }); await first; await h.flush();
  assert.deepEqual(h.recovery()[0]?.files.map(file => file.state), ["accepted", "rejected", "created"]);
  const dialog = nodes(h.current(), "Dialog")[0]; (dialog.props.onOpenChange as (open: boolean) => void)(false); await h.flush(); h.dispose();
  const restored = host(0, h.saved); restored.render({ ...restored.props(), open: false }); await restored.flush();
  assert.ok(nodes(restored.current(), "section").some(node => node.props["data-testid"] === "upload-recovery-receipts"));
  assert.ok(text(restored.current()).includes("uploadRecoveryState.accepted"));
  assert.ok(text(restored.current()).includes("created-doc"));
  assert.ok(text(restored.current()).includes("accepted-doc")); assert.equal(posts(restored).length, 0);
  const refresh = nodes(restored.current(), "Button").find(node => text(node).includes("uploadRecoveryRefresh")); assert.ok(refresh);
  (refresh.props.onClick as () => void)(); await restored.flush(); assert.equal(posts(restored).length, 0);
  assert.equal([...h.saved.values()].join("").includes("private parser text"), false);
});

test("navigation retains dispatched uncertainty and never-submitted sequential files", async () => {
  const h = host(2); h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  assert.deepEqual(h.recovery()[0]?.files.map(file => file.state), ["unknown", "not_submitted"]);
  h.dispose(); h.pending[0].resolve(receipt(1)); await first;
  const restored = host(0, h.saved); restored.render({ ...restored.props(), open: false }); await restored.flush();
  assert.deepEqual(restored.recovery()[0]?.files.map(file => file.state), ["unknown", "not_submitted"]);
  assert.equal(posts(h).length, 1); assert.equal(posts(restored).length, 0);
});

test("accepted upload survives a document refresh failure and is never retried", async () => {
  const h = host(); h.setInvalidate(async () => { throw Error("controlled read failure"); });
  h.render(); await h.flush(); const first = h.invoke(); await h.flush(); h.pending[0].resolve(receipt(1)); await first; await h.flush();
  assert.equal(h.recovery()[0]?.files[0].state, "accepted");
  assert.equal(h.recovery()[0]?.files[0].documentId, "doc-a"); assert.deepEqual(h.props().pendingFiles, []);
  assert.equal(posts(h).length, 1); assert.equal(text(h.current()).includes("uploadInspectBeforeRetry"), false);
});

test("storage failure before dispatch prevents both configuration and file mutation", async () => {
  const h = host(); h.failStorage(); h.render(); await h.flush(); await h.invoke(); await h.flush();
  assert.equal(h.calls.length, 0); assert.ok(text(h.current()).includes("uploadRecoveryUnavailable"));
});

test("same-user token ABA before config resolution cannot dispatch upload", async () => {
  const h = host(); const gate = deferred<unknown>(); h.setPut(() => gate.promise);
  h.render(); await h.flush(); const first = h.invoke(); await h.flush();
  h.changeAuth({ token: "synthetic-token-b" }); h.changeAuth({ token: "synthetic-token-a" });
  gate.resolve({ status: "success" }); await first; await h.flush(); assert.equal(posts(h).length, 0);
});

test("changing tenant or losing dataset editor access hides old upload metadata immediately", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush(); h.pending[0].resolve(receipt(1)); await first; await h.flush();
  for (const changed of [{ tenant_id: "tenant-b" }, { my_permission: "viewer" }]) {
    h.render({ ...h.props(), dataset: { ...(h.props().dataset as Data), ...changed } });
    assert.equal(nodes(h.current(), "section").length, 0);
  }
});


test("cached dataset identity cannot authorize a different route upload", async () => {
  const h = host(); h.render({ ...h.props(), dataset: { ...(h.props().dataset as Data), dataset_id: "wrong-dataset" } });
  await h.flush(); await h.invoke(); await h.flush(); assert.equal(h.calls.length, 0); assert.equal(h.saved.size, 0);
});


for (const change of ["token", "logout", "authority"] as const) {
  test(`rendered upload receipts and old callbacks retire across ${change} ABA`, async () => {
    const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush(); h.pending[0].resolve(receipt(1)); await first; await h.flush();
    const oldRefresh = nodes(h.current(), "Button").find(node => text(node).includes("uploadRecoveryRefresh"));
    const oldClear = nodes(h.current(), "Button").find(node => text(node).includes("uploadRecoveryClear"));
    assert.ok(oldRefresh); assert.ok(oldClear);
    const beforeReads = h.invalidations(); const beforeStorage = [...h.saved]; const previous = structuredClone(h.auth);
    if (change === "token") h.changeAuth({ token: "synthetic-token-b" });
    else if (change === "logout") h.changeAuth({ isAuthenticated: false, token: "" });
    else h.changeAuth({ user: { ...h.auth.user, roles: ["viewer"] } });
    // Global auth cleanup is covered with the actual store in the storage suite.
    h.saved.clear(); h.changeAuth(previous);
    (oldRefresh.props.onClick as () => void)(); (oldClear.props.onClick as () => void)();
    assert.equal(h.invalidations(), beforeReads); assert.equal(h.saved.size, 0);
    await h.flush(); assert.equal(nodes(h.current(), "section").length, 0);
    assert.ok(beforeStorage.length > 0); assert.equal(posts(h).length, 1);
  });
}


test("failed receipt settlement retains conservative disk outcome without replaying accepted upload", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush(); h.failStorage();
  h.pending[0].resolve(receipt(1)); await first; await h.flush();
  assert.equal(h.recovery()[0]?.files[0].state, "unknown");
  assert.ok(text(h.current()).includes("uploadRecoveryState.accepted"));
  assert.ok(text(h.current()).includes("uploadRecoveryUnavailable"));
  assert.deepEqual(h.props().pendingFiles, []); assert.equal(posts(h).length, 1);
});


test("late receipt refresh failure is silent after no-render authentication ABA", async () => {
  const h = host(); h.render(); await h.flush(); const first = h.invoke(); await h.flush(); h.pending[0].resolve(receipt(1)); await first; await h.flush();
  const gate = deferred<void>(); h.setInvalidate(() => gate.promise); const previousToasts = h.toasts.length;
  const refresh = nodes(h.current(), "Button").find(node => text(node).includes("uploadRecoveryRefresh")); assert.ok(refresh);
  (refresh.props.onClick as () => void)();
  h.changeAuth({ token: "synthetic-token-b" }); h.changeAuth({ token: "synthetic-token-a" });
  gate.reject(Error("controlled late read failure")); await h.flush();
  assert.equal(h.toasts.length, previousToasts); assert.equal(posts(h).length, 1);
});
