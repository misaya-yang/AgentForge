/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled React scheduling and HTTP collaborators. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { AxiosError, isAxiosError } from "axios";
import { pluginLifecycleImpact, pluginLifecycleReviewKey } from "./pluginLifecycleImpact.ts";
import type { PluginRegistry } from "../../api/plugins.ts";

// Complete production page, controls, API, recovery hook and auth read scope.
// Real query observers; React keyed presence/effects and HTTP are controlled.
// This is offline regression evidence, not browser or native server acceptance.
type Tree = { type: any; props: Record<string, any>; key?: string };
type Frame = { slots: any[]; effects: (() => void)[]; cleanup: (() => void)[] };
function fixture(): PluginRegistry {
  return { schema_version: "ai-platform-plugin-registry/v1", catalog_revision: 12,
    catalog_revision_token: "12", tenant_id: "tenant-a", can_manage: true, package_installation_supported: false,
    plugins: ["plugin-a", "plugin-b"].map(id => ({ id, lifecycle: { managed: true, enabled: true,
      selected_version: "v1", revision: "9", selected_catalog_revision: "12", selection_status: "current", updated_at: null },
    versions: ["v1", "v2"].map(version => ({ version, availability: version === "v1" ? "available" : "policy_blocked",
      required_permissions: [], declared_permissions: [], capabilities: [{ id: `${id}-${version}`, name: `${id}-${version}`,
        version: "1", schema_hash: "sha256:fixture", category: "data", risk_level: "low", read_only: true,
        required_permissions: [], declared_permissions: [], available: version === "v1" }] })) })) };
}
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
async function settle() { for (let i = 0; i < 100; i++) await Promise.resolve(); }
function nodes(value: any, predicate: (node: Tree) => boolean = () => true): Tree[] {
  if (!value || typeof value !== "object") return []; if (Array.isArray(value)) return value.flatMap(child => nodes(child, predicate));
  return [...(predicate(value) ? [value] : []), ...nodes(value.props?.children, predicate)];
}
function host() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } }); client.mount();
  const frames = new Map<string, Frame>(); const subscriptions: (() => void)[] = []; let frame!: Frame; let cursor = 0;
  let dirty = false; let seen = new Set<string>(); let tree: Tree; let live = fixture(); let mounted = true;
  let read: () => Promise<PluginRegistry> = async () => structuredClone(live);
  let write: (id: string, body: any) => Promise<any> = async (id, body) => {
    const plugin = live.plugins.find(row => row.id === id)!;
    plugin.lifecycle = { ...plugin.lifecycle, selected_version: body.selected_version, enabled: body.enabled, revision: "10" };
    return { plugin_id: id, tenant_id: live.tenant_id, lifecycle: structuredClone(plugin.lifecycle) };
  };
  const reads: string[] = []; const writes: { id: string; body: any }[] = []; const invalidations: unknown[] = []; const queryKeys: unknown[][] = [];
  const initialAuth = { isAuthenticated: true, token: "fixture-token-a", user: { user_id: "admin-a", roles: ["admin"], permissions: [] as string[], tier: "admin" } };
  const auth = createStore(() => initialAuth); let identity = 0;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((x, i) => Object.is(x, b?.[i]));
  const react = {
    useState(initial: any) { const owner = frame; const index = cursor++; if (!(index in owner.slots)) owner.slots[index] = typeof initial === "function" ? initial() : initial;
      return [owner.slots[index], (next: any) => { const value = typeof next === "function" ? next(owner.slots[index]) : next;
        if (!Object.is(value, owner.slots[index])) { owner.slots[index] = value; dirty = true; } }]; },
    useRef(initial: any) { const index = cursor++; return frame.slots[index] ?? (frame.slots[index] = { current: initial }); },
    useMemo(compute: () => any, deps: unknown[]) { const index = cursor++; const old = frame.slots[index];
      if (!old || !same(old.deps, deps)) frame.slots[index] = { deps, value: compute() }; return frame.slots[index].value; },
    useCallback(callback: () => any, deps: unknown[]) { return react.useMemo(() => callback, deps); },
    useEffect(run: () => void | (() => void), deps: unknown[]) { const owner = frame; const index = cursor++;
      if (!same(owner.slots[index], deps)) { owner.slots[index] = deps; owner.effects.push(() => { owner.cleanup[index]?.(); owner.cleanup[index] = run() || (() => {}); }); } },
  };
  const jsx = (type: any, props: any, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, name) => String(name) });
  const authority = (user: any) => JSON.stringify([user?.user_id, user?.roles, user?.permissions, user?.tier]);
  const invalidate = client.invalidateQueries.bind(client);
  client.invalidateQueries = ((options: any) => { invalidations.push(options); return invalidate(options); }) as typeof client.invalidateQueries;
  const modules: Record<string, any> = {
    react: { ...react, useLayoutEffect: react.useEffect }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "Link" }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    axios: { isAxiosError }, "lucide-react": named,
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: any) => select(auth.getState()), auth) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++identity}` },
    "./pluginLifecycleImpact": { pluginLifecycleImpact, pluginLifecycleReviewKey },
    "@/components/ManagementQueryState": { ManagementQueryState: "ManagementQueryState" },
    "@/lib/api": { api: {
      async get(path: string) { assert.equal(path, "/api/v1/assistant/plugins"); reads.push(path); return { data: await read() }; },
      async put(path: string, body: any) { const match = path.match(/^\/api\/v1\/assistant\/plugins\/([^/]+)\/lifecycle$/); assert.ok(match);
        const id = decodeURIComponent(match[1]); writes.push({ id, body: structuredClone(body) }); return { data: await write(id, body) }; },
    } },
    "@tanstack/react-query": { useQueryClient: () => client, useQuery(options: any) {
      const index = cursor++; let observer = frame.slots[index] as QueryObserver | undefined;
      if (!observer) { observer = new QueryObserver(client, options); frame.slots[index] = observer;
        const stop = observer.subscribe(() => { dirty = true; }); subscriptions.push(stop); frame.cleanup[index] = stop; }
      observer.setOptions(options); queryKeys.push(options.queryKey); return observer.getCurrentResult();
    } },
  };
  const loaded: Record<string, any> = {};
  const files: Record<string, string> = { "@/api/plugins": "../../api/plugins.ts", "./PluginLifecycleControls": "./PluginLifecycleControls.tsx",
    "./PluginImpactReview": "./PluginImpactReview.tsx", "./usePluginLifecycleRecovery": "./usePluginLifecycleRecovery.ts",
    "@/hooks/useAuthenticatedReadScope": "../../hooks/useAuthenticatedReadScope.ts" };
  function load(filename: string): any {
    if (loaded[filename]) return loaded[filename]; const exports: any = {}; loaded[filename] = exports;
    const code = ts.transpileModule(readFileSync(new URL(filename, import.meta.url), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
    vm.runInNewContext(code, { exports, require: (name: string) => {
      if (files[name]) return load(files[name]); if (name.startsWith("@/components/ui/")) return named;
      assert.ok(name in modules, `Unexpected import ${name}`); return modules[name]; }, Error, Date, console }); return exports;
  }
  const page = load("./PluginsSettings.tsx").PluginsSettingsPage;
  function renderNode(node: any, path: string): any {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((child, index) => renderNode(child, `${path}:${child?.key != null ? `key=${child.key}` : index}`));
    const key = `${path}:${node.type?.name ?? node.type}:${node.key ?? ""}`;
    if (typeof node.type === "function") { seen.add(key); frame = frames.get(key) ?? { slots: [], effects: [], cleanup: [] };
      frames.set(key, frame); cursor = 0; return renderNode(node.type(node.props), key); }
    return { ...node, props: { ...node.props, children: renderNode(node.props?.children, `${key}:children`) } };
  }
  function render() { for (let i = 0; i < 20; i++) { dirty = false; seen = new Set(); tree = mounted ? renderNode({ type: page, props: {} }, "root") : null;
    for (const [key, owner] of frames) { if (!seen.has(key)) { owner.cleanup.forEach(stop => stop?.()); frames.delete(key); } else owner.effects.splice(0).forEach(run => run()); }
    if (!dirty) return tree; } throw new Error("Render loop"); }
  function one(predicate: (node: Tree) => boolean, root = render()): Tree { const result = nodes(root, predicate)[0]; assert.ok(result, "Missing fixture element"); return result; }
  const card = (id = "plugin-a") => one(node => node.type === "article" && nodes(node, child => child.type === "h2" && child.props.children === id).length > 0);
  const select = (id?: string) => one(node => node.type === "select", card(id));
  const button = (suffix: string, id?: string) => one(node => node.type === "Button" && node.props.children === `pluginRegistry.lifecycle.${suffix}`, card(id));
  const act = (node: Tree, event = "onClick", payload?: any) => { assert.equal(Boolean(node.props.disabled), false, "Control disabled"); node.props[event](payload); render(); };
  return { reads, writes, invalidations, queryKeys, render, card, select, button, setAuth: auth.setState,
    async start() { render(); await settle(); render(); }, async flush() { await settle(); render(); },
    setRead(next: typeof read) { read = next; }, setWrite(next: typeof write) { write = next; }, setLive(next: PluginRegistry) { live = next; }, getLive: () => structuredClone(live),
    filter(value: string) { one(node => node.type === "Input").props.onChange({ target: { value } }); render(); },
    filterAvailability(value: string) { one(node => node.type === "select" && node.props["aria-label"] === "pluginRegistry.filter").props.onChange({ target: { value } }); render(); },
    choose(value = "v2", id?: string) { act(select(id), "onChange", { target: { value } }); },
    review(id?: string) { one(node => node.type === "form", card(id)).props.onSubmit({ preventDefault() {} }); render(); },
    acknowledge(id?: string, value = true) { const impact = one(node => node.type === "section" && node.props["aria-label"] === "pluginRegistry.impact.title", card(id));
      act(one(node => node.type === "input", impact), "onChange", { target: { checked: value } }); },
    prepare(id?: string) { this.choose("v2", id); this.review(id); this.acknowledge(id); },
    save(id?: string) { act(button("save", id)); },
    cancel(id?: string) { act(one(node => node.type === "Button" && node.props.children === "common.cancel", card(id))); },
    refresh() { one(node => node.type === "Button" && nodes(node.props.children).length > 0 && JSON.stringify(node.props.children).includes("management.refresh")).props.onClick(); render(); },
    async cancelRead() { await client.cancelQueries({ queryKey: ["plugin-registry"] }); await settle(); render(); },
    async background() { const promise = client.refetchQueries({ queryKey: ["plugin-registry"] }); render(); await promise; await settle(); render(); },
    has(suffix: string, id?: string) { return nodes(card(id), node => node.props.children === `pluginRegistry.lifecycle.${suffix}`).length > 0; },
    dispose() { mounted = false; render(); subscriptions.forEach(stop => stop()); client.unmount(); client.clear(); },
  };
}

for (const filter of ["search", "availability"] as const) test(`pending lifecycle write survives ${filter} card unmount without another PUT`, async () => {
  const h = host(); const pending = deferred<any>(); try { await h.start(); h.setWrite(() => pending.promise); h.prepare(); const oldSave = h.button("save").props.onClick; h.save();
    if (filter === "search") { h.filter("no-match"); h.filter(""); } else { h.filterAvailability("partially_available"); h.filterAvailability("all"); }
    assert.equal(h.select().props.disabled, true); oldSave(); await h.flush(); assert.equal(h.writes.length, 1);
  } finally { pending.reject(new Error("controlled completion")); await h.flush(); h.dispose(); }
});
for (const status of [403, 409, 503]) test(`Axios ${status} outcome survives filtering and requires post-settlement reread`, async () => {
  const h = host(); try { await h.start(); h.setWrite(async () => { const error = new AxiosError("fixture rejection"); error.response = { status, statusText: "controlled", data: {}, headers: {}, config: {} as any }; throw error; });
    h.prepare(); h.save(); await h.flush(); h.filter("no-match"); h.filter("");
    assert.equal(h.select().props.disabled, true); assert.equal(h.has(status === 403 ? "forbidden" : status === 409 ? "conflict" : "uncertain"), true); assert.equal(h.writes.length, 1); assert.equal(h.reads.length, 1);
    h.refresh(); await h.flush(); assert.equal(h.select().props.disabled, false); h.choose(); h.review(); assert.equal(h.button("save").props.disabled, true);
    h.acknowledge(); h.save(); await h.flush(); assert.equal(h.writes.length, 2);
    assert.deepEqual(h.writes[1].body, { selected_version: "v2", enabled: true, expected_revision: "9", expected_catalog_revision: "12", expected_tenant_id: "tenant-a" });
  } finally { h.dispose(); }
});
test("confirmed write with failed reread remains saved and locked after filtering", async () => {
  const h = host(); try { await h.start(); h.setWrite(async () => { h.setRead(async () => { throw new Error("GET unavailable"); }); return {}; });
    h.prepare(); h.save(); await h.flush(); h.filter("no-match"); h.filter("");
    assert.equal(h.has("savedRefreshFailed"), true); assert.equal(h.select().props.disabled, true); assert.equal(h.writes.length, 1);
    h.refresh(); await h.flush(); assert.equal(h.has("savedRefreshFailed"), true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("registry read begun during pending PUT cannot release its later uncertain outcome", async () => {
  const h = host(); const put = deferred<any>(); const get = deferred<PluginRegistry>(); try { await h.start(); h.setWrite(() => put.promise); h.prepare(); h.save();
    h.setRead(() => get.promise); h.refresh(); put.reject(new Error("unknown outcome")); await h.flush(); get.resolve(h.getLive()); await h.flush();
    assert.equal(h.select().props.disabled, true); assert.equal(h.has("uncertain"), true); assert.equal(h.writes.length, 1);
    h.setRead(async () => h.getLive()); h.refresh(); await h.flush(); assert.equal(h.select().props.disabled, false);
  } finally { h.dispose(); }
});
test("background registry remount preserves pending ownership and late failure", async () => {
  const h = host(); const put = deferred<any>(); try { await h.start(); h.setWrite(() => put.promise); h.prepare(); h.save();
    const changed = h.getLive(); changed.catalog_revision_token = "13"; changed.catalog_revision = 13; h.setLive(changed); await h.background();
    assert.equal(h.select().props.disabled, true); put.reject(new Error("unknown outcome")); await h.flush(); assert.equal(h.has("uncertain"), true);
    assert.equal(h.select().props.disabled, true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("one plugin readback cannot release another pending or later-unknown attempt", async () => {
  const h = host(); const first = deferred<any>(); const second = deferred<any>(); try { await h.start(); h.setWrite(id => id === "plugin-a" ? first.promise : second.promise);
    h.prepare("plugin-a"); h.prepare("plugin-b"); h.save("plugin-a"); h.save("plugin-b"); first.resolve({}); await h.flush();
    assert.equal(h.select("plugin-a").props.disabled, false); assert.equal(h.select("plugin-b").props.disabled, true);
    second.reject(new Error("second outcome unknown")); await h.flush(); h.filter("plugin-b"); assert.equal(h.has("uncertain", "plugin-b"), true);
    h.filter(""); assert.equal(h.select("plugin-a").props.disabled, false); assert.equal(h.select("plugin-b").props.disabled, true); assert.equal(h.writes.length, 2);
  } finally { h.dispose(); }
});
for (const change of ["token-aba", "authority", "user"] as const) test(`retained lifecycle handlers refuse ${change} before rerender`, async () => {
  const h = host(); try { await h.start(); h.prepare(); const saved = h.button("save").props.onClick;
    if (change === "token-aba") { h.setAuth({ token: "fixture-token-b" }); h.setAuth({ token: "fixture-token-a" }); }
    else h.setAuth({ user: { user_id: change === "user" ? "admin-b" : "admin-a", roles: ["reader"], permissions: [], tier: "normal" } });
    saved(); await h.flush(); assert.equal(h.writes.length, 0); assert.equal(h.queryKeys.some(key => JSON.stringify(key).includes("fixture-token")), false);
  } finally { h.dispose(); }
});
test("old-session successful PUT cannot refresh or invalidate the replacement scope", async () => {
  const h = host(); const put = deferred<any>(); try { await h.start(); h.setWrite(() => put.promise); h.prepare(); h.save();
    h.setAuth({ token: "fixture-token-b" }); await h.flush(); const reads = h.reads.length; put.resolve({}); await h.flush();
    assert.equal(h.reads.length, reads); assert.equal(h.invalidations.length, 0); assert.equal(h.select().props.disabled, false);
  } finally { h.dispose(); }
});
test("retained reviewed handler cannot submit after registry descriptor or revision changes", async () => {
  const h = host(); try { await h.start(); h.prepare(); const saved = h.button("save").props.onClick;
    const changed = h.getLive(); changed.plugins[0].lifecycle.revision = "10"; changed.plugins[0].versions[1].capabilities[0].risk_level = "high";
    h.setLive(changed); await h.background(); saved(); await h.flush(); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});

test("superseded explicit reread waits for fresh replacement data before unlocking", async () => {
  const h = host(); const first = deferred<PluginRegistry>(); const second = deferred<PluginRegistry>();
  try { await h.start(); h.setWrite(async () => { throw new Error("unknown outcome"); }); h.prepare(); h.save(); await h.flush();
    h.setRead(() => first.promise); h.refresh(); h.setRead(() => second.promise); const background = h.background(); await h.flush();
    assert.equal(h.select().props.disabled, true); assert.equal(h.has("uncertain"), true);
    second.resolve(h.getLive()); await background; first.resolve(h.getLive()); await h.flush();
    assert.equal(h.select().props.disabled, false); h.choose(); h.review(); assert.equal(h.button("save").props.disabled, true);
    assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});

for (const reason of ["filter", "refresh", "uncertain-then-refresh"] as const) test(`retired reviewed callback cannot bypass fresh acknowledgement after ${reason}`, async () => {
  const h = host(); try { await h.start(); h.prepare(); const saved = h.button("save").props.onClick;
    if (reason === "uncertain-then-refresh") { h.setWrite(async () => { throw new Error("unknown outcome"); }); h.save(); await h.flush(); }
    const before = h.writes.length;
    if (reason === "filter") { h.filter("no-match"); h.filter(""); } else { h.refresh(); await h.flush(); }
    saved(); await h.flush(); assert.equal(h.writes.length, before);
    h.prepare(); assert.equal(h.button("save").props.disabled, false);
  } finally { h.dispose(); }
});
test("retained reviewed callback cannot submit against failed registry observation", async () => {
  const h = host(); try { await h.start(); h.prepare(); const saved = h.button("save").props.onClick;
    h.setRead(async () => { throw new Error("registry unavailable"); }); await h.background(); saved(); await h.flush();
    assert.equal(h.writes.length, 0); assert.equal(h.select().props.disabled, true);
  } finally { h.dispose(); }
});

for (const reason of ["cancel", "uncheck"] as const) test(`obsolete reviewed callback cannot reuse consent after ${reason}`, async () => {
  const h = host(); try { await h.start(); h.prepare(); const saved = h.button("save").props.onClick;
    if (reason === "cancel") { h.cancel(); h.choose("v1"); h.choose("v2"); h.review(); } else h.acknowledge(undefined, false);
    saved(); await h.flush(); assert.equal(h.writes.length, 0); assert.equal(h.button("save").props.disabled, true);
    h.acknowledge(); h.save(); await h.flush(); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});

test("cancelled explicit reread cannot use unchanged cached data to unlock an uncertain attempt", async () => {
  const h = host(); const get = deferred<PluginRegistry>();
  try { await h.start(); h.setWrite(async () => { throw new Error("unknown outcome"); }); h.prepare(); h.save(); await h.flush();
    h.setRead(() => get.promise); h.refresh(); await h.cancelRead();
    assert.equal(h.select().props.disabled, true); assert.equal(h.has("uncertain"), true);
    get.resolve(h.getLive()); await h.flush(); assert.equal(h.select().props.disabled, true);
    h.setRead(async () => h.getLive()); h.refresh(); await h.flush(); assert.equal(h.select().props.disabled, false);
    assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
