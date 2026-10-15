import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data; key?: string };
type Slot = { value: unknown; deps?: unknown[]; cleanup?: () => void };
type Frame = { slots: Slot[]; cursor: number };
const requestKey = "kb-create-dataset:owner-a";
const formKey = "kb-create-form:owner-a";
const file = (name: string) => new File(["synthetic source"], name, { type: "text/plain" });
const settle = () => new Promise<void>(resolve => setImmediate(resolve));
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function nodes(tree: unknown, type: string): Node[] {
  if (Array.isArray(tree)) return tree.flatMap(child => nodes(child, type));
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Node;
  return [...(node.type === type ? [node] : []), ...nodes(node.props?.children, type)];
}
function text(tree: unknown): string {
  if (typeof tree === "string") return tree;
  if (!tree || typeof tree !== "object") return "";
  return Object.values(tree).map(text).join(" ");
}

// Complete production wizard, authenticated scope, create model and API functions.
// Actual Zustand notifications; controlled React scheduling, storage and HTTP.
// Keyed child unmounts run cleanup. This is not DOM/browser/backend acceptance.
function host(storage = new Map<string, string>()) {
  const auth = createStore(() => ({ token: "session-a", isAuthenticated: true,
    user: { user_id: "owner-a", permissions: ["knowledge:dataset:create"], roles: ["owner"], tier: "normal" } }));
  const frames = new Map<string, Frame>();
  let current: Frame, dirty = true, tree: unknown, mounted = true;
  let effects: (() => void)[] = [], seen = new Set<string>();
  let storageUnavailable = false;
  const calls: { method: string; path: string; body?: unknown }[] = [];
  const navigation: string[] = [];
  const dataset = (id: string, name = "Source library") => ({ dataset_id: id, name, my_permission: "owner" });
  let read = async (id: string): Promise<unknown> => dataset(id);
  let create = async (body: Data): Promise<unknown> => dataset(body.dataset_id as string, body.name as string);
  let upload: (body: unknown, path: string) => Promise<unknown> = async () => ({ document_id: "doc-accepted" });
  const same = (a: unknown[] | undefined, b: unknown[]) => a?.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const state = (initial: unknown) => {
    const frame = current, i = frame.cursor++;
    frame.slots[i] ??= { value: typeof initial === "function" ? initial() : initial };
    return [frame.slots[i].value, (next: unknown) => {
      const value = typeof next === "function" ? next(frame.slots[i].value) : next;
      if (!Object.is(value, frame.slots[i].value)) { frame.slots[i].value = value; dirty = true; }
    }];
  };
  const memo = (fn: () => unknown, deps: unknown[]) => {
    const i = current.cursor++; if (!current.slots[i] || !same(current.slots[i].deps, deps)) current.slots[i] = { value: fn(), deps };
    return current.slots[i].value;
  };
  const effect = (fn: () => void | (() => void), deps: unknown[]) => {
    const frame = current, i = frame.cursor++;
    if (frame.slots[i] && same(frame.slots[i].deps, deps)) return;
    const previous = frame.slots[i]; frame.slots[i] = { value: null, deps };
    effects.push(() => { previous?.cleanup?.(); const cleanup = fn(); if (cleanup) frame.slots[i].cleanup = cleanup; });
  };
  const react = { useState: state, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps),
    useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect };
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data, key?: string) => ({ type, props, key });
  const authHook = Object.assign((fn: (value: ReturnType<typeof auth.getState>) => unknown) => fn(auth.getState()), {
    getState: auth.getState, subscribe: auth.subscribe,
  });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-router-dom": { useNavigate: () => (path: string) => navigation.push(path) },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    antd: { message: { warning() {}, error() {} } }, "lucide-react": named,
    "@/components/ui/button": named, "@/components/ui/alert-dialog": named,
    "@/store/useAuthStore": { useAuthStore: authHook },
    "@/lib/utils": { generateUUID: randomUUID }, "@/lib/sse": {},
    "@/lib/api": { api: {
      async get(path: string) { calls.push({ method: "GET", path }); return { data: await read(path.split("/").at(-1)!) }; },
      async post(path: string, body: unknown) {
        calls.push({ method: "POST", path, body });
        return { data: await (path === "/api/v1/knowledge/datasets" ? create(body as Data) : upload(body, path)) };
      },
    } },
  };
  for (const name of ["DatasetCreateBasicStep", "DatasetCreateIndexStep", "DatasetCreateSourcesStep", "SourceUploadFailureAlert"]) {
    modules[`@/pages/knowledge/create/${name}`] = { [name]: name };
  }
  function load(path: string) {
    const exports: Data = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: {
      jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS,
    } }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
      crypto: { randomUUID }, FormData, File, console: { error() {} },
      sessionStorage: {
        getItem: (key: string) => { if (storageUnavailable) throw Error("synthetic storage failure"); return storage.get(key) ?? null; },
        setItem: (key: string, value: string) => { if (storageUnavailable) throw Error("synthetic storage failure"); storage.set(key, value); },
        removeItem: (key: string) => { if (storageUnavailable) throw Error("synthetic storage failure"); storage.delete(key); },
      }, window: { location: { reload() {} } },
    });
    return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = load("../../../lib/sdkAuthorityScope.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = load("../../../hooks/useAuthenticatedReadScope.ts");
  modules["@/types/knowledge"] = load("../../../types/knowledge.ts");
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["@/pages/knowledge/create/datasetCreateModel"] = load("./datasetCreateModel.ts");
  const Page = load("../DatasetCreate.tsx").default as () => unknown;
  const cleanup = (frame: Frame) => frame.slots.forEach(slot => slot.cleanup?.());
  function expand(value: unknown, path: string): unknown {
    if (Array.isArray(value)) return value.map((child, i) => expand(child, `${path}/${i}`));
    if (!value || typeof value !== "object") return value;
    const node = value as Node;
    if (typeof node.type === "function") {
      const id = `${path}/${node.key ?? node.type.name}`; seen.add(id);
      if (!frames.has(id)) frames.set(id, { slots: [], cursor: 0 });
      const previous = current; current = frames.get(id)!; current.cursor = 0;
      const result = node.type(node.props); current = previous;
      return expand(result, id);
    }
    return { ...node, props: { ...node.props, children: expand(node.props?.children, path + "/children") } };
  }
  function render() {
    assert.equal(mounted, true); dirty = false; seen = new Set();
    tree = expand({ type: Page, props: {} }, "root");
    for (const [id, frame] of frames) if (!seen.has(id)) { cleanup(frame); frames.delete(id); }
    const pending = effects; effects = []; pending.forEach(fn => fn());
  }
  async function flush() { for (let i = 0; i < 6; i++) { await settle(); if (mounted && dirty) render(); } }
  function component(name: string) { const node = nodes(tree, name)[0]; assert.ok(node, name); return node; }
  function button(key: string) {
    const node = nodes(tree, "Button").find(n => text(n.props.children).includes(`knowledge.create.${key}`)); assert.ok(node, key); return node;
  }
  function invoke(key: string) { const node = button(key); assert.equal(Boolean(node.props.disabled), false, key); return (node.props.onClick as () => Promise<void> | void)(); }
  async function prepare(names: string[] = ["first.txt", "second.txt"], withUrl = true) {
    render(); await flush(); (component("DatasetCreateBasicStep").props.onNameChange as (v: string) => void)("Source library");
    render(); invoke("next"); render();
    (component("DatasetCreateSourcesStep").props.onFilesSelect as (files: File[]) => void)(names.map(file)); render();
    if (withUrl) {
      (component("DatasetCreateSourcesStep").props.onUrlInputChange as (v: string) => void)("https://example.com/source"); render();
      (component("DatasetCreateSourcesStep").props.onAddUrl as () => void)(); render();
    }
    invoke("next"); render(); await flush();
  }
  return { storage, calls, navigation, auth, render, flush, component, button, invoke, prepare, dataset,
    current: () => tree, setRead: (fn: typeof read) => { read = fn; }, setCreate: (fn: typeof create) => { create = fn; },
    setUpload: (fn: typeof upload) => { upload = fn; }, breakStorage: () => { storageUnavailable = true; },
    dispose: () => { mounted = false; frames.forEach(cleanup); frames.clear(); },
  };
}
const creates = (h: ReturnType<typeof host>) => h.calls.filter(call => call.method === "POST" && call.path.endsWith("/datasets"));
const uploads = (h: ReturnType<typeof host>) => h.calls.filter(call => call.method === "POST" && !call.path.endsWith("/datasets"));

test("Cancel during create retains identity and prevents late source dispatch or navigation", async () => {
  const h = host(), pending = deferred<unknown>(); try {
    await h.prepare(); h.setCreate(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
    const id = h.storage.get(requestKey)!; assert.ok(id); h.invoke("cancel");
    pending.resolve(h.dataset(id)); await submit; await h.flush();
    assert.equal(uploads(h).length, 0); assert.deepEqual(h.navigation, ["/knowledge"]); assert.equal(h.storage.get(requestKey), id);
  } finally { h.dispose(); }
});
test("unmount during first upload prevents queued files and URLs and keeps recovery identity", async () => {
  const h = host(), pending = deferred<unknown>();
  await h.prepare(); h.setUpload(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
  const id = h.storage.get(requestKey); assert.equal(uploads(h).length, 1); h.dispose();
  pending.resolve({ document_id: "accepted-before-leaving" }); await submit;
  assert.equal(uploads(h).length, 1); assert.equal(h.navigation.length, 0); assert.equal(h.storage.get(requestKey), id);
});
test("auth A-B-A before rerender cannot resume a retired create submission", async () => {
  const h = host(), pending = deferred<unknown>(); try {
    await h.prepare(); h.setCreate(() => pending.promise); const submit = h.invoke("confirm");
    const id = h.storage.get(requestKey)!; h.auth.setState({ token: "session-b" }); h.auth.setState({ token: "session-a" });
    pending.resolve(h.dataset(id)); await submit;
    assert.equal(uploads(h).length, 0); assert.equal(h.navigation.length, 0); assert.equal(h.storage.get(requestKey), id);
    h.render(); await h.flush(); assert.ok(h.button("openExistingDraft")); assert.equal(creates(h).length, 1);
  } finally { h.dispose(); }
});
for (const boundary of ["account", "token", "permissions", "logout"] as const) {
  test(`${boundary} change during upload stops remaining dispatch and stale cleanup`, async () => {
    const h = host(), pending = deferred<unknown>(); try {
      await h.prepare(); h.setUpload(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
      const id = h.storage.get(requestKey), state = h.auth.getState();
      if (boundary === "account") h.auth.setState({ user: { ...state.user, user_id: "owner-b" } });
      if (boundary === "token") h.auth.setState({ token: "session-b" });
      if (boundary === "permissions") h.auth.setState({ user: { ...state.user, permissions: [] } });
      if (boundary === "logout") h.auth.setState({ isAuthenticated: false });
      pending.reject(new Error("late upload failure")); await submit;
      assert.equal(uploads(h).length, 1); assert.equal(h.navigation.length, 0); assert.equal(h.storage.get(requestKey), id);
    } finally { h.dispose(); }
  });
}
test("replacement account has a fresh form and cannot submit the previous account's callback", async () => {
  const h = host(); try {
    await h.prepare(); const oldSubmit = h.button("confirm").props.onClick as () => Promise<void>;
    h.auth.setState({ token: "session-b", user: { ...h.auth.getState().user, user_id: "owner-b" } }); h.render(); await h.flush();
    assert.equal(h.component("DatasetCreateBasicStep").props.name, ""); await oldSubmit;
    assert.equal(h.calls.length, 0); assert.ok(h.storage.get(formKey)); assert.equal(h.storage.has("kb-create-dataset:owner-b"), false);
  } finally { h.dispose(); }
});
test("known create identity is recovered by read only after interrupted upload remount", async () => {
  const storage = new Map<string, string>(), h = host(storage), pending = deferred<unknown>();
  await h.prepare(); h.setUpload(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
  const id = storage.get(requestKey)!; h.dispose(); pending.resolve({ document_id: "accepted" }); await submit;
  const next = host(storage); try {
    next.render(); await next.flush(); assert.ok(next.button("openExistingDraft"));
    assert.equal(creates(next).length, 0); assert.equal(uploads(next).length, 0); assert.equal(storage.get(requestKey), id);
    assert.equal(next.calls[0].path, `/api/v1/knowledge/datasets/${id}`);
  } finally { next.dispose(); }
});
test("successful active submission sends each source once and clears request identity", async () => {
  const h = host(); try {
    await h.prepare(); const submit = h.button("confirm").props.onClick as () => Promise<void>;
    const first = submit(), second = submit(); await Promise.all([first, second]); await h.flush();
    assert.equal(creates(h).length, 1); assert.equal(uploads(h).length, 3);
    assert.equal(h.storage.has(requestKey), false); assert.equal(h.storage.has(formKey), false);
    assert.equal(h.navigation.length, 1); assert.match(h.navigation[0], /^\/knowledge\/kb_/);
  } finally { h.dispose(); }
});
test("safe source rejection retries only the failed source without recreating the dataset", async () => {
  const h = host(); let failures = 0; try {
    await h.prepare(); h.setUpload(async body => {
      if (body instanceof FormData && (body.get("file") as File).name === "second.txt" && failures++ === 0) throw { response: { status: 422 } };
      return { document_id: "accepted" };
    });
    await h.invoke("confirm"); await h.flush(); assert.equal(h.component("SourceUploadFailureAlert").props.outcomeUnknown, false);
    await h.invoke("retryFailedSources"); assert.equal(creates(h).length, 1); assert.equal(uploads(h).length, 4);
    assert.equal(h.storage.has(requestKey), false); assert.equal(h.navigation.length, 1);
  } finally { h.dispose(); }
});
test("unknown source result is not automatically replayed by retry", async () => {
  const h = host(); try {
    await h.prepare(["first.txt"], false); h.setUpload(async () => { throw Error("response lost"); });
    await h.invoke("confirm"); await h.flush(); assert.equal(h.component("SourceUploadFailureAlert").props.outcomeUnknown, true);
    await h.invoke("retryFailedSources"); await h.flush(); assert.equal(creates(h).length, 1); assert.equal(uploads(h).length, 1);
    assert.ok(h.storage.get(requestKey)); assert.equal(h.navigation.length, 0);
  } finally { h.dispose(); }
});
test("storage failure before create sends no request", async () => {
  const h = host(); try {
    await h.prepare(); h.breakStorage(); await h.invoke("confirm"); await h.flush();
    assert.equal(h.calls.length, 0); assert.equal(h.component("SourceUploadFailureAlert").props.error, "knowledge.create.draftStorageUnavailable");
  } finally { h.dispose(); }
});
test("retired initial identity lookup cannot reveal an older account's dataset", async () => {
  const storage = new Map([[requestKey, "kb_existing"]]), h = host(storage), pending = deferred<unknown>(); try {
    h.setRead(() => pending.promise); h.render(); await h.flush();
    h.auth.setState({ token: "session-b", user: { ...h.auth.getState().user, user_id: "owner-b" } }); h.render(); await h.flush();
    pending.resolve(h.dataset("kb_existing", "Previous owner private name")); await h.flush();
    assert.equal(text(h.current()).includes("Previous owner private name"), false); assert.equal(h.component("DatasetCreateBasicStep").props.name, "");
    assert.equal(h.navigation.length, 0); assert.equal(creates(h).length, 0);
  } finally { h.dispose(); }
});

for (const action of ["open", "discard"] as const) {
  test(`retired recovered-dataset ${action} callback cannot navigate or clear the owner's identity`, async () => {
    const storage = new Map([[requestKey, "kb_existing"]]), h = host(storage); try {
      h.render(); await h.flush();
      const callback = (action === "open" ? h.button("openExistingDraft") : nodes(h.current(), "AlertDialogAction")[0]).props.onClick as () => void;
      h.auth.setState({ token: "session-b", user: { ...h.auth.getState().user, user_id: "owner-b" } });
      callback(); assert.equal(h.navigation.length, 0); assert.equal(storage.get(requestKey), "kb_existing");
    } finally { h.dispose(); }
  });
}

test("lost create response recovers the same identity by explicit GET without another write", async () => {
  const storage = new Map<string, string>(), h = host(storage);
  try {
    await h.prepare(); h.setCreate(async () => { throw Error("response lost after acceptance"); });
    await h.invoke("confirm"); await h.flush(); const id = storage.get(requestKey)!;
    assert.ok(id); assert.equal(h.component("SourceUploadFailureAlert").props.outcomeUnknown, true);
    await h.invoke("confirm"); await h.flush(); assert.ok(h.button("openExistingDraft"));
    assert.equal(creates(h).length, 1); assert.equal(uploads(h).length, 0);
    assert.equal(h.calls.at(-1)?.path, `/api/v1/knowledge/datasets/${id}`);
    h.dispose(); const next = host(storage);
    try {
      next.render(); await next.flush(); assert.ok(next.button("openExistingDraft"));
      assert.equal(creates(next).length, 0); assert.equal(uploads(next).length, 0);
      assert.equal(next.calls[0].path, `/api/v1/knowledge/datasets/${id}`);
    } finally { next.dispose(); }
  } finally { h.dispose(); }
});
test("Cancel during existing identity lookup prevents a late 404 from creating again", async () => {
  const storage = new Map([[requestKey, "kb_pending"]]), h = host(storage), pending = deferred<unknown>();
  try {
    h.setRead(async () => { throw { response: { status: 404 } }; }); await h.prepare();
    h.setRead(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
    h.invoke("cancel"); pending.reject({ response: { status: 404 } }); await submit;
    assert.equal(creates(h).length, 0); assert.equal(uploads(h).length, 0);
    assert.deepEqual(h.navigation, ["/knowledge"]); assert.equal(storage.get(requestKey), "kb_pending");
  } finally { h.dispose(); }
});
for (const boundary of ["cancel", "unmount"] as const) {
  for (const outcome of ["resolve", "reject"] as const) {
    test(`${boundary} while awaiting URL ${outcome} never sends the remaining URL`, async () => {
      const h = host(), pending = deferred<unknown>();
      try {
        await h.prepare([], true); h.invoke("previous"); h.render();
        (h.component("DatasetCreateSourcesStep").props.onUrlInputChange as (value: string) => void)("https://example.com/second"); h.render();
        (h.component("DatasetCreateSourcesStep").props.onAddUrl as () => void)(); h.render(); h.invoke("next"); h.render();
        h.setUpload(() => pending.promise); const submit = h.invoke("confirm"); await h.flush();
        const id = h.storage.get(requestKey); assert.equal(uploads(h).length, 1); assert.ok(uploads(h)[0].path.endsWith("/url"));
        if (boundary === "cancel") h.invoke("cancel"); else h.dispose();
        if (outcome === "resolve") pending.resolve({ document_id: "accepted-url" }); else pending.reject(Error("late URL failure"));
        await submit; assert.equal(uploads(h).length, 1); assert.equal(h.storage.get(requestKey), id);
        assert.deepEqual(h.navigation, boundary === "cancel" ? ["/knowledge"] : []);
      } finally { h.dispose(); }
    });
  }
}
test("disposed wizard and auth ABA cannot overwrite a remounted owner's replacement recovery draft", async () => {
  const storage = new Map<string, string>(), old = host(storage), pending = deferred<unknown>();
  await old.prepare(); old.setCreate(() => pending.promise); const submit = old.invoke("confirm");
  const oldId = storage.get(requestKey)!; old.dispose();
  old.auth.setState({ token: "session-b" }); old.auth.setState({ token: "session-a" });
  storage.set(requestKey, "kb_replacement");
  const replacement = { ...JSON.parse(storage.get(formKey)!), name: "Replacement private draft" };
  storage.set(formKey, JSON.stringify(replacement)); const next = host(storage);
  try {
    next.render(); await next.flush(); const saved = storage.get(formKey);
    pending.resolve(old.dataset(oldId)); await submit; await next.flush();
    assert.equal(uploads(old).length, 0); assert.equal(old.navigation.length, 0);
    assert.equal(storage.get(requestKey), "kb_replacement"); assert.equal(storage.get(formKey), saved);
    assert.ok(next.button("openExistingDraft")); assert.equal(creates(next).length, 0); assert.equal(next.navigation.length, 0);
  } finally { next.dispose(); }
});
