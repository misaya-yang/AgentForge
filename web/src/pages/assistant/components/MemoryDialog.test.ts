import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Hook = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
function nodes(value: unknown): Tree[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const node = value as Tree; return [node, ...nodes(node.props?.children)];
}
function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join(" ");
  return value && typeof value === "object" ? text((value as Tree).props?.children) : "";
}
const sample = (value = "private-A", enabled = true) => ({ enabled, effect: "New runs only", has_more: false,
  items: [{ key: "name", value, source: "user-edit", namespace: "default", created_at: null, updated_at: null, expires_at: null }] });
const translate = (key: string, fallback?: string) => fallback ?? key;
function host() {
  let auth = { token: "token-a", isAuthenticated: true, user: { user_id: "a", permissions: ["read"], roles: ["member"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  let slots: Hook[] = []; const wrapper: Hook[] = []; let child: Hook[] = []; let index = 0; let key: string | undefined; let uuid = 0;
  let effects: Array<() => void> = []; let tree: unknown; let closed = 0;
  const requests: Array<{ path: string; method: string; data: unknown; resolve: (data: unknown) => void; reject: (error: Error) => void }> = [];
  const memo = (factory: () => unknown, deps: unknown[]) => { const i = index++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: factory(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = index++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Hook = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    react: { useState(initial: unknown) { const i = index++; const owner = slots; owner[i] ??= { value: initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: translate }) },
    "@/components/ui/dialog": Object.fromEntries(["Dialog", "DialogContent", "DialogDescription", "DialogHeader", "DialogTitle"].map(name => [name, name])),
    "@/components/ui/button": { Button: "Button" }, "@/components/ui/input": { Input: "Input" },
    "@/lib/utils": { generateUUID: () => `scope-${++uuid}` },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (value: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, prior: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/api": { api: Object.fromEntries(["get", "patch", "put", "delete"].map(method => [method,
      (path: string, data?: unknown) => new Promise((resolve, reject) => requests.push({ path, method, data, resolve: value => resolve({ data: value }), reject }))])) },
  };
  function compile(path: string) { const exports: Record<string, (props: unknown) => Tree> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, AbortController, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/lib/sdkAuthorityScope"] = compile("../../../lib/sdkAuthorityScope.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = compile("../../../hooks/useAuthenticatedReadScope.ts");
  modules["@/api/assistantMemory"] = compile("../../../api/assistantMemory.ts");
  const component = compile("./MemoryDialog.tsx").MemoryDialog;
  const flush = () => { const pending = effects; effects = []; pending.forEach(fn => fn()); };
  const dispose = () => { child.forEach(hook => hook.cleanup?.()); child = []; };
  return {
    requests,
    render(open = true) {
      slots = wrapper; index = 0; const outer = component({ open, onClose: () => { closed += 1; } }); flush();
      if (!outer) { dispose(); key = undefined; tree = null; return tree; }
      if (outer.key !== key) { dispose(); key = outer.key; }
      // Baseline is a direct Dialog, candidate has a keyed inner editor.
      if (typeof outer.type === "function") { slots = child; index = 0; tree = (outer.type as (props: unknown) => Tree)(outer.props); } else tree = outer;
      flush(); return tree;
    },
    auth(patch: Partial<typeof auth>) { const previous = auth; auth = { ...auth, ...patch }; [...subscribers].forEach(fn => fn(auth, previous)); },
    find(type: string, label?: string) { const found = nodes(tree).find(node => node.type === type && (!label || text(node).includes(label))); assert.ok(found, `${type}:${label}`); return found; },
    button(label: string) { return this.find("button", label).props.onClick as () => void; },
    label: () => text(tree), closed: () => closed,
  };
}
const settle = async () => { await new Promise(resolve => setImmediate(resolve)); };


const load = async (h: ReturnType<typeof host>) => { h.render(); h.requests[0].resolve(sample()); await settle(); h.render(); };
const change = (h: ReturnType<typeof host>) => (h.find("input").props.onChange as (event: unknown) => void)({ target: { checked: false } });

test("disable is single-flight through authoritative readback and retains records", async () => {
  const h = host(); await load(h); change(h); change(h);
  assert.equal(h.requests.filter(r => r.method === "patch").length, 1);
  assert.equal(JSON.stringify(h.requests[1].data), JSON.stringify({ enabled: false }));
  h.requests[1].resolve({ enabled: false }); await settle(); h.render();
  assert.equal(h.find("input").props.checked, true);
  assert.equal(h.find("input").props.disabled, true);
  h.requests[2].resolve(sample("private-A", false)); await settle(); h.render();
  assert.equal(h.find("input").props.checked, false); assert.match(h.label(), /private-A/);
  assert.match(h.label(), /does not erase existing records or revoke an active run/);
});
test("edit and confirmed delete use actual API and returned records", async () => {
  const h = host(); await load(h); h.button("Edit")(); h.render();
  (h.find("textarea").props.onChange as (event: unknown) => void)({ target: { value: "revised" } }); h.render();
  h.button("Save")(); h.button("Save")(); assert.equal(h.requests.length, 2);
  assert.equal(h.requests[1].method, "put"); assert.equal(JSON.stringify(h.requests[1].data), JSON.stringify({ key: "name", value: "revised" }));
  h.requests[1].resolve({}); await settle(); h.requests[2].resolve(sample("server-current")); await settle(); h.render();
  assert.match(h.label(), /server-current/); assert.doesNotMatch(h.label(), /private-A/);
  h.button("Delete")(); h.render(); assert.equal(h.requests.length, 3);
  h.button("Remove")(); h.button("Remove")(); assert.equal(h.requests.length, 4);
  assert.equal(h.requests[3].method, "delete"); assert.equal(JSON.stringify(h.requests[3].data), JSON.stringify({ params: { key: "name" } }));
  h.requests[3].resolve({}); await settle(); h.requests[4].resolve({ ...sample(), items: [] }); await settle(); h.render();
  assert.match(h.label(), /No long-term records saved/);
});
test("close and reopen clears drafts and fences captured actions and late reads", async () => {
  const h = host(); await load(h); h.button("Edit")(); h.render(); const save = h.button("Save");
  const close = h.find("Dialog").props.onOpenChange as (open: boolean) => void; close(false); save();
  assert.equal(h.requests.length, 1); assert.equal(h.closed(), 1);
  h.render(false); h.render(); assert.doesNotMatch(h.label(), /private-A/); assert.equal(h.requests.length, 2);
  close(false); assert.equal(h.closed(), 1);
  h.requests[1].resolve(sample("reopened")); await settle(); h.render(); assert.match(h.label(), /reopened/);
});
test("auth A-B-A retires loaded edit and suppresses old-user mutation before render", async () => {
  const h = host(); await load(h); h.button("Edit")(); h.render(); const save = h.button("Save");
  h.auth({ token: "token-b" }); h.auth({ token: "token-a" }); save();
  assert.equal(h.requests.length, 1); h.render(); assert.doesNotMatch(h.label(), /private-A/);
  assert.equal(h.requests.length, 2);
});
test("relogin fences mutation readback and shows only new-authority response", async () => {
  const h = host(); await load(h); change(h);
  h.auth({ token: "token-b", user: { user_id: "b", permissions: ["read"], roles: ["member"], tier: "normal" } });
  h.render(); assert.doesNotMatch(h.label(), /private-A/); assert.equal(h.requests.length, 3);
  h.requests[1].resolve({ enabled: false }); await settle(); assert.equal(h.requests.length, 3);
  h.requests[2].resolve(sample("private-B")); await settle(); h.render(); assert.match(h.label(), /private-B/);
});
test("late readback cannot restore records after close or permission replacement", async () => {
  for (const transition of ["close", "permission"]) {
    const h = host(); await load(h); change(h); h.requests[1].resolve({ enabled: false }); await settle();
    if (transition === "close") { (h.find("Dialog").props.onOpenChange as (open: boolean) => void)(false); h.render(false); }
    else h.auth({ user: { user_id: "a", permissions: [], roles: ["member"], tier: "normal" } });
    h.render(); h.requests[2].resolve(sample("retired-readback", false)); await settle(); h.render();
    assert.doesNotMatch(h.label(), /retired-readback|private-A/);
  }
});
test("unknown writes or failed readback require explicit read-only recovery, never replay", async () => {
  for (const failReadback of [false, true]) {
    const h = host(); await load(h); const stale = h.find("input").props.onChange as (event: unknown) => void; change(h);
    if (failReadback) { h.requests[1].resolve({ enabled: false }); await settle(); h.requests[2].reject(new Error("offline")); }
    else h.requests[1].reject(new Error("offline"));
    await settle(); h.render(); assert.match(h.label(), /memoryChangeUnknown/); assert.doesNotMatch(h.label(), /private-A/);
    stale({ target: { checked: false } }); assert.equal(h.requests.filter(r => r.method === "patch").length, 1);
    h.button("Retry")(); const recovery = h.requests.at(-1)!; assert.equal(recovery.method, "get");
    recovery.resolve(sample("confirmed-current", false)); await settle(); h.render();
    assert.match(h.label(), /confirmed-current/); assert.doesNotMatch(h.label(), /memoryChangeUnknown/); assert.equal(h.find("input").props.checked, false);
    assert.equal(h.requests.filter(r => r.method !== "get").length, 1);
  }
});
test("initial load failure ends loading and retry can recover without a write", async () => {
  const h = host(); h.render(); h.requests[0].reject(new Error("offline")); await settle(); h.render();
  assert.match(h.label(), /Could not load memory settings/); assert.doesNotMatch(h.label(), /Loading/);
  h.button("Retry")(); h.requests[1].resolve(sample()); await settle(); h.render(); assert.match(h.label(), /private-A/);
  assert.equal(h.requests.every(r => r.method === "get"), true);
});
test("signout suppresses new reads and stale actions", async () => {
  const h = host(); await load(h); const toggle = h.find("input").props.onChange as (event: unknown) => void;
  h.auth({ isAuthenticated: false }); toggle({ target: { checked: false } }); assert.equal(h.render(), null);
  assert.equal(h.requests.length, 1);
});

test("refresh discards edited drafts and delete confirmations before displaying current records", async () => {
  for (const mode of ["Edit", "Delete"]) {
    const h = host(); await load(h); h.button(mode)(); h.render();
    if (mode === "Edit") { (h.find("textarea").props.onChange as (event: unknown) => void)({ target: { value: "old-draft" } }); h.render(); }
    h.button("Refresh")(); h.requests[1].resolve(sample("fresh-server")); await settle(); h.render();
    assert.match(h.label(), /fresh-server/); assert.doesNotMatch(h.label(), /old-draft|Remove this record/);
    assert.throws(() => h.find("textarea"));
  }
});
test("reenable reads actual enabled preference without deleting prior records", async () => {
  const h = host(); h.render(); h.requests[0].resolve(sample("saved", false)); await settle(); h.render();
  (h.find("input").props.onChange as (event: unknown) => void)({ target: { checked: true } });
  assert.equal(JSON.stringify(h.requests[1].data), JSON.stringify({ enabled: true }));
  h.requests[1].resolve({ enabled: true }); await settle(); h.requests[2].resolve(sample("saved", true)); await settle(); h.render();
  assert.equal(h.find("input").props.checked, true); assert.match(h.label(), /saved/);
});
