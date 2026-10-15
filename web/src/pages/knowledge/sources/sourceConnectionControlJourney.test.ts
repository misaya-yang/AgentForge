import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
const id = "legacy-connection";
const row = { connection_id: id, name: "Cloud", domain: "fixture.atlassian.net", status: "active", sync_mode: "manual", revision: "2026-10-12T00:00:00+00:00" };
const key = 'kb-source-control:["owner","dataset"]';
const intent = { connection_id: id, expected_revision: row.revision, expected_status: "active", status: "disabled" };
const impact = { connection: row, binding_count: 3, bindings: [{ binding_id: "visible", dataset_id: "dataset", space_name: "Engineering", status: "pending" }], hidden_count: 2, next_offset: null };
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setImmediate(resolve)); };
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  return [...((value as Node).type === type ? [value as Node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string { return typeof value === "string" ? value : value && typeof value === "object" ? Object.values(value).map(text).join(" ") : ""; }
function host({ restored = false, credentialRestored = false, storageFailed = false } = {}) {
  const state: unknown[] = [], effects: Array<{ deps: unknown[]; cleanup?: () => void }> = [];
  const storage = new Map<string, string>(); if (restored) storage.set(key, JSON.stringify(intent));
  if (credentialRestored) storage.set(key, JSON.stringify({ operation: "credentials", connection_id: id, expected_revision: row.revision, expected_status: "active" }));
  let cursor = 0, effectCursor = 0, enabled = true, reconnectEnabled = true;
  let pendingEffects: Array<() => void> = [], wait: Promise<void> | undefined, release: (() => void) | undefined;
  let response: unknown = { ...row, status: "disabled", revision: "2026-10-12T00:00:01+00:00" }, readback: unknown = impact;
  const calls: Array<{ method: string; url: string; body?: unknown }> = [];
  const named = new Proxy({}, { get: (_t, name) => String(name) });
  const jsx = (type: unknown, props: Data) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = typeof initial === "function" ? initial() : initial;
        return [state[slot], (value: unknown) => { state[slot] = typeof value === "function" ? value(state[slot]) : value; }]; },
      useRef(initial: unknown) { const slot = cursor++; if (!(slot in state)) state[slot] = { current: initial }; return state[slot]; },
      useEffect(callback: () => (() => void) | void, deps: unknown[]) { const index = effectCursor++, old = effects[index];
        if (!old || deps.some((value, i) => value !== old.deps[i])) pendingEffects.push(() => { old?.cleanup?.(); effects[index] = { deps, cleanup: callback() || undefined }; }); },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "@/components/ui/button": { Button: "Button" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/lib/api": { api: {
      async post(url: string, body: unknown) { assert.ok(storage.get(key)); calls.push({ method: "POST", url, body: structuredClone(body) }); const value = response; await wait; if (value instanceof Error) throw value; return { data: value }; },
      async patch(url: string, body: unknown) { assert.ok(storage.get(key)); calls.push({ method: "PATCH", url, body: structuredClone(body) }); const value = response; await wait; if (value instanceof Error) throw value; return { data: value }; },
      async get(url: string) { calls.push({ method: "GET", url }); const value = url.endsWith("/manage") ? { connections: [row], next_offset: null } : readback; await wait; if (value instanceof Error) throw value; return { data: value }; },
    } },
  };
  function load(path: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => modules[name] ?? named, AbortController,
      sessionStorage: { getItem(k: string) { if (storageFailed) throw Error("blocked"); return storage.get(k) ?? null; }, setItem(k: string, v: string) { if (storageFailed) throw Error("blocked"); storage.set(k, v); }, removeItem(k: string) { if (storageFailed) throw Error("blocked"); storage.delete(k); } },
    }); return exports;
  }
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./sourceConnectionControlState"] = load("./sourceConnectionControlState.ts");
  const component = load("./SourceConnectionControls.tsx").SourceConnectionControlsSession as (props: Data) => unknown;
  function render() { cursor = 0; effectCursor = 0; const result = component({ datasetId: "dataset", enabled, reconnectEnabled, userId: "owner", onChanged() {} }); const work = pendingEffects; pendingEffects = []; work.forEach(run => run()); return result; }
  function button(label: string) { return nodes(render(), "Button").find(node => text(node).includes(`connectionControls.${label}`)); }
  function click(label: string) { const b = button(label); assert.ok(b, label); assert.ok(!b.props.disabled, label); (b.props.onClick as () => void)(); }
  return { render, calls, storage,
    reconnectReady(value: boolean) { reconnectEnabled = value; render(); },
    credentials() { for (const name of ["replacement-email", "replacement-token"]) { const input = nodes(render(), "input").find(n => n.props.name === name); assert.ok(input); (input.props.onChange as (event: unknown) => void)({ target: { value: name === "replacement-email" ? "replacement@example.invalid" : "synthetic-replacement-only" } }); } }, click, button, setResponse(value: unknown) { response = value; }, setRead(value: unknown) { readback = value; }, disable() { enabled = false; render(); }, enable() { enabled = true; render(); },
    confirm() { const box = nodes(render(), "input")[0]; assert.ok(box); (box.props.onChange as (event: unknown) => void)({ target: { checked: true } }); },
    pause() { wait = new Promise(resolve => { release = resolve; }); }, resume() { release?.(); wait = undefined; },
    async review() { click("load"); await flush(); click("review"); await flush(); },
  };
}

test("production controls require impact confirmation, send exact CAS once, and only read after acknowledgment", async () => {
  const h = host(); await h.review();
  assert.ok(text(h.render()).includes("connectionControls.hidden"));
  assert.ok(h.button("pause")?.props.disabled);
  h.confirm(); h.pause(); h.click("pause");
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls.at(-1)?.body)), { expected_revision: row.revision, expected_status: "active", status: "disabled" });
  assert.equal(h.calls.at(-1)?.url, `/api/v1/knowledge/dataset/sources/confluence/connections/${id}/control`);
  assert.equal(h.storage.get(key), JSON.stringify(intent));
  h.resume(); await flush(); assert.ok(text(h.render()).includes("connectionControls.saved"));
  h.setRead({ ...impact, connection: { ...row, status: "disabled", revision: "new-revision" } });
  h.click("recheck"); await flush(); h.click("acceptCurrent");
  assert.equal(h.storage.size, 0); assert.ok(h.button("resume")?.props.disabled);
  h.confirm(); h.click("resume"); await flush();
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 2);
  assert.equal((h.calls.at(-1)?.body as Data).expected_revision, "new-revision");
});

test("lost reply plus GET404 retains uncertain intent and never retries on render or read", async () => {
  const h = host(); await h.review(); h.confirm(); h.setResponse(Error("lost ack")); h.click("pause"); await flush();
  h.setRead(Error("404")); h.click("recheck"); await flush(); h.render();
  assert.equal(h.storage.get(key), JSON.stringify(intent));
  assert.ok(text(h.render()).includes("connectionControls.uncertain"));
  assert.equal(h.button("acceptCurrent"), undefined);
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 1);
});

test("reload recovery allows readback without mutation capability and requires explicit adoption", async () => {
  const h = host({ restored: true }); h.disable(); h.click("recheck"); await flush();
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].method, "GET");
  assert.equal(h.storage.size, 1); h.click("acceptCurrent"); assert.equal(h.storage.size, 0);
  assert.equal(h.calls.length, 1);
});

test("stale recovery owner and unavailable storage block mutation", async () => {
  const h = host(); await h.review(); h.confirm(); h.storage.set(key, JSON.stringify({ ...intent, connection_id: "another" })); h.click("pause"); await flush();
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 0);
  assert.ok(text(h.render()).includes("connectionControls.storage"));
  const blocked = host({ storageFailed: true }); await blocked.review(); blocked.confirm(); assert.ok(blocked.button("pause")?.props.disabled);
});

test("mismatched readback never releases uncertainty; unsupported legacy state has no mutation control", async () => {
  const h = host({ restored: true }); h.setRead({ ...impact, connection: { ...row, connection_id: "other" } }); h.click("recheck"); await flush();
  assert.equal(h.button("acceptCurrent"), undefined); assert.equal(h.storage.size, 1);
  const other = host(); other.setRead({ ...impact, connection: { ...row, status: "error" } }); await other.review();
  assert.equal(other.button("pause"), undefined); assert.equal(other.button("resume"), undefined);
});

test("losing capability resets impact confirmation", async () => {
  const h = host(); await h.review(); h.confirm(); h.disable();
  assert.equal(h.button("pause"), undefined);
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 0);
});


test("capability loss and regain discard a late impact response", async () => {
  const h = host(); h.click("load"); await flush(); h.pause(); h.click("review");
  h.disable(); h.enable(); h.resume(); await flush();
  assert.equal(h.button("pause"), undefined); assert.equal(h.button("review"), undefined);
  assert.equal(h.calls.filter(c => c.method === "PATCH").length, 0);
});


test("replacement transmits once with CAS, clears secrets, preserves status and uses metadata-only recovery", async () => {
  const h = host(); await h.review(); h.confirm(); h.credentials();
  h.setResponse({ ...row, revision: "new-revision" }); h.pause(); h.click("replaceCredentials");
  const post = h.calls.find(c => c.method === "POST"); assert.ok(post);
  assert.equal(post.url, `/api/v1/knowledge/dataset/sources/confluence/connections/${id}/credentials`);
  assert.deepEqual(post.body, { expected_revision: row.revision, expected_status: "active", email: "replacement@example.invalid", api_token: "synthetic-replacement-only" });
  assert.deepEqual(JSON.parse(h.storage.get(key)!), { operation: "credentials", connection_id: id, expected_revision: row.revision, expected_status: "active" });
  assert.ok(!JSON.stringify(h.render()).includes("synthetic-replacement-only"));
  h.render(); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
  h.resume(); await flush(); assert.ok(text(h.render()).includes("credentialsSaved"));
  h.setRead({ ...impact, connection: { ...row, revision: "new-revision" } }); h.click("recheck"); await flush();
  h.click("acceptCurrent"); assert.equal(h.storage.size, 0); assert.ok(h.button("replaceCredentials")?.props.disabled);
  assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
});

test("lost replacement reply and failed metadata read never retry or recover secrets", async () => {
  const h = host(); await h.review(); h.confirm(); h.credentials(); h.setResponse(Error("lost ack")); h.click("replaceCredentials"); await flush();
  assert.ok(text(h.render()).includes("credentialsUncertain")); h.setRead(Error("404")); h.click("recheck"); await flush();
  assert.equal(h.button("acceptCurrent"), undefined); assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
  assert.ok(!h.storage.get(key)?.includes("replacement@example.invalid"));
  assert.ok(!JSON.stringify(h.render()).includes("synthetic-replacement-only"));
  h.setRead(impact); h.click("recheck"); await flush(); h.click("acceptCurrent");
  assert.ok(h.button("replaceCredentials")?.props.disabled);
});

test("replacement readiness loss clears credentials and old confirmation while pause stays available", async () => {
  const h = host(); await h.review(); h.confirm(); h.credentials(); h.reconnectReady(false);
  await h.review(); assert.equal(h.button("replaceCredentials"), undefined); assert.ok(h.button("pause"));
  h.reconnectReady(true); await h.review(); assert.ok(h.button("replaceCredentials")?.props.disabled);
  assert.ok(nodes(h.render(), "input").filter(n => n.props.name).every(n => n.props.value === ""));
  assert.ok(h.button("pause")?.props.disabled);
});

test("replacement pending intent restores for metadata recovery without secret entry", async () => {
  const h = host(); await h.review(); h.confirm(); h.credentials(); h.setResponse(Error("lost ack")); h.click("replaceCredentials"); await flush();
  h.disable(); h.click("recheck"); await flush(); h.click("acceptCurrent");
  assert.equal(h.calls.filter(c => c.method === "POST").length, 1); assert.equal(h.storage.size, 0);
});


test("restored credential intent performs only metadata reads and needs explicit acknowledgement", async () => {
  const h = host({ credentialRestored: true }); h.disable();
  assert.ok(text(h.render()).includes("credentialsUncertain")); h.click("recheck"); await flush();
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].method, "GET");
  h.click("acceptCurrent"); assert.equal(h.storage.size, 0);
});
