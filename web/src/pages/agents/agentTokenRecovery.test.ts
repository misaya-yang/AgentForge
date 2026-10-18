import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { HttpError, isHttpError } from '../../lib/fetchClient.ts';
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...["children", "description", "footer", "addonAfter"].flatMap(key => all(node.props?.[key], predicate))];
}
function text(tree: unknown): string {
  if (tree === null || tree === undefined || typeof tree === "boolean") return "";
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  const node = tree as Tree;
  return ["title", "description", "children"].map(key => text(node.props?.[key])).join(" ");
}
function deferred<T>() {
  let resolve!: (value: T) => void; let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function denied(status = 403) { return new HttpError("fixture-secret", "ERR_BAD_REQUEST", undefined, undefined, { status, data: { detail: null, token: "fixture-secret" }, statusText: "Denied", headers: {}, config: {} as never }); }
const token = { token_id: "token-a", name: "Existing token", scopes: ["chat:write"], revoked_at: null };
const prefix = "agents.studio.channels.";
function host(wrapper = false) {
  const slots: unknown[] = []; const cleanups: Array<() => void> = [];
  let cursor = 0; let effects: Array<() => void> = [];
  let current = true; let canManage = true; let uuid = 0;
  let auth = { token: "account-a", isAuthenticated: true, user: { user_id: "user-a", permissions: ["agents:read"], roles: ["owner"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  let read = async (): Promise<unknown[]> => [token];
  let write = async (): Promise<unknown> => ({ token: "fixture-secret" });
  let copy = async () => {};
  let readCalls = 0;
  const writes: unknown[][] = []; const notifications: string[] = [];
  const react = {
    useState(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (value: unknown) => { slots[i] = typeof value === "function" ? value(slots[i]) : value; }]; },
    useRef(initial: unknown) { const i = cursor++; return slots[i] ?? (slots[i] = { current: initial }); },
    useLayoutEffect(effect: () => void | (() => void), deps: unknown[]) {
      const i = cursor++; const old = slots[i] as unknown[] | undefined;
      if (!old || deps.some((value, index) => !Object.is(value, old[index]))) { slots[i] = deps; effects.push(() => { cleanups[i]?.(); cleanups[i] = effect() || (() => {}); }); }
    },
  };
  Object.assign(react, {
    useMemo(make: () => unknown, deps: unknown[]) { const i = cursor++; const old = slots[i] as { value: unknown; deps: unknown[] } | undefined; if (!old || deps.some((value, index) => !Object.is(value, old.deps[index]))) slots[i] = { value: make(), deps }; return (slots[i] as { value: unknown }).value; },
    useCallback(fn: unknown, deps: unknown[]) { const i = cursor++; const old = slots[i] as { value: unknown; deps: unknown[] } | undefined; if (!old || deps.some((value, index) => !Object.is(value, old.deps[index]))) slots[i] = { value: fn, deps }; return (slots[i] as { value: unknown }).value; },
  });
  const authStore = Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (listener: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(listener); return () => subscribers.delete(listener); } });
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const api = Object.fromEntries(["createAgentApiToken", "rotateAgentApiToken", "revokeAgentApiToken"].map(name => [name, (...args: unknown[]) => { writes.push([name, ...args]); return write(); }]));
  const modules: Record<string, unknown> = {
    react, "@/lib/fetchClient": { isHttpError }, "@/store/useAuthStore": { useAuthStore: authStore }, "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/utils": { generateUUID: () => `scope-${++uuid}` }, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { ...Object.fromEntries(["Alert", "Button", "Modal", "Tag"].map(name => [name, name])), Empty: { PRESENTED_IMAGE_SIMPLE: "empty" }, Input: { Password: "Password" }, App: { useApp: () => ({ message: { success: (value: string) => notifications.push(value) } }) } },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/api/agents": { ...api, listAgentApiTokens: () => { readCalls++; return read(); } },

  };
  const hookExports = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../../hooks/useAuthenticatedReadScope.ts", import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, { exports: hookExports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } });
  modules["@/hooks/useAuthenticatedReadScope"] = hookExports;
  const compiled: Record<string, (props: unknown) => Tree> = {};
  const source = readFileSync(new URL("./ApiTokenPanel.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source + "\nexports.Content = TokenPanelContent;", {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports: compiled, require: (id: string) => { assert.ok(id in modules, `Unexpected import ${id}`); return modules[id]; }, navigator: { clipboard: { writeText: () => copy() } } });
  const h = {
    writes, notifications,
    get readCalls() { return readCalls; },
    render() { cursor = 0; effects = []; const tree = wrapper ? compiled.ApiTokenPanel({ publication: { publication_id: "publication-a" }, canManage }) : compiled.Content({ publication: { publication_id: "publication-a" }, isCurrent: () => current }); effects.forEach(effect => effect()); return tree; },
    async flush() { h.render(); for (let i = 0; i < 10; i++) await Promise.resolve(); return h.render(); },
    button(label: string) { const match = all(h.render(), node => node.type === "Button" && (text(node).includes(prefix + label) || node.props["aria-label"] === prefix + label))[0]; assert.ok(match, `Missing button ${label}`); return match; },
    click(label: string) { const button = h.button(label); assert.equal(button.props.disabled === true, false, `${label} disabled`); return (button.props.onClick as () => unknown)(); },
    has(label: string) { return text(h.render()).includes(prefix + label); },
    name(value: string) { const input = all(h.render(), node => node.props?.placeholder === prefix + "tokenName")[0]; (input.props.onChange as (event: unknown) => void)({ target: { value } }); },
    issued() { return all(h.render(), node => node.type === "Password")[0]?.props.value; },
    read(fn: typeof read) { read = fn; }, write(fn: typeof write) { write = fn; }, copy(fn: typeof copy) { copy = fn; },
    current(value: boolean) { current = value; }, role(value: boolean) { canManage = value; }, scope(value: string) { const prior = auth; auth = { ...auth, token: value }; subscribers.forEach(fn => fn(auth, prior)); },
    authority() { const prior = auth; auth = { ...auth, user: { ...auth.user, roles: ["viewer"] } }; subscribers.forEach(fn => fn(auth, prior)); },
    unmount() { cleanups.forEach(fn => fn()); },
  };
  return h;
}

test("loading and failed token reads never appear as an empty list; read-only refresh recovers", async () => {
  const h = host(); h.read(async () => { throw new Error("fixture-secret must not leak"); });
  assert.equal(h.has("tokensLoading"), true); await h.flush();
  assert.equal(h.has("tokensLoadFailed"), true); assert.equal(h.has("noTokens"), false);
  assert.equal(text(h.render()).includes("fixture-secret"), false);
  h.read(async () => []); h.click("refreshTokens"); await h.flush();
  assert.equal(h.has("noTokens"), true); assert.equal(h.has("tokensLoadFailed"), false); assert.equal(h.writes.length, 0);
});

test("failed later read hides cached active rows and disables mutation", async () => {
  const h = host(); await h.flush(); assert.ok(text(h.render()).includes(token.name));
  h.read(async () => { throw new Error("offline"); }); h.click("refreshTokens"); await h.flush();
  assert.equal(text(h.render()).includes(token.name), false); assert.equal(h.button("createToken").props.disabled, true);
});

for (const kind of ["createToken", "rotate", "revoke"] as const) {
  test(`${kind}: permission rejection is visible, sanitized, and is not replayed by refresh`, async () => {
    const h = host(); await h.flush(); h.name("New token");
    h.write(async () => { throw denied(); });
    h.click(kind); await h.flush();
    assert.equal(h.has("tokenPermissionDenied"), true); assert.equal(h.writes.length, 1);
    assert.equal(text(h.render()).includes("fixture-secret"), false);
    h.click("refreshTokens"); await h.flush(); assert.equal(h.writes.length, 1);
    assert.equal(h.button("createToken").props.disabled, true);
  });
}

test("synchronous ownership prevents double and cross-operation dispatch until readback settles", async () => {
  const h = host(); await h.flush(); const write = deferred<unknown>(); const read = deferred<unknown[]>();
  h.write(() => write.promise); const rotate = h.button("rotate").props.onClick as () => void; const revoke = h.button("revoke").props.onClick as () => void;
  rotate(); revoke(); rotate(); assert.equal(h.writes.length, 1);
  h.read(() => read.promise); write.resolve({ token: "fixture-secret" }); await h.flush(); revoke(); assert.equal(h.writes.length, 1);
  read.resolve([token]); await h.flush(); assert.equal(h.issued(), "fixture-secret");
});

test("uncertain create stays blocked after successful readback until an explicit new decision", async () => {
  const h = host(); await h.flush(); h.name("New token"); h.write(async () => { throw new Error("connection ended"); });
  h.click("createToken"); await h.flush(); assert.equal(h.has("tokenOperationFailed"), true);
  h.click("refreshTokens"); await h.flush(); assert.equal(h.writes.length, 1);
  assert.equal(h.button("createToken").props.disabled, true);
  h.click("tokenNewDecision"); assert.equal(h.has("tokenOperationFailed"), false); assert.equal(h.writes.length, 1);
  assert.equal(h.button("createToken").props.disabled, true); // Cleared name requires a new intent.
});

test("successful write with failed readback preserves one-time secret and reports accepted state", async () => {
  const h = host(); await h.flush(); h.name("New token"); h.read(async () => { throw new Error("offline"); });
  h.click("createToken"); await h.flush(); assert.equal(h.has("tokenWriteConfirmed"), true);
  assert.equal(h.issued(), "fixture-secret"); assert.equal(h.has("tokensLoadFailed"), true);
  h.click("copied"); h.read(async () => [token]); h.click("refreshTokens"); await h.flush();
  assert.equal(h.writes.length, 1); assert.equal(h.button("rotate").props.disabled, true);
});

for (const change of ["unmount", "authority"] as const) {
  test(`late write after ${change} cannot display secret or issue readback`, async () => {
    const h = host(); await h.flush(); h.name("New token"); const pending = deferred<unknown>(); h.write(() => pending.promise);
    h.click("createToken"); if (change === "unmount") h.unmount(); else h.current(false);
    const reads = h.readCalls; pending.resolve({ token: "fixture-secret" }); await h.flush();
    assert.equal(h.issued(), ""); assert.equal(h.readCalls, reads);
  });
}

test("permission-denied readback clears a returned secret", async () => {
  const h = host(); await h.flush(); h.name("New token"); h.read(async () => { throw denied(404); });
  h.click("createToken"); await h.flush(); assert.equal(h.issued(), "");
});

test("clipboard rejection never claims success and permits manual copying", async () => {
  const h = host(); await h.flush(); h.name("New token"); h.click("createToken"); await h.flush();
  h.copy(async () => { throw new Error("clipboard unavailable"); });
  const copy = all(h.render(), node => node.type === "Button" && node.props.type === "text")[0];
  await (copy.props.onClick as () => Promise<void>)();
  assert.equal(h.notifications.length, 0); assert.equal(h.has("tokenCopyFailed"), true); assert.equal(h.issued(), "fixture-secret");
});

test("wrapper removes owner controls immediately and fences retained callbacks", () => {
  const h = host(true); const before = h.render(); const current = before.props.isCurrent as () => boolean;
  assert.equal(current(), true); h.role(false); assert.equal(h.render(), null); assert.equal(current(), false);
  h.role(true); h.scope("scope-b"); assert.notEqual(h.render().key, before.key);
  h.authority(); assert.equal(current(), false);
});

for (const failure of [null, new Error("fixture-secret"), denied(500)]) {
  test(`unstructured/nullable error stays visible and triggers readback: ${String(failure)}`, async () => {
    const h = host(); await h.flush(); h.name("New token"); h.write(async () => { throw failure; });
    h.click("createToken"); await h.flush(); assert.equal(h.has("tokenOperationFailed"), true); assert.equal(h.readCalls, 2);
  });
}

test("actual authenticated scope fences credential A→B→A before rerender", () => {
  const h = host(true); const before = h.render(); const isCurrent = before.props.isCurrent as () => boolean;
  h.scope("account-b"); h.scope("account-a"); assert.equal(isCurrent(), false);
  assert.notEqual(h.render().key, before.key);
});
