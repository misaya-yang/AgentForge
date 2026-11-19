import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Node = { type: unknown; props: Record<string, unknown> };
type Data = Record<string, unknown>;
type RerankConfig = { enabled: boolean; model: string; provider?: "dashscope" | "cohere" | "jina" | "bge" };
const initial = (id = "a", rerank?: RerankConfig) => ({ dataset_id: id,
  chunking: { mode: "fixed_size", chunk_size: 500, chunk_overlap: 50, strip_html: false },
  retrieval: { mode: "hybrid", top_k: 5, fusion: { strategy: "rrf", alpha: 0.7, rrf_k: 91 }, ...(rerank ? { rerank } : {}) },
  embedding: { provider: "dashscope", model: "text-embedding-v4", dimension: 1024 },
  statistics: { document_count: 0, segment_count: 0 },
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  const node = value as Node;
  return [...(node.type === type ? [node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (!value || typeof value !== "object") return "";
  return Object.values(value).map(text).join(" ");
}
function merge(current: Data, patch: Data): Data {
  const result = structuredClone(current);
  for (const [key, value] of Object.entries(patch)) result[key] = value && typeof value === "object" && !Array.isArray(value)
    ? merge((result[key] ?? {}) as Data, value as Data) : value;
  return result;
}
// Actual SettingsTab + API functions; React scheduling/storage and HTTP are
// controlled collaborators. This is no-listener evidence, not browser acceptance.
function host(storage = new Map<string, string>(), initialRerank?: RerankConfig) {
  const slots: unknown[] = [], cleanups = new Map<number, () => void>();
  let cursor = 0, dirty = true, effects: (() => void)[] = [], tree: unknown;
  const unsaved: boolean[] = [], events = new Map<string, unknown>(), errors: unknown[] = [];
  const calls: { method: string; url: string; payload?: Data }[] = [];
  const auth = { user: { user_id: "owner-a" }, token: "synthetic-session-a" };
  const live = new Map<string, ReturnType<typeof initial>>([["a", initial("a", initialRerank)],["b", initial("b")]]);
  let read = async (id: string): Promise<unknown> => structuredClone(live.get(id));
  let write = async (id: string, payload: Data) => {
    const data = live.get(id)!;
    if (payload.chunking_config) data.chunking = structuredClone(payload.chunking_config) as typeof data.chunking;
    if (payload.retrieval_config) data.retrieval = merge(data.retrieval, payload.retrieval_config as Data) as typeof data.retrieval;
    if (payload.embedding_model) data.embedding = { provider: payload.embedding_provider as string, model: payload.embedding_model as string, dimension: payload.embedding_dimension as number };
    return { status: "success", dataset_id: id };
  };
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
    react: { ...react, useCallback: (f: () => unknown, deps: unknown[]) => react.useMemo(() => f, deps) },
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "Fragment" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) }, "lucide-react": named,
    "@/store/useAuthStore": { useAuthStore: (f: (state: typeof auth) => unknown) => f(auth) },
    "@/hooks/use-toast": { toast: { error: (...args: unknown[]) => errors.push(args), success: () => {} } },
    "@/pages/knowledge/detail/useDatasetUploadController": { DATASET_EMBEDDING_MODELS: [
      { provider: "dashscope", model: "text-embedding-v4", dimension: 1024 },
      { provider: "fixture", model: "new-model", dimension: 768 },
    ] },
    "@/lib/api": { getApiBaseUrl: () => "", api: {
      async get(url: string) { calls.push({ method: "GET", url }); if (url.endsWith("/debug")) return { data: null };
        return { data: await read(url.split("/").at(-2)!) }; },
      async put(url: string, payload: Data) { calls.push({ method: "PUT", url, payload: structuredClone(payload) });
        return { data: await write(url.endsWith("/config") ? url.split("/").at(-2)! : url.split("/").at(-1)!, payload) }; },
    } },
  };
  function load(file: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText;
    vm.runInNewContext(code, { exports, require: (id: string) => modules[id] ?? named,
      console: { error: () => {} }, setTimeout, sessionStorage: {
        getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key),
      }, window: { addEventListener: (key: string, value: unknown) => events.set(key, value), removeEventListener: (key: string) => events.delete(key) },
    });
    return exports;
  }
  modules["@/types/knowledge"] = load("../../../types/knowledge.ts");
  modules["./settingsDraftScope"] = load("./settingsDraftScope.ts");
  modules["./settingsConfigSave"] = load("./settingsConfigSave.ts");
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  const { SettingsTab } = load("./SettingsTab.tsx") as { SettingsTab: (props: Data) => unknown };
  let props: Data = { datasetId: "a", dataset: { index_config: initial() }, active: true,
    onUnsavedChange: (v: boolean) => unsaved.push(v), onDatasetRefetch: () => {} };
  function render(next = props) { props = next; cursor = 0; dirty = false; tree = SettingsTab(props); const q = effects; effects = []; q.forEach(f => f()); }
  async function flush() { for (let i = 0; i < 12; i++) { await new Promise<void>(resolve => setImmediate(resolve)); if (dirty) render(); } }
  function card(section: string) { return nodes(tree, "Card").find(n => text(n).includes(`knowledge.detail.config${section}`))!; }
  function button(key: string, within = tree) { const node = nodes(within, "Button").find(n => text(n).includes(`knowledge.detail.${key}`)); assert.ok(node, key); return node; }
  async function edit(section = "Retrieval") { const within = card(section); const node = button(section === "Embedding" ? "modify" : "edit", within); assert.equal(Boolean(node.props.disabled), false); (node.props.onClick as () => void)(); await flush(); }
  async function topK(value: number) { const node = nodes(card("Retrieval"), "input").find(n => n.props.min === 1 && n.props.max === 20)!;
    (node.props.onChange as (e: unknown) => void)({ target: { value: String(value) } }); await flush(); }
  async function rerankModel(value: string) { const node = nodes(card("Retrieval"), "Input").find(n => typeof n.props.onChange === "function");
    assert.ok(node, "rerank model input");
    (node.props.onChange as (e: unknown) => void)({ target: { value } }); await flush(); }
  async function click(key: string, within = tree) { const node = button(key, within); assert.equal(Boolean(node.props.disabled), false, key); await (node.props.onClick as () => Promise<void>)(); await flush(); }
  return { auth, live, storage, calls, errors, events, unsaved, render, flush, edit, topK, rerankModel, click, button, card,
    current: () => tree, props: () => props, dispose: () => cleanups.forEach(fn => fn()),
    setRead: (fn: typeof read) => { read = fn; }, setWrite: (fn: typeof write) => { write = fn; },
    defaultWrite: write, defaultRead: read,
  };
}

for (const section of ["Chunking", "Retrieval", "Embedding"]) test(`${section}: accepted mutation plus failed GET retains draft, GET-only retry settles`, async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(section);
  if (section === "Retrieval") await h.topK(9);
  h.setRead(async () => { throw Error("readback unavailable"); });
  await h.click(section === "Embedding" ? "save" : "saveConfig", h.card(section));
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1);
  assert.equal(h.unsaved.at(-1), true); assert.equal(h.events.has("beforeunload"), true);
  assert.ok(JSON.parse(h.storage.get("kb-settings:owner-a:a")!).saveAttempt);
  assert.equal(h.button("cancel", h.card(section)).props.disabled, true);
  assert.ok(text(h.current()).includes("configSaveAcceptedUnconfirmed"));
  h.setRead(h.defaultRead); await h.click("configSaveCheck"); await h.flush();
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1);
  assert.equal(h.unsaved.at(-1), false); assert.equal(h.storage.size, 0);
  h.dispose();
});

test("retrieval settings save an explicit rerank model ID while retaining the configured provider", async () => {
  const h = host(new Map(), { enabled: true, model: "qwen3-rerank", provider: "dashscope" });
  h.render(); await h.flush(); await h.edit();
  const modelInput = nodes(h.card("Retrieval"), "Input").find(n => n.props.value === "qwen3-rerank");
  assert.ok(modelInput, "configured model ID is editable");
  assert.equal(modelInput.props.id, "knowledge-rerank-model");
  await h.rerankModel("account-specific-rerank-id");
  assert.equal(JSON.parse(h.storage.get("kb-settings:owner-a:a")!).rerankModel, "account-specific-rerank-id");

  await h.click("saveConfig", h.card("Retrieval"));
  const submitted = h.calls.filter(call => call.method === "PUT").at(-1)?.payload?.retrieval_config as Data;
  const rerank = submitted.rerank as Data;
  assert.deepEqual(rerank, { enabled: true, model: "account-specific-rerank-id" });
  assert.equal("provider" in rerank, false, "the editor does not replace the configured provider");
  assert.deepEqual(h.live.get("a")!.retrieval.rerank, {
    enabled: true, model: "account-specific-rerank-id", provider: "dashscope",
  });
  assert.equal(h.unsaved.at(-1), false);
  h.dispose();
});

test("new edit during readback remains dirty after the submitted values are confirmed", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  const read = deferred<unknown>(); h.setRead(() => read.promise);
  const saving = h.click("saveConfig", h.card("Retrieval")); await h.flush();
  await h.topK(11); read.resolve(structuredClone(h.live.get("a"))); await saving; await h.flush();
  assert.equal(h.unsaved.at(-1), true);
  const draft = JSON.parse(h.storage.get("kb-settings:owner-a:a")!);
  assert.equal(draft.topK, 11); assert.equal(draft.saveAttempt, null);
  assert.equal(h.live.get("a")!.retrieval.top_k, 9); h.dispose();
});

test("HTTP400 after commit reconciles by GET without another write", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  h.setWrite(async (id, body) => { await h.defaultWrite(id, body); throw Object.assign(Error("compensation failed"), { response: { status: 400 } }); });
  await h.click("saveConfig", h.card("Retrieval"));
  assert.equal(h.live.get("a")!.retrieval.top_k, 9); assert.equal(h.unsaved.at(-1), true);
  assert.ok(text(h.current()).includes("configSaveObserved"));
  await h.click("configSaveUseCurrent"); assert.equal(h.unsaved.at(-1), false);
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1); h.dispose();
});

test("mismatched or stale readback needs explicit draft discard; no replay", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  h.setRead(async () => initial()); await h.click("saveConfig", h.card("Retrieval"));
  assert.equal(h.unsaved.at(-1), true); assert.ok(text(h.current()).includes("configSaveConflict"));
  assert.equal(h.button("saveConfig", h.card("Retrieval")).props.disabled, true);
  await h.click("configSaveUseCurrent"); assert.equal(h.unsaved.at(-1), false);
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1); h.dispose();
});

test("refresh during mutation restores recovery-only state and does not replay", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  const write = deferred<{ status: string; dataset_id: string }>(); h.setWrite(() => write.promise);
  const saving = h.click("saveConfig", h.card("Retrieval")); await h.flush();
  const saved = h.storage.get("kb-settings:owner-a:a")!;
  assert.equal(JSON.parse(saved).saveAttempt.phase, "staged"); h.dispose();
  const reopened = host(new Map([["kb-settings:owner-a:a", saved]])); reopened.render(); await reopened.flush();
  assert.equal(reopened.unsaved.at(-1), true); assert.ok(text(reopened.current()).includes("configSaveUnconfirmed"));
  assert.equal(reopened.button("saveConfig", reopened.card("Retrieval")).props.disabled, true);
  assert.equal(reopened.calls.filter(c => c.method === "PUT").length, 0);
  write.resolve({ status: "success", dataset_id: "a" }); await saving; reopened.dispose();
});

test("A to B to A fences the earlier read and preserves restored recovery", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  const old = deferred<unknown>(); h.setRead(() => old.promise);
  const saving = h.click("saveConfig", h.card("Retrieval")); await h.flush();
  h.setRead(h.defaultRead); h.render({ ...h.props(), datasetId: "b" }); await h.flush();
  h.render({ ...h.props(), datasetId: "a" }); await h.flush();
  old.resolve(initial()); await saving; await h.flush();
  assert.equal(h.unsaved.at(-1), true); assert.ok(text(h.current()).includes("configSaveAcceptedUnconfirmed"));
  assert.equal(JSON.parse(h.storage.get("kb-settings:owner-a:a")!).topK, 9); h.dispose();
});

test("new account/session on same dataset cannot receive prior draft or late response", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  const old = deferred<unknown>(); h.setRead(() => old.promise);
  const saving = h.click("saveConfig", h.card("Retrieval")); await h.flush();
  h.auth.user.user_id = "owner-b"; h.auth.token = "synthetic-session-b"; h.setRead(h.defaultRead); h.render(); await h.flush();
  old.resolve(initial()); await saving; await h.flush();
  assert.equal(h.unsaved.at(-1), false); assert.equal(h.storage.has("kb-settings:owner-b:a"), false);
  assert.ok(h.storage.has("kb-settings:owner-a:a")); h.dispose();
});

test("migration refresh does not discard an edited value or recovery marker", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  h.setRead(async () => { throw Error("readback unavailable"); }); await h.click("saveConfig", h.card("Retrieval"));
  h.setRead(h.defaultRead);
  const panel = nodes(h.current(), "EmbeddingMigrationPanel")[0]; assert.ok(panel);
  const callback = panel.props.onMigrationChange;
  assert.equal(typeof callback, "function"); (callback as () => void)(); await h.flush();
  assert.equal(h.unsaved.at(-1), true); assert.ok(JSON.parse(h.storage.get("kb-settings:owner-a:a")!).saveAttempt); h.dispose();
});


test("a migration read started before save cannot overwrite confirmed saved values", async () => {
  const h = host(); h.render(); await h.flush();
  const old = deferred<unknown>(); h.setRead(() => old.promise);
  const panel = nodes(h.current(), "EmbeddingMigrationPanel")[0];
  (panel.props.onMigrationChange as () => void)(); await h.flush();
  // Existing editor remains usable while a background refresh is pending.
  h.setRead(h.defaultRead); await h.edit(); await h.topK(9);
  await h.click("saveConfig", h.card("Retrieval"));
  assert.equal(h.unsaved.at(-1), false);
  old.resolve(initial()); await h.flush();
  await h.edit();
  const range = nodes(h.card("Retrieval"), "input").find(n => n.props.min === 1 && n.props.max === 20)!;
  assert.equal(range.props.value, 9);
  // Cancel resets from saved values and exposes a stale datasetConfig overwrite.
  await h.click("cancel", h.card("Retrieval")); await h.edit();
  assert.equal(nodes(h.card("Retrieval"), "input").find(n => n.props.min === 1 && n.props.max === 20)!.props.value, 9);
  h.dispose();
});

test("accepted readback failure survives refresh without becoming unknown commit", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  h.setRead(async () => { throw Error("read unavailable"); }); await h.click("saveConfig", h.card("Retrieval"));
  const storage = new Map(h.storage); h.dispose();
  const reopened = host(storage); reopened.render(); await reopened.flush();
  assert.ok(text(reopened.current()).includes("configSaveAcceptedUnconfirmed"));
  assert.equal(reopened.calls.filter(c => c.method === "PUT").length, 0); reopened.dispose();
});


test("fast A to B to A with B still loading restores an enabled GET-only recovery", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  const oldA = deferred<unknown>(), pendingB = deferred<unknown>();
  h.setRead(() => oldA.promise);
  const saving = h.click("saveConfig", h.card("Retrieval")); await h.flush();
  h.setRead(id => id === "b" ? pendingB.promise : h.defaultRead(id));
  h.render({ ...h.props(), datasetId: "b" }); await h.flush();
  h.render({ ...h.props(), datasetId: "a" }); await h.flush();
  oldA.resolve(initial()); await saving; await h.flush();
  assert.equal(h.button("configSaveCheck").props.disabled, false);
  assert.equal(h.unsaved.at(-1), true);
  await h.click("configSaveCheck"); assert.equal(h.unsaved.at(-1), false);
  pendingB.resolve(initial("b")); await h.flush();
  assert.equal(h.unsaved.at(-1), false);
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1); h.dispose();
});


test("unavailable session storage preserves in-memory uncertainty and never replays", async () => {
  class UnavailableStorage extends Map<string, string> {
    override set(): this { throw Error("storage quota unavailable"); }
  }
  const h = host(new UnavailableStorage()); h.render(); await h.flush(); await h.edit(); await h.topK(9);
  h.setRead(async () => { throw Error("readback unavailable"); });
  await h.click("saveConfig", h.card("Retrieval"));
  assert.equal(h.unsaved.at(-1), true); assert.equal(h.events.has("beforeunload"), true);
  assert.ok(text(h.current()).includes("configSaveAcceptedUnconfirmed"));
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 1);
  h.setRead(h.defaultRead); await h.click("configSaveCheck");
  assert.equal(h.unsaved.at(-1), false); assert.equal(h.calls.filter(c => c.method === "PUT").length, 1);
  h.dispose();
});

test("locally rejected overlap never stages or transmits a mutation", async () => {
  const h = host(); h.render(); await h.flush(); await h.edit("Chunking");
  const ranges = nodes(h.card("Chunking"), "input");
  (ranges.find(n => n.props.max === 2000)!.props.onChange as (e: unknown) => void)({ target: { value: "100" } });
  (ranges.find(n => n.props.max === 200)!.props.onChange as (e: unknown) => void)({ target: { value: "200" } });
  await h.flush(); await h.click("saveConfig", h.card("Chunking"));
  assert.equal(h.calls.filter(c => c.method === "PUT").length, 0); assert.equal(h.errors.length, 1);
  assert.equal(h.unsaved.at(-1), true); assert.equal(JSON.parse(h.storage.get("kb-settings:owner-a:a")!).saveAttempt, null);
  h.dispose();
});
