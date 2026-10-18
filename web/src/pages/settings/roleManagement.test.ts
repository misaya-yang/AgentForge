import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import fetchHttp, { type FetchClient } from '../../lib/fetchClient.ts';

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Frame = { slots: unknown[]; effects: (() => void)[]; cleanup: (() => void)[] };
const role = (overrides: Record<string, unknown> = {}) => ({ role_name: "reader_team", description: "Read team resources", permissions: ["service:view"], is_system: false, ...overrides });
const permission = { permission_code: "service:view", description: "View services", name: "View", category: "service" };
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(yes => { resolve = yes; }); return { resolve, promise }; }
async function settle() { for (let i = 0; i < 60; i++) await Promise.resolve(); }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
/** Complete production page, production Roles and Users adapters; controlled React/HTTP only. */
function host() {
  const frames = new Map<string, Frame>(); let frame!: Frame; let cursor = 0; let dirty = false; let seen = new Set<string>();
  let live: ReturnType<typeof role> | null = null; let codes = [permission];
  const requests: { method: string; path: string; body?: unknown }[] = [];
  let read = async (path: string): Promise<unknown> => {
    if (path === "/api/v1/roles") return { roles: [], total: 0 };
    if (path === "/api/v1/roles/permissions") return { permissions: codes, total: codes.length };
    if (live) return structuredClone(live);
    throw { isHttpError: true, response: { status: 404 } };
  };
  let write = async (_path: string, body: Record<string, unknown>): Promise<unknown> => { live = role(body); return live; };
  const auth = { token: "synthetic-a", user: { user_id: "operator", roles: ["platform_admin"], permissions: ["role:list", "role:create"], tier: "normal" }, hasPermission: (code: string) => auth.user.permissions.includes(code) };
  let serial = 0;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const react = {
    useState(initial: unknown) { const owner = frame; const index = cursor++; if (!(index in owner.slots)) owner.slots[index] = typeof initial === "function" ? initial() : initial;
      return [owner.slots[index], (next: unknown) => { const value = typeof next === "function" ? next(owner.slots[index]) : next; if (!Object.is(value, owner.slots[index])) { owner.slots[index] = value; dirty = true; } }]; },
    useRef(initial: unknown) { const index = cursor++; return frame.slots[index] ?? (frame.slots[index] = { current: initial }); },
    useMemo(compute: () => unknown, deps: unknown[]) { const index = cursor++; const old = frame.slots[index] as { deps?: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) frame.slots[index] = { deps, value: compute() }; return (frame.slots[index] as { value: unknown }).value; },
    useLayoutEffect(run: () => void | (() => void), deps: unknown[]) { const owner = frame; const index = cursor++;
      if (!same(owner.slots[index] as unknown[], deps)) { owner.slots[index] = deps; owner.effects.push(() => { owner.cleanup[index]?.(); owner.cleanup[index] = run() || (() => {}); }); } },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    "@/lib/fetchClient": { isHttpError: (error: { isHttpError?: boolean }) => error?.isHttpError === true },
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-router-dom": { Link: "Link" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth }) },
    "@/lib/utils": { generateUUID: () => `scope-${++serial}` },
    "@/lib/api": { api: { defaults: { transformRequest: [] },
      async get(path: string) { requests.push({ method: "GET", path }); return { data: await read(path) }; },
      async post(path: string, body: Record<string, unknown>) { requests.push({ method: "POST", path, body: structuredClone(body) }); return { data: await write(path, body) }; },
    } },
  };
  function load(filename: string): Record<string, (props: Record<string, unknown>) => Tree> {
    const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(filename, import.meta.url), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => { if (name.startsWith("@/components/ui/")) return new Proxy({}, { get: (_target, key) => String(key) }); assert.ok(name in modules, `Unexpected import ${name}`); return modules[name]; }, Error,
    }); return exports;
  }
  modules["../lib/api"] = modules["@/lib/api"];
  modules["@/api/users"] = load("../../api/users.ts");
  modules["@/api/roles"] = load("../../api/roles.ts");
  modules["@/lib/sdkAuthorityScope"] = load("../../lib/sdkAuthorityScope.ts");
  const page = load("./RoleManagement.tsx").RoleManagementPage;
  function renderNode(node: unknown, path: string): unknown {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((value, index) => renderNode(value, `${path}:${index}`));
    const value = node as Tree;
    if (typeof value.type === "function") {
      const key = `${path}:${value.type.name}:${value.key ?? ""}`; seen.add(key);
      frame = frames.get(key) ?? { slots: [], effects: [], cleanup: [] }; frames.set(key, frame); cursor = 0;
      return renderNode(value.type(value.props), key);
    }
    return { ...value, props: { ...value.props, children: renderNode(value.props?.children, `${path}:children`) } };
  }
  function render() { for (let turn = 0; turn < 15; turn++) { dirty = false; seen = new Set(); const tree = renderNode({ type: page, props: {} }, "root");
    for (const [key, owner] of frames) { if (!seen.has(key)) { owner.cleanup.forEach(cleanup => cleanup?.()); frames.delete(key); } else owner.effects.splice(0).forEach(effect => effect()); }
    if (!dirty) return tree;
  } throw new Error("Render loop"); }
  const nodes = (predicate: (node: Tree) => boolean) => all(render(), predicate);
  const button = (suffix: string) => { const found = nodes(node => node.type === "Button" && node.props.children === `roleManagement.${suffix}`)[0]; assert.ok(found, suffix); return found; };
  return { auth, requests, render, button,
    posts: () => requests.filter(request => request.method === "POST"),
    async start() { render(); await settle(); render(); },
    async click(suffix: string) { const item = button(suffix); assert.equal(Boolean(item.props.disabled), false); (item.props.onClick as () => void)(); await settle(); render(); },
    edit(name = "reader_team") { for (const [index, value] of [name, "Read team resources"].entries()) (nodes(node => node.type === "Input")[index].props.onChange as (event: unknown) => void)({ target: { value } }); render(); },
    select() { (nodes(node => node.type === "input")[0].props.onChange as (event: unknown) => void)({ target: { checked: true } }); render(); },
    submit() { (nodes(node => node.type === "form")[0].props.onSubmit as (event: unknown) => void)({ preventDefault() {} }); render(); },
    has(suffix: string) { return nodes(node => node.props?.children === `roleManagement.${suffix}`).length > 0; },
    links() { return nodes(node => node.type === "Link").map(node => node.props.to); },
    setLive(value: typeof live) { live = value; }, setCodes(value: typeof codes) { codes = value; }, setRead(value: typeof read) { read = value; }, setWrite(value: typeof write) { write = value; },
    dispose() { for (const owner of frames.values()) owner.cleanup.forEach(cleanup => cleanup?.()); },
  };
}
async function reviewed(h: ReturnType<typeof host>) { await h.start(); h.edit(); h.select(); h.submit(); }

test("catalog → explicit review → one create → exact readback → existing assignment link", async () => {
  const h = host(); try { await reviewed(h); assert.equal(h.posts().length, 0); assert.ok(h.has("scope")); await h.click("confirm");
    assert.equal(h.posts().length, 1); assert.equal(h.posts()[0].path, "/api/v1/roles"); assert.deepEqual(h.posts()[0].body, { role_name: "reader_team", description: "Read team resources", permissions: ["service:view"] });
    assert.ok(h.has("confirmed")); assert.ok(h.links().includes("/users")); await h.click("reconcile"); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("tenant administrators can read but cannot create shared roles", async () => {
  const h = host(); try { h.auth.user.roles = ["admin"]; await h.start(); assert.ok(h.has("readOnly")); assert.ok(!h.has("createTitle")); assert.equal(h.posts().length, 0); } finally { h.dispose(); }
});
test("missing role:list avoids all catalog requests", async () => {
  const h = host(); try { h.auth.user.permissions = []; await h.start(); assert.ok(h.has("denied")); assert.equal(h.requests.length, 0); } finally { h.dispose(); }
});
test("reserved permissions name explains exact-role routing limitation without writes", async () => {
  const h = host(); try { await h.start(); h.edit("permissions"); h.submit(); assert.ok(h.has("reserved")); assert.equal(h.posts().length, 0); } finally { h.dispose(); }
});
test("existing exact name and removed selected permission stop before dispatch", async () => {
  for (const existing of [true, false]) { const h = host(); try { await reviewed(h); if (existing) h.setLive(role()); else h.setCodes([]); await h.click("confirm");
    assert.ok(h.has(existing ? "exists" : "catalogChanged")); assert.equal(h.posts().length, 0);
  } finally { h.dispose(); } }
});
test("unknown create with matching metadata never claims creator ownership or retries", async () => {
  const h = host(); try { await reviewed(h); h.setWrite(async (_path, body) => { h.setLive(role(body)); throw new Error("lost acknowledgement"); }); await h.click("confirm");
    assert.ok(h.has("matchesUnknown")); assert.ok(!h.has("confirmed")); await h.click("reconcile"); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("post-submit absence and mismatched readback stay locked to GET recovery", async () => {
  for (const absent of [true, false]) { const h = host(); try { await reviewed(h); h.setWrite(async () => { h.setLive(absent ? null : role({ permissions: [] })); throw new Error("uncertain"); }); await h.click("confirm");
    assert.ok(h.has(absent ? "absentUnknown" : "differs")); await h.click("reconcile"); assert.equal(h.posts().length, 1); assert.ok(!h.has("confirm"));
  } finally { h.dispose(); } }
});
test("accepted create with failed readback never becomes confirmed", async () => {
  const h = host(); try { await reviewed(h); h.setWrite(async () => { h.setRead(async () => { throw new Error("offline"); }); return role(); }); await h.click("confirm");
    assert.ok(h.has("readbackFailed")); assert.ok(!h.has("confirmed")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("double confirm dispatches only once while the write is pending", async () => {
  const h = host(); const pending = deferred<unknown>(); try { await reviewed(h); h.setWrite(async (_path, body) => { h.setLive(role(body)); return pending.promise; });
    const callback = h.button("confirm").props.onClick as () => void; callback(); callback(); await settle(); assert.equal(h.posts().length, 1);
    pending.resolve(role()); await settle(); h.render(); assert.ok(h.has("confirmed")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("account, credential and authority changes fence late successful writes", async () => {
  for (const change of ["account", "credential", "permission", "tier", "role"]) { const h = host(); const pending = deferred<unknown>(); try {
    await reviewed(h); h.setWrite(async () => pending.promise); const action = h.click("confirm"); await settle();
    if (change === "account") h.auth.user.user_id = "another";
    if (change === "credential") h.auth.token = "synthetic-b";
    if (change === "permission") h.auth.user.permissions = ["role:list"];
    if (change === "tier") h.auth.user.tier = "changed";
    if (change === "role") h.auth.user.roles = ["admin"];
    h.render(); pending.resolve(role()); await action; assert.ok(!h.has("confirmed")); assert.ok(!h.has("submitted")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); } }
});
test("unmount fences post-submit readback without stopping an already dispatched write", async () => {
  const h = host(); const pending = deferred<unknown>(); await reviewed(h); h.setWrite(async () => pending.promise);
  const action = h.click("confirm"); await settle(); const count = h.requests.length; h.dispose(); pending.resolve(role()); await action;
  assert.equal(h.requests.length, count); assert.equal(h.posts().length, 1);
});
test("authority changes during preflight and stale callbacks never dispatch", async () => {
  for (const change of ["account", "credential", "role", "unmount"]) { const h = host(); const pending = deferred<unknown>(); try {
    await reviewed(h); h.setRead(async path => path.endsWith("/permissions") ? pending.promise : Promise.reject({ isHttpError: true, response: { status: 404 } }));
    const callback = h.button("confirm").props.onClick as () => void; callback(); await settle();
    if (change === "account") h.auth.user.user_id = "another";
    if (change === "credential") h.auth.token = "synthetic-b";
    if (change === "role") h.auth.user.roles = ["admin"];
    if (change === "unmount") h.dispose(); else h.render();
    callback(); pending.resolve({ permissions: [permission] }); await settle(); assert.equal(h.posts().length, 0);
  } finally { h.dispose(); } }
});

test("actual shared Fetch auth interceptor cannot dispatch old review with replacement identity", async () => {
  for (const change of ["none", "token", "account", "role", "permission", "unmount"]) {
    const auth = createStore(() => ({ csrfToken: "synthetic-csrf", isAuthenticated: true, token: "synthetic-a", user: { user_id: "operator", roles: ["platform_admin"], permissions: ["role:list", "role:create"], tier: "normal" } }));
    let dispose: (() => void) | undefined;
    let active = true;
    const scope = () => JSON.stringify([auth.getState().token, auth.getState().user]); const captured = scope();
    const modules: Record<string, unknown> = {
      "@/lib/fetchClient": fetchHttp, "@/observability/browserTelemetry": { browserTraceparent: () => undefined },
    "@/config/runtime": { getApiBaseUrlConfig: () => "https://console.example" },
      "@/hooks/use-toast": { toast: { warning() {} } }, "@/i18n": { default: { t: (key: string) => key } },
      "@/store/useAuthStore": { useAuthStore: auth },
    };
    const load = (file: string): Record<string, unknown> => {
      const exports: Record<string, unknown> = {};
      vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
        exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, Error, URL, window: { location: { origin: "https://console.example", pathname: "/settings", href: "" } }, __hot: { dispose(stop: () => void) { dispose = stop; } },
      }); return exports;
    };
    modules["@/lib/sdkAuthorityScope"] = load("../../lib/sdkAuthorityScope.ts");
    const shared = load("../../lib/api.ts"); modules["@/lib/api"] = shared;
    try {
      const realApi = shared.api as FetchClient; const calls: { body: unknown; auth: unknown; csrf: unknown; cookies: unknown }[] = [];
      // A later optional async interceptor must not weaken this request-local fence.
      realApi.interceptors.request.use(async config => config);
      realApi.defaults.adapter = async config => { calls.push({ body: config.data, auth: config.headers.Authorization, csrf: config.headers["X-CSRF-Token"], cookies: config.withCredentials }); return { data: role(), status: 200, statusText: "OK", headers: {}, config }; };
      const production = load("../../api/roles.ts");
      const create = production.createRole as (input: unknown, mayDispatch: () => boolean) => Promise<void>;
      const pending = create({ role_name: "reader_team", description: "Read team resources", permissions: ["service:view"] }, () => active && captured === scope());
      if (change === "token") auth.setState({ token: "synthetic-b" });
      if (change === "account") auth.setState({ user: { ...auth.getState().user, user_id: "another" } });
      if (change === "role") auth.setState({ user: { ...auth.getState().user, roles: ["admin"] } });
      if (change === "permission") auth.setState({ user: { ...auth.getState().user, permissions: ["role:list"] } });
      if (change === "unmount") active = false;
      if (change === "none") { await pending; assert.equal(calls.length, 1); assert.equal(calls[0].auth, undefined); assert.equal(calls[0].csrf, "synthetic-csrf"); assert.equal(calls[0].cookies, true); assert.deepEqual(JSON.parse(calls[0].body as string), { role_name: "reader_team", description: "Read team resources", permissions: ["service:view"] }); }
      else { await assert.rejects(pending, /role_creation_authority_changed/); assert.equal(calls.length, 0); }
    } finally { assert.ok(dispose); dispose(); }
  }
});
