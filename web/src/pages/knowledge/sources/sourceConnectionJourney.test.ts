import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Data = Record<string, unknown>;
type Node = { type: unknown; props: Data };
const id = "6f54b77d-96af-4a3a-a15e-bac5fb09c21c";
const fixture = JSON.parse(readFileSync(new URL("./sourceConnectionContract.fixture.json", import.meta.url), "utf8"));
const intent = fixture.request;
const receipt = fixture.receipt;
const key = 'kb-source-connection:["owner","dataset"]';
const secretEmail = "synthetic-secret-email@example.invalid", secretToken = "synthetic-secret-token-only";
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setImmediate(resolve)); };
function nodes(value: unknown, type: string): Node[] {
  if (!value || typeof value !== "object") return [];
  return [...((value as Node).type === type ? [value as Node] : []), ...Object.values(value).flatMap(v => nodes(v, type))];
}
function text(value: unknown): string { return typeof value === "string" ? value : value && typeof value === "object" ? Object.values(value).map(text).join(" ") : ""; }
function host({ restored = false, storageFailed = false } = {}) {
  const state: unknown[] = [], effects: Array<{ deps: unknown[]; cleanup?: () => void }> = [];
  const storage = new Map<string, string>(); if (restored) storage.set(key, JSON.stringify(intent));
  let cursor = 0, effectCursor = 0, enabled = true, reason: string | null = null;
  let pendingEffects: Array<() => void> = [], wait: Promise<void> | undefined, release: (() => void) | undefined;
  let post: unknown = receipt, status: unknown = receipt;
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
      async post(url: string, body: unknown) { assert.ok(storage.get(key)); calls.push({ method: "POST", url, body: structuredClone(body) }); const value = post; await wait; if (value instanceof Error) throw value; return { data: value }; },
      async get(url: string) { calls.push({ method: "GET", url }); const value = status; await wait; if (value instanceof Error) throw value; return { data: value }; },
    } },
  };
  function load(path: string) {
    const exports: Record<string, unknown> = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => modules[name] ?? named, AbortController, crypto: { randomUUID: () => id },
      sessionStorage: { getItem(k: string) { if (storageFailed) throw Error("blocked"); return storage.get(k) ?? null; }, setItem(k: string, v: string) { if (storageFailed) throw Error("blocked"); storage.set(k, v); }, removeItem(k: string) { if (storageFailed) throw Error("blocked"); storage.delete(k); } },
    }); return exports;
  }
  modules["@/api/knowledge"] = load("../../../api/knowledge.ts");
  modules["./sourceConnectionContract"] = load("./sourceConnectionContract.ts");
  const component = load("./SourceConnectionSetup.tsx").SourceConnectionSetupSession as (props: Data) => unknown;
  function render() { cursor = 0; effectCursor = 0; const result = component({ datasetId: "dataset", enabled, reason, userId: "owner" }); const work = pendingEffects; pendingEffects = []; work.forEach(run => run()); return result; }
  function field(name: string, value: string) { const input = nodes(render(), "input").find(node => node.props.name === name); assert.ok(input); (input.props.onChange as (event: unknown) => void)({ target: { value } }); }
  function click(label: string) { const button = nodes(render(), "Button").find(node => text(node).includes(`connectionSetup.${label}`)); assert.ok(button, label); assert.ok(!button.props.disabled, label); (button.props.onClick as () => void)(); }
  return { render, calls, storage, click, field, setPost(value: unknown) { post = value; }, setStatus(value: unknown) { status = value; }, disable(value: string | null = null) { enabled = false; reason = value; render(); }, enable() { enabled = true; reason = null; },
    pause() { wait = new Promise(resolve => { release = resolve; }); }, resume() { release?.(); wait = undefined; },
    fill() { field("connection-name", intent.name); field("cloud-hostname", intent.domain); field("confluence-email", secretEmail); field("confluence-token", secretToken); },
  };
}

test("production form and API submit once, clear secrets, save only safe recovery, never bind/sync", async () => {
  const h = host(); h.fill(); h.pause(); h.click("save");
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].url, "/api/v1/knowledge/dataset/sources/confluence/connections");
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].body)), { ...intent, email: secretEmail, api_token: secretToken });
  assert.equal(h.storage.get(key), JSON.stringify(intent));
  assert.ok(!text(h.render()).includes(secretToken) && !text(h.render()).includes(secretEmail));
  h.render(); assert.equal(h.calls.length, 1); h.resume(); await flush();
  assert.ok(text(h.render()).includes("connectionSetup.saved"));
  h.click("recheck"); await flush(); assert.equal(h.calls[1].method, "GET");
  assert.equal(h.calls[1].url, `/api/v1/knowledge/dataset/sources/confluence/connections/${id}`);
});

test("unknown POST and GET404 preserve UUID, never offer credential resubmission", async () => {
  const h = host(); h.setPost(Error("transport")); h.fill(); h.click("save"); await flush();
  assert.ok(text(h.render()).includes("connectionSetup.uncertain"));
  h.setStatus(Object.assign(Error("404"), { response: { status: 404 } })); h.click("recheck"); await flush();
  assert.equal(h.storage.get(key), JSON.stringify(intent)); assert.equal(nodes(h.render(), "input").length, 0);
  assert.equal(h.calls.filter(c => c.method === "POST").length, 1);
});

test("restored request reads exact receipt only and invalid receipt remains uncertain", async () => {
  const h = host({ restored: true }); assert.equal(h.calls.length, 0);
  h.setStatus({ ...receipt, connection_id: "different" }); h.click("recheck"); await flush();
  assert.ok(text(h.render()).includes("connectionSetup.readFailed")); assert.equal(h.storage.get(key), JSON.stringify(intent));
  assert.equal(h.calls.filter(c => c.method === "POST").length, 0);
});

test("definitive encryption rejection clears only this attempt and gives setup guidance", async () => {
  const h = host(); h.setPost(Object.assign(Error("rejected"), { response: { data: { detail: { code: "CONFLUENCE_CONNECTION_NOT_ACCEPTED", accepted: false, reason: "encryption_unavailable" } } } }));
  h.fill(); h.click("save"); await flush(); assert.equal(h.storage.has(key), false);
  assert.ok(text(h.render()).includes("connectionSetup.encryptionUnavailable"));
  assert.ok(!text(h.render()).includes(secretToken) && !text(h.render()).includes(secretEmail));
});

test("missing capability and failed storage block credential submission", () => {
  const h = host(); h.disable("encryption_key_required"); assert.equal(nodes(h.render(), "input").length, 0);
  assert.ok(text(h.render()).includes("connectionSetup.keyRequired")); assert.equal(h.calls.length, 0);
  const failed = host({ storageFailed: true }); assert.ok(text(failed.render()).includes("connectionSetup.storage")); assert.equal(failed.calls.length, 0);
});

test("a stale producer cannot overwrite another recovery identity", async () => {
  const h = host(); h.fill(); h.storage.set(key, JSON.stringify({ ...intent, request_id: "1cfb0645-5e43-4d20-9e1b-88e087c96681" }));
  h.click("save"); await flush(); assert.equal(h.calls.length, 0); assert.ok(text(h.render()).includes("connectionSetup.storage"));
});

test("failed recheck clears stale saved receipt and new-connection control", async () => {
  const h = host({ restored: true }); h.click("recheck"); await flush(); assert.ok(text(h.render()).includes("connectionSetup.saved"));
  h.setStatus(Error("revoked")); h.click("recheck"); await flush();
  assert.ok(!nodes(h.render(), "Button").some(node => text(node).includes("connectionSetup.another")));
  assert.ok(text(h.render()).includes("connectionSetup.uncertain"));
});

test("losing capability clears typed credentials without sending", () => {
  const h = host(); h.fill(); h.disable(); h.enable();
  const inputs = nodes(h.render(), "input");
  assert.equal(inputs.find(node => node.props.name === "confluence-email")?.props.value, "");
  assert.equal(inputs.find(node => node.props.name === "confluence-token")?.props.value, ""); assert.equal(h.calls.length, 0);
});
