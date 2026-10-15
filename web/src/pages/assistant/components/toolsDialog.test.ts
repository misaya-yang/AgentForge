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
const entry = (name: string, extra = {}) => ({ name, required_inputs: [], device_required: false, ...extra });
const sample = () => ({ platform_catalog: [entry("catalog-only", { tenant_visible: false })], next_turn_estimate: { status: "available_now", tools: [entry("dynamic-mcp", { effect: "write", approval: "required", required_inputs: ["target"], device_required: true })] }, last_run_pinned: { run_id: "full-run-id", status: "completed", tools: [entry("pinned-only")] }, note: "estimate" });
function host() {
  let auth = { token: "token-a", isAuthenticated: true, user: { user_id: "a", permissions: ["read"], roles: ["member"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  let slots: Hook[] = []; const wrapper: Hook[] = []; let child: Hook[] = []; let index = 0; let key: string | undefined; let uuid = 0;
  let effects: Array<() => void> = []; let tree: unknown; let closed = 0;
  const requests: Array<{ path: string; signal: AbortSignal; resolve: (data: unknown) => void; reject: (error: Error) => void }> = [];
  const memo = (factory: () => unknown, deps: unknown[]) => { const i = index++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: factory(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = index++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Hook = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    react: { useState(initial: unknown) { const i = index++; const owner = slots; owner[i] ??= { value: initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (_key: string, fallback: string, args?: Record<string, unknown>) => fallback.replace(/{{(\w+)}}/g, (_, name: string) => String(args?.[name])) }) },
    "@/components/ui/dialog": Object.fromEntries(["Dialog", "DialogContent", "DialogDescription", "DialogHeader", "DialogTitle"].map(name => [name, name])),
    "@/components/ui/button": { Button: "Button" }, "@/components/ui/input": { Input: "Input" },
    "@/lib/utils": { generateUUID: () => `scope-${++uuid}` },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (value: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, prior: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/api": { api: { get: (path: string, { signal }: { signal: AbortSignal }) => new Promise((resolve, reject) => requests.push({ path, signal, resolve: data => resolve({ data }), reject })) } },
  };
  function compile(path: string) { const exports: Record<string, (props: unknown) => Tree> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, AbortController, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/api/assistantTools"] = compile("../../../api/assistantTools.ts");
  const component = compile("./ToolsDialog.tsx").ToolsDialog;
  const flush = () => { const pending = effects; effects = []; pending.forEach(fn => fn()); };
  const dispose = () => { child.forEach(hook => hook.cleanup?.()); child = []; };
  return {
    requests,
    render(sessionId = "session-a", open = true) {
      slots = wrapper; index = 0; const outer = component({ sessionId, open, onClose: () => { closed += 1; } }); flush();
      if (!outer) { dispose(); key = undefined; tree = null; return tree; }
      if (outer.key !== key) { dispose(); key = outer.key; }
      slots = child; index = 0; tree = (outer.type as (props: unknown) => Tree)(outer.props); flush(); return tree;
    },
    auth(patch: Partial<typeof auth>) { const previous = auth; auth = { ...auth, ...patch }; [...subscribers].forEach(fn => fn(auth, previous)); },
    find(type: string, label?: string) { const found = nodes(tree).find(node => node.type === type && (!label || text(node).includes(label))); assert.ok(found, `${type}:${label}`); return found; },
    button(label: string) { return this.find("Button", label).props.onClick as () => void; },
    label: () => text(tree), closed: () => closed,
  };
}
const settle = async () => { await new Promise(resolve => setImmediate(resolve)); };

test("renders disjoint returned sets and unknown metadata without inferring authorization", async () => {
  const h = host(); h.render(); assert.match(h.label(), /Loading/); h.requests[0].resolve(sample()); await settle(); h.render();
  assert.match(h.label(), /catalog-only/); assert.doesNotMatch(h.label(), /dynamic-mcp|pinned-only/); assert.match(h.label(), /No tenant permission/);
  h.button("Next-run estimate")(); h.render(); assert.match(h.label(), /dynamic-mcp/); assert.match(h.label(), /write/); assert.match(h.label(), /required/); assert.match(h.label(), /Not reported/); assert.match(h.label(), /target/); assert.match(h.label(), /device_id/);
  h.button("Last pinned run")(); h.render(); assert.match(h.label(), /pinned-only/); assert.match(h.label(), /full-run-id/); assert.doesNotMatch(h.label(), /No tenant permission/);
});
test("local filter makes no reads and empty results differ from unavailable estimate and missing snapshot", async () => {
  const h = host(); h.render(); const data = sample(); data.next_turn_estimate.status = "unavailable"; h.requests[0].resolve({ ...data, last_run_pinned: null }); await settle(); h.render();
  (h.find("Input").props.onChange as (event: unknown) => void)({ target: { value: "missing" } }); h.render(); assert.match(h.label(), /Showing 0 of 1/); assert.equal(h.requests.length, 1);
  h.button("Next-run estimate")(); h.render(); assert.match(h.label(), /could not be checked/); assert.match(h.label(), /dynamic-mcp/);
  h.button("Last pinned run")(); h.render(); assert.match(h.label(), /No pinned run/);
});
test("failure ends loading and explicit retry refreshes the actual response", async () => {
  const h = host(); h.render(); h.requests[0].reject(new Error("offline")); await settle(); h.render(); assert.doesNotMatch(h.label(), /Loading/); assert.match(h.label(), /unavailable/);
  h.button("Retry")(); h.render(); assert.equal(h.requests.length, 2); h.requests[1].resolve(sample()); await settle(); h.render(); h.button("Refresh")(); h.render(); assert.equal(h.requests.length, 3); assert.match(h.label(), /Loading/); assert.doesNotMatch(h.label(), /catalog-only/);
});
test("session replacement discards prior reads and callbacks", async () => {
  const h = host(); h.render(); const close = h.find("Dialog").props.onOpenChange as (open: boolean) => void;
  h.render("session-b"); assert.equal(h.requests[0].signal.aborted, true); close(false); assert.equal(h.closed(), 0);
  h.requests[0].resolve(sample()); await settle(); h.render("session-b"); assert.doesNotMatch(h.label(), /catalog-only/);
  h.requests[1].resolve({ ...sample(), platform_catalog: [entry("new-session")] }); await settle(); h.render("session-b"); assert.match(h.label(), /new-session/);
});
test("close invalidates pending responses immediately and reopen starts a fresh read", async () => {
  const h = host(); h.render(); const close = h.find("Dialog").props.onOpenChange as (open: boolean) => void; close(false); assert.equal(h.closed(), 1);
  h.requests[0].resolve(sample()); await settle(); h.render("session-a", false); h.render(); close(false); assert.equal(h.closed(), 1); assert.equal(h.requests.length, 2); assert.doesNotMatch(h.label(), /catalog-only/);
});
test("credential A-B-A transitions fence old responses before rerender", async () => {
  const h = host(); h.render(); const close = h.find("Dialog").props.onOpenChange as (open: boolean) => void;
  h.auth({ token: "token-b" }); h.auth({ token: "token-a" }); close(false); assert.equal(h.closed(), 0); h.requests[0].resolve(sample()); await settle(); h.render(); assert.equal(h.requests.length, 2); assert.doesNotMatch(h.label(), /catalog-only/);
});
test("signout suppresses reads and permission changes replace the inspector", async () => {
  const h = host(); h.render(); h.auth({ user: { user_id: "a", permissions: [], roles: ["member"], tier: "normal" } }); h.render(); assert.equal(h.requests.length, 2);
  h.auth({ isAuthenticated: false }); assert.equal(h.render(), null); assert.equal(h.requests[1].signal.aborted, true); assert.equal(h.requests.length, 2);
});
