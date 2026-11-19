import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
const fixture = JSON.parse(readFileSync(new URL("./sourceBindingContract.fixture.json", import.meta.url), "utf8"));
const key = 'kb-source-binding:["owner","dataset"]';
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  return [...((value as Node).type === type ? [value as Node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string {
  if (typeof value === "string") return value;
  return value && typeof value === "object" ? Object.values(value).map(text).join(" ") : "";
}
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setImmediate(resolve)); };

// Production React component, API functions, contract and backend-checked fixture.
// React host, session storage and transport are controlled; no browser acceptance.
function host({ restored = false, storageFailed = false } = {}) {
  const state: unknown[] = [], effects: Array<{ deps: unknown[]; cleanup?: () => void }> = [];
  const storage = new Map<string, string>();
  if (restored) storage.set(key, JSON.stringify(fixture.request));
  let cursor = 0, effectCursor = 0, enabled = true, refreshes = 0;
  let pendingEffects: Array<() => void> = [];
  let post: unknown = fixture.receipt, status: unknown = fixture.receipt, space: unknown = fixture.space;
  let connections: unknown = { connections: [{ connection_id: "connection", name: "Saved fixture" }], next_offset: null };
  let wait: Promise<void> | undefined, release: (() => void) | undefined;
  const calls: Array<{ method: string; url: string; body?: unknown; params?: Data }> = [];
  const named = new Proxy({}, { get: (_t, name) => String(name) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = typeof initial === "function" ? initial() : initial;
        return [state[slot], (value: unknown) => { state[slot] = typeof value === "function" ? value(state[slot]) : value; }]; },
      useRef(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = { current: initial }; return state[slot]; },
      useEffect(callback: () => (() => void) | void, deps: unknown[]) { const index = effectCursor++, old = effects[index];
        if (!old || deps.some((value, i) => value !== old.deps[i])) pendingEffects.push(() => {
          old?.cleanup?.(); effects[index] = { deps, cleanup: callback() || undefined };
        }); },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "@/components/ui/button": { Button: "Button" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/lib/api": { api: {
      async get(url: string, options?: { params?: Data }) {
        calls.push({ method: "GET", url, params: options?.params });
        const value = url.endsWith("/connections") ? connections : url.endsWith("/space") ? space : status;
        await wait; if (value instanceof Error) throw value; return { data: value };
      },
      async post(url: string, body: unknown) {
        calls.push({ method: "POST", url, body }); assert.ok(storage.get(key), "Persist identity before POST");
        const value = post; await wait; if (value instanceof Error) throw value; return { data: value };
      },
    } },
  };
  function load(path: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => modules[name] ?? named, AbortController,
      crypto: { randomUUID: () => fixture.request.request_id },
      sessionStorage: { getItem(k: string) { if (storageFailed) throw Error("blocked"); return storage.get(k) ?? null; },
        setItem(k: string, v: string) { if (storageFailed) throw Error("blocked"); storage.set(k, v); },
        removeItem(k: string) { if (storageFailed) throw Error("blocked"); storage.delete(k); } },
    }); return exports;
  }
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./sourceBindingContract"] = load("./sourceBindingContract.ts");
  const { SourceBindingSetupSession } = load("./SourceBindingSetup.tsx") as { SourceBindingSetupSession: (props: Data) => unknown };
  function tree() { cursor = 0; effectCursor = 0; pendingEffects = [];
    const result = SourceBindingSetupSession({ datasetId: "dataset", userId: "owner", enabled, onSaved: () => { refreshes++; } });
    pendingEffects.forEach(run => run()); return result;
  }
  function button(label: string) { return nodes(tree(), "Button").find(n => text(n).includes(`knowledge.sources.bindingSetup.${label}`)); }
  function click(label: string) { const b = button(label); assert.ok(b, `Missing ${label}`); assert.equal(b.props.disabled, false); (b.props.onClick as () => void)(); }
  function input(type: string, value: string) { const item = nodes(tree(), type)[0]; assert.ok(item, `Missing ${type}`);
    (item.props.onChange as (e: unknown) => void)({ target: { value } }); }
  async function select() { click("loadConnections"); await flush(); input("select", "connection"); input("input", "SPACE"); click("verify"); await flush(); }
  return { tree, button, click, input, select, calls, storage, refreshes: () => refreshes,
    setEnabled(v: boolean) { enabled = v; tree(); }, setStorageFailed(v: boolean) { storageFailed = v; },
    setPost(v: unknown) { post = v; }, setStatus(v: unknown) { status = v; }, setSpace(v: unknown) { space = v; }, setConnections(v: unknown) { connections = v; },
    defer() { wait = new Promise<void>(resolve => { release = resolve; }); }, release() { release?.(); wait = undefined; },
    close() { effects.forEach(e => e.cleanup?.()); release?.(); },
  };
}

test("saved connection -> exact homepage preview -> binding, without automatic sync", async () => {
  const h = host(); try {
    h.tree(); await flush(); assert.equal(h.calls.length, 0);
    await h.select(); assert.ok(text(h.tree()).includes("Homepage")); assert.ok(text(h.tree()).includes("bindingSetup.scope"));
    assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
    h.click("save"); await flush();
    const posts = h.calls.filter(c => c.method === "POST"); assert.equal(posts.length, 1);
    assert.equal(JSON.stringify(posts[0].body), JSON.stringify(fixture.request));
    assert.ok(posts[0].url.endsWith("/confluence/bindings"));
    assert.ok(text(h.tree()).includes("bindingSetup.saved")); assert.equal(h.refreshes(), 1);
    assert.ok(h.calls.every(c => !c.url.endsWith("/sync")));
  } finally { h.close(); }
});

test("duplicate save click is fenced before React state settles", async () => {
  const h = host(); try {
    await h.select(); h.defer(); const save = h.button("save")?.props.onClick as () => void;
    save(); save(); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
    h.release(); await flush();
  } finally { h.close(); }
});

for (const failure of [new Error("lost response"), { ...fixture.receipt, binding_id: "wrong" }]) {
  test("ambiguous POST or wrong receipt retains exact request; readback is GET only", async () => {
    const h = host(); try {
      await h.select(); h.setPost(failure); h.click("save"); await flush();
      assert.ok(text(h.tree()).includes("bindingSetup.uncertain")); assert.ok(h.storage.has(key)); assert.equal(h.button("save"), undefined);
      h.setStatus(Object.assign(new Error("missing"), { response: { status: 404 } })); h.click("recheck"); await flush();
      assert.ok(h.storage.has(key)); assert.equal(h.button("another"), undefined);
      h.setStatus(fixture.receipt); h.click("recheck"); await flush();
      assert.ok(text(h.tree()).includes("bindingSetup.saved")); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
      assert.ok(h.calls.at(-1)?.url.endsWith(fixture.request.request_id));
    } finally { h.close(); }
  });
}

test("restored request does not automatically read or resubmit", async () => {
  const h = host({ restored: true }); try {
    h.tree(); await flush(); assert.equal(h.calls.length, 0); assert.equal(h.button("loadConnections"), undefined);
    h.click("recheck"); await flush(); assert.deepEqual(h.calls.map(c => c.method), ["GET"]);
  } finally { h.close(); }
});

test("pre-admission rejection releases only its own stored request", async () => {
  const h = host(); try {
    await h.select(); h.setPost(Object.assign(new Error("rejected"), { response: { data: { detail: fixture.rejection } } }));
    h.click("save"); await flush(); assert.equal(h.storage.has(key), false); assert.ok(text(h.tree()).includes("bindingSetup.rejected"));
  } finally { h.close(); }
});

test("late rejection cannot clear another producer's recovery identity", async () => {
  const h = host(); try {
    await h.select(); h.setPost(Object.assign(new Error("rejected"), { response: { data: { detail: fixture.rejection } } }));
    h.defer(); h.click("save"); const other = { ...fixture.request, request_id: "93f0e57e-3828-4a21-9edb-ce8f01bfaa99" };
    h.storage.set(key, JSON.stringify(other)); h.release(); await flush();
    assert.equal(h.storage.get(key), JSON.stringify(other)); assert.ok(text(h.tree()).includes("bindingSetup.storageFailed"));
  } finally { h.close(); }
});

test("storage failure before dispatch blocks the write", async () => {
  const h = host(); try { await h.select(); h.setStorageFailed(true); h.click("save"); await flush();
    assert.equal(h.calls.filter(c => c.method === "POST").length, 0); assert.ok(text(h.tree()).includes("bindingSetup.storageFailed"));
  } finally { h.close(); }
});

for (const change of ["selection", "capability", "unmount"]) {
  test(`abandoned preview cannot publish after ${change}`, async () => {
    const h = host(); try {
      h.click("loadConnections"); await flush(); h.input("select", "connection"); h.input("input", "SPACE");
      h.defer(); h.click("verify");
      if (change === "selection") h.input("input", "OTHER");
      if (change === "capability") h.setEnabled(false);
      if (change === "unmount") h.close();
      h.release(); await flush(); assert.equal(h.button("save"), undefined); assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
    } finally { h.close(); }
  });
}

test("connection paging follows next offset and invalidates the selection", async () => {
  const h = host(); try {
    h.setConnections({ connections: [{ connection_id: "connection", name: "Page one" }], next_offset: 50 });
    await h.select(); assert.ok(h.button("save"));
    h.setConnections({ connections: [{ connection_id: "next", name: "Page two" }], next_offset: null });
    h.click("next"); await flush(); assert.equal(h.button("save"), undefined);
    assert.equal(h.calls.at(-1)?.params?.offset, 50); assert.ok(text(h.tree()).includes("Page two"));
    h.click("previous"); await flush(); assert.equal(h.calls.at(-1)?.params?.offset, 0);
  } finally { h.close(); }
});

test("capability absence presents no new binding action", () => {
  const h = host(); try { h.setEnabled(false); assert.equal(h.tree(), null); assert.equal(h.calls.length, 0); } finally { h.close(); }
});
