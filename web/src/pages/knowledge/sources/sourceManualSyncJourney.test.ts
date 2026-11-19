import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
const id = "4fd4c558-18f1-4b33-9314-675cf4907a01";
const nextId = "4fd4c558-18f1-4b33-9314-675cf4907a02";
const binding = { binding_id: "binding-a", manual_sync_available: true };
function receipt(status = "pending", taskId = id, extra = {}) {
  return { task_id: taskId, binding_id: binding.binding_id, status, progress: 0, total_items: 3, processed_items: 0,
    synced_items: null, failed_items: null, error_count: null, error_summary: null, started_at: null, completed_at: null, ...extra };
}
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  return [...((value as Node).type === type ? [value as Node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  return value && typeof value === "object" ? Object.values(value).map(text).join(" ") : "";
}
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setImmediate(resolve)); };

// Actual production component, query options, API and TanStack observer. React hosts,
// session storage and HTTP are controlled, not browser/provider/database acceptance.
function host({ stored, storageFails = false, capability = true, currentTask }: { stored?: string; storageFails?: boolean; capability?: boolean; currentTask?: unknown } = {}) {
  const calls: Array<{ method: string; url: string; body?: unknown }> = [];
  const storage = new Map<string, string>();
  const key = 'kb-source-sync:["user-a","dataset-a","binding-a"]';
  if (stored) storage.set(key, stored);
  let response: unknown = receipt(), postResponse: unknown = receipt();
  let postWait: Promise<void> | undefined, release: (() => void) | undefined;
  let cursor = 0, uuidIndex = stored ? 1 : 0;
  const state: unknown[] = [];
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity, retry: false } } });
  let observer: QueryObserver | undefined, unsubscribe: (() => void) | undefined;
  let queryOptions: Data;
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const named = new Proxy({}, { get: (_target, name) => String(name) });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = typeof initial === "function" ? initial() : initial;
        return [state[slot], (value: unknown) => { state[slot] = typeof value === "function" ? value(state[slot]) : value; }]; },
      useRef(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = { current: initial }; return state[slot]; },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "@/components/ui/button": { Button: "Button" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@tanstack/react-query": { useQueryClient: () => client, useQuery(options: Data) {
      queryOptions = options;
      if (!observer) { observer = new QueryObserver(client, options as never); unsubscribe = observer.subscribe(() => {}); }
      else observer.setOptions(options as never);
      return { ...observer.getCurrentResult(), refetch: observer.refetch.bind(observer) };
    } },
    "@/lib/api": { api: {
      async post(url: string, body: unknown) { calls.push({ method: "POST", url, body }); assert.ok(storage.get(key), "Persist request identity before POST");
        await postWait; if (postResponse instanceof Error) throw postResponse; return { data: postResponse }; },
      async get(url: string) { calls.push({ method: "GET", url }); if (response instanceof Error) throw response; return { data: response }; },
    } },
  };
  function load(path: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => modules[name] ?? named,
      crypto: { randomUUID: () => [id, nextId][uuidIndex++] },
      sessionStorage: { getItem: (key: string) => { if (storageFails) throw Error("storage blocked"); return storage.get(key) ?? null; },
        removeItem: (key: string) => { if (storageFails) throw Error("storage blocked"); storage.delete(key); },
        setItem: (key: string, value: string) => { if (storageFails) throw Error("storage blocked"); storage.set(key, value); } },
    }); return exports;
  }
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./sourceSyncQuery"] = load("./sourceSyncQuery.ts");
  const { SourceManualSyncSession } = load("./SourceManualSync.tsx") as { SourceManualSyncSession: (props: Data) => unknown };
  function tree() { cursor = 0; return SourceManualSyncSession({ userId: "user-a", datasetId: "dataset-a", disabled: false,
    binding: { ...binding, manual_sync_available: capability, current_sync_task: currentTask } }); }
  function button(label: string) { return nodes(tree(), "Button").find(n => text(n).includes(`knowledge.sources.manual.${label}`)); }
  function click(label: string) { const n = button(label); assert.ok(n, `Missing ${label}`); assert.equal(n.props.disabled, false); (n.props.onClick as () => void)(); }
  return { calls, tree, button, click, storage, options: () => queryOptions,
    setStorageFails(value: boolean) { storageFails = value; },
    setRead(value: unknown) { response = value; }, setPost(value: unknown) { postResponse = value; },
    defer() { postWait = new Promise<void>(resolve => { release = resolve; }); },
    release() { release?.(); }, close() { release?.(); unsubscribe?.(); client.clear(); },
  };
}

test("eligible existing binding requires explicit click, persists UUID and follows exact receipt", async () => {
  const h = host(); try {
    h.tree(); await flush(); assert.equal(h.calls.length, 0);
    h.click("start"); await flush(); h.tree(); await flush();
    assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
    assert.equal(JSON.stringify(h.calls[0]), JSON.stringify({ method: "POST", url: "/api/v1/knowledge/dataset-a/sources/confluence/binding-a/sync", body: { request_id: id } }));
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.status.pending")); assert.equal(h.button("start")?.props.disabled, true);
    h.setRead(receipt("processing", id, { processed_items: 2 })); h.click("recheck"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.status.processing"));
    h.setRead(receipt("completed", id, { synced_items: 3 })); h.click("recheck"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.status.completed")); assert.equal(h.button("start")?.props.disabled, false);
    assert.ok(h.calls.filter(c => c.method === "GET").every(c => c.url.endsWith(`/sync/${id}`)));
  } finally { h.close(); }
});

test("repeated synchronous clicks cannot duplicate POST while response is pending", async () => {
  const h = host(); try { h.defer(); const n = h.button("start")!; (n.props.onClick as () => void)(); (n.props.onClick as () => void)();
    assert.equal(h.calls.length, 1); assert.equal(h.button("start")?.props.disabled, true); h.release(); await flush();
  } finally { h.close(); }
});

test("uncertain POST and GET404 retain identity, only explicit reads and never automatic replay", async () => {
  const h = host(); try {
    h.setPost(Error("response lost")); h.click("start"); await flush(); h.tree(); await flush();
    assert.equal(h.calls.length, 1); assert.ok(text(h.tree()).includes("knowledge.sources.manual.uncertain"));
    h.setRead(Object.assign(Error("not observed"), { response: { status: 404 } })); h.click("recheck"); await flush();
    assert.equal(h.button("start")?.props.disabled, true); assert.ok(text(h.tree()).includes("knowledge.sources.manual.readFailed"));
    h.setRead(receipt("processing")); h.click("recheck"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.status.processing"));
    assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
  } finally { h.close(); }
});

for (const status of [403, 404, 500]) test(`failed read ${status} hides retained success and prevents new sync`, async () => {
  const h = host({ stored: id }); try {
    h.tree(); await flush(); assert.equal(h.calls.length, 0);
    h.setRead(receipt("completed", id, { synced_items: 3 })); h.click("recheck"); await flush(); h.tree();
    h.setRead(Object.assign(Error("failed"), { response: { status } })); h.click("recheck"); await flush();
    assert.ok(!text(h.tree()).includes("knowledge.sources.manual.status.completed")); assert.equal(h.button("start")?.props.disabled, true);
    assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
  } finally { h.close(); }
});

test("partial failure reports applied updates, explicit retry uses a new request ID", async () => {
  const h = host({ stored: id }); try {
    h.setRead(receipt("failed", id, { synced_items: 2, failed_items: 1, error_count: 1 })); h.click("recheck"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.partialFailure"));
    h.setPost(receipt("pending", nextId)); h.click("retry"); await flush();
    assert.equal(JSON.stringify(h.calls.filter(c => c.method === "POST").map(c => c.body)), JSON.stringify([{ request_id: nextId }]));
  } finally { h.close(); }
});

test("processed attempts alone never imply partial successful updates", async () => {
  const h = host({ stored: id }); try { h.setRead(receipt("failed", id, { processed_items: 3, synced_items: 0 }));
    h.click("recheck"); await flush(); assert.ok(!text(h.tree()).includes("knowledge.sources.manual.partialFailure"));
  } finally { h.close(); }
});

for (const opts of [{ capability: false }, { storageFails: true }, { stored: "malformed" }]) test(`fail closed: ${JSON.stringify(opts)}`, async () => {
  const h = host(opts); try { h.tree(); await flush(); const n = h.button("start"); assert.ok(!n || n.props.disabled); assert.equal(h.calls.length, 0); } finally { h.close(); }
});

test("server current task is read-only recovery when no local receipt is present", async () => {
  const h = host({ currentTask: receipt(), capability: false }); try { h.tree(); await flush(); assert.equal(h.calls.length, 0); assert.ok(!h.button("start"));
    h.click("recheck"); await flush(); assert.ok(h.calls[0].url.endsWith(`/sync/${id}`));
  } finally { h.close(); }
});

test("mismatched returned task cannot unlock retry or expose another binding result", async () => {
  const h = host({ stored: id }); try { h.setRead(receipt("completed", nextId)); h.click("recheck"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.readFailed")); assert.equal(h.button("start")?.props.disabled, true);
  } finally { h.close(); }
});


test("write storage failure after initial render prevents POST", async () => {
  const h = host(); try { h.tree(); h.setStorageFails(true); h.click("start"); await flush();
    assert.equal(h.calls.length, 0); assert.ok(text(h.tree()).includes("knowledge.sources.manual.storageFailed"));
  } finally { h.close(); }
});

test("query polling is GET-only and stops at terminal status or any read error", () => {
  const h = host(); try { h.tree(); const interval = h.options().refetchInterval as (q: unknown) => number | false;
    for (const status of ["pending", "processing"]) assert.equal(interval({ state: { data: receipt(status), error: null } }), 2000);
    for (const status of ["completed", "failed"]) assert.equal(interval({ state: { data: receipt(status), error: null } }), false);
    assert.equal(interval({ state: { data: receipt("processing"), error: Error("forbidden") } }), false);
    assert.equal(h.options().retry, false); assert.equal(h.options().refetchOnWindowFocus, false); assert.equal(h.options().refetchOnReconnect, false);
    assert.equal(h.calls.length, 0);
  } finally { h.close(); }
});


test("explicit service pre-admission rejection releases guard without automatic retry", async () => {
  const h = host(); try { h.setPost(Object.assign(Error("rejected"), { response: { status: 403,
    data: { detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false } } } }));
    h.click("start"); await flush(); assert.ok(text(h.tree()).includes("knowledge.sources.manual.rejected"));
    assert.equal(h.button("start")?.props.disabled, false); assert.equal(h.calls.length, 1); assert.equal(h.storage.size, 0);
    h.setPost(receipt("pending", nextId)); h.click("start"); await flush();
    assert.equal(h.calls.filter(c => c.method === "POST").length, 2);
  } finally { h.close(); }
});

test("generic POST403 does not pretend service proved rejection", async () => {
  const h = host(); try { h.setPost(Object.assign(Error("proxy error"), { response: { status: 403 } }));
    h.click("start"); await flush(); assert.ok(text(h.tree()).includes("knowledge.sources.manual.uncertain"));
    assert.equal(h.button("start")?.props.disabled, true); assert.equal(h.storage.size, 1);
  } finally { h.close(); }
});


test("production Sources tab connects dataset identity and eligibility to binding controls", () => {
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en-US" } }) },
  };
  const presentation: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./sourcePresentation.ts", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, { exports: presentation, require: () => named });
  modules["./sourcePresentation"] = presentation;
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./SourcesTab.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: (name: string) => modules[name] ?? named });
  const tree = (exports.SourcesTab as (props: Data) => unknown)({ datasetId: "dataset-a", sourcesLoading: true, sourcesFailed: false,
    sources: { connector_status: "available", file_uploads: { count: 0 }, url_imports: { count: 0 }, confluence_bindings: [binding], total_documents: 0 },
  });
  const control = nodes(tree, "SourceManualSync")[0]; assert.ok(control);
  assert.equal(control.props.datasetId, "dataset-a"); assert.equal(control.props.binding, binding); assert.equal(control.props.disabled, true);
  for (const connectorStatus of ["unavailable", "not_configured", "unknown", "available"]) {
    for (const sourcesFailed of [true, false]) {
      const hiddenTree = (exports.SourcesTab as (props: Data) => unknown)({ datasetId: "dataset-a", sourcesLoading: false, sourcesFailed,
        sources: { connector_status: connectorStatus, file_uploads: { count: 0 }, url_imports: { count: 0 }, confluence_bindings: [binding], total_documents: 0 },
      });
      assert.equal(nodes(hiddenTree, "SourceManualSync").length, connectorStatus === "available" && !sourcesFailed ? 1 : 0);
    }
  }
});


test("stale start callback cannot dispatch another UUID after acceptance before rerender", async () => {
  const h = host(); try { const click = h.button("start")!.props.onClick as () => void;
    click(); await flush(); click(); await flush(); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
  } finally { h.close(); }
});


test("production wrapper scopes remount identity to user, dataset and binding", () => {
  let userId: string | undefined = "user-a";
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const jsx = (type: unknown, props: Data, key: unknown) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/store/useAuthStore": { useAuthStore: (select: (state: unknown) => unknown) => select({ user: userId ? { user_id: userId } : null }) },
  };
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./SourceManualSync.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: (name: string) => modules[name] ?? named });
  const render = exports.SourceManualSync as (props: Data) => { key: string } | null;
  const props = { datasetId: "dataset-a", binding, disabled: false };
  const original = render(props)!.key;
  assert.notEqual(render({ ...props, datasetId: "dataset-b" })!.key, original);
  assert.notEqual(render({ ...props, binding: { ...binding, binding_id: "binding-b" } })!.key, original);
  userId = "user-b"; assert.notEqual(render(props)!.key, original);
  userId = undefined; assert.equal(render(props), null);
});

test("backend-generated receipt fixture flows through production API, query and controls", async () => {
  const fixtures = JSON.parse(readFileSync(new URL("./sourceSyncContract.fixture.json", import.meta.url), "utf8"));
  const h = host(); try {
    h.setPost(fixtures.pending); h.click("start"); await flush();
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.status.pending"));
    for (const key of ["processing", "completed", "failed_partial"]) {
      const fixture = fixtures[key];
      assert.equal(fixture.task_id, id); assert.equal(fixture.binding_id, binding.binding_id);
      h.setRead(fixture); h.click("recheck"); await flush();
      assert.ok(text(h.tree()).includes(`knowledge.sources.manual.status.${fixture.status}`));
      if (key === "failed_partial") {
        assert.ok(text(h.tree()).includes("knowledge.sources.manual.partialFailure"));
        assert.ok(text(h.tree()).includes("knowledge.sources.manual.failedItems"));
        assert.ok(text(h.tree()).includes("knowledge.sources.manual.errors"));
        assert.equal(h.button("retry")?.props.disabled, false);
      }
    }
    assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
    assert.ok(h.calls.filter(c => c.method === "GET").every(c => c.url.endsWith(`/sync/${id}`)));
  } finally { h.close(); }
});


for (const explicit of [false, true]) test(`unavailable POST503 respects explicit service admission proof: ${explicit}`, async () => {
  const h = host(); try { h.setPost(Object.assign(Error("unavailable"), { response: { status: 503,
    data: explicit ? { detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false } } : {} } }));
    h.click("start"); await flush();
    assert.equal(h.button("start")?.props.disabled, !explicit);
    assert.equal(h.storage.size, explicit ? 0 : 1); assert.equal(h.calls.length, 1);
  } finally { h.close(); }
});

test("GET service NOT_ACCEPTED503 never clears an earlier ambiguous submission", async () => {
  const h = host({ stored: id }); try {
    h.setRead(Object.assign(Error("currently unavailable"), { response: { status: 503,
      data: { detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false } } } }));
    h.click("recheck"); await flush(); assert.equal(h.button("start")?.props.disabled, true);
    assert.equal(h.storage.size, 1); assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
  } finally { h.close(); }
});


test("late rejection cannot remove or overwrite a newer mounted producer receipt", async () => {
  const h = host(); try {
    h.defer(); h.setPost(Object.assign(Error("not accepted"), { response: { status: 503,
      data: { detail: { code: "CONFLUENCE_SYNC_NOT_ACCEPTED", accepted: false } } } }));
    h.click("start"); const key = [...h.storage.keys()][0]; h.storage.set(key, nextId);
    h.release(); await flush(); assert.equal(h.storage.get(key), nextId);
    assert.equal(h.button("start")?.props.disabled, true); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
  } finally { h.close(); }
});

test("a stale successful producer cannot replace a newer stored receipt on another sync", async () => {
  const h = host(); try {
    h.setPost(receipt("completed")); h.click("start"); await flush(); h.tree();
    const key = [...h.storage.keys()][0]; h.storage.set(key, nextId);
    h.click("start"); await flush(); assert.equal(h.storage.get(key), nextId);
    assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
    assert.ok(text(h.tree()).includes("knowledge.sources.manual.storageFailed"));
  } finally { h.close(); }
});

const backendErrors = JSON.parse(readFileSync(new URL("./sourceSyncErrors.fixture.json", import.meta.url), "utf8"));
for (const key of ["not_accepted_403", "not_accepted_409", "not_accepted_503", "unknown_post_503"]) {
  test(`actual backend ASGI rejection fixture: ${key}`, async () => {
    const h = host(); try {
      const fixture = backendErrors[key];
      h.setPost(Object.assign(Error("controlled backend rejection"), { response: { status: fixture.status, data: fixture.body } }));
      h.click("start"); await flush();
      const rejected = key.startsWith("not_accepted_");
      assert.equal(h.button("start")?.props.disabled, !rejected);
      assert.equal(h.storage.size, rejected ? 0 : 1);
      assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
    } finally { h.close(); }
  });
}
for (const key of ["unknown_get_404", "unavailable_get_503"]) {
  test(`actual backend ASGI read fixture retains historical guard: ${key}`, async () => {
    const h = host({ stored: id }); try {
      const fixture = backendErrors[key];
      h.setRead(Object.assign(Error("controlled backend read failure"), { response: { status: fixture.status, data: fixture.body } }));
      h.click("recheck"); await flush(); assert.equal(h.button("start")?.props.disabled, true);
      assert.equal(h.storage.size, 1); assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
    } finally { h.close(); }
  });
}
