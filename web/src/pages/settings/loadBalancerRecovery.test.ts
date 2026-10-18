import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import fetchHttp, { HttpError } from '../../lib/fetchClient.ts';
import { MutationObserver, QueryClient, QueryObserver, focusManager } from "@tanstack/react-query";

// Mount the complete production Settings page and its editor with real TanStack
// query/mutation observers. React scheduling and HTTP are controlled collaborators;
// these are offline journey regressions, not browser or server acceptance.
type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Frame = { slots: unknown[]; effects: (() => void)[]; cleanup: (() => void)[] };
const strategies = ["round_robin", "random", "weighted", "consistent_hash"];
const config = (strategy = "round_robin") => ({ strategy: "weighted", runtime: { strategy },
  available_strategies: strategies.map(value => ({ value, label: value, description: value })) });
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function settle() { for (let i = 0; i < 80; i++) await Promise.resolve(); }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function host(options: { initialError?: boolean; realApi?: boolean } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity }, mutations: { retry: false, gcTime: Infinity } } });
  const frames = new Map<string, Frame>();
  const subscriptions: (() => void)[] = [];
  client.mount();
  let frame!: Frame;
  let cursor = 0;
  let dirty = false;
  let seen = new Set<string>();
  let tree: Tree;
  let live = config();
  let read: () => Promise<unknown> = options.initialError ? async () => { throw new Error("fixture unavailable"); } : async () => structuredClone(live);
  let write: (body: { strategy: string }) => Promise<unknown> = async body => { live = config(body.strategy); return {}; };
  const reads: { signal?: AbortSignal }[] = [];
  const writes: { strategy: string }[] = [];
  const queryKeys: unknown[][] = [];
  const initialAuth = { csrfToken: "synthetic-csrf", isAuthenticated: true, token: "synthetic-session-A", user: { user_id: "admin-A", roles: ["admin"], permissions: [] as string[], tier: "normal" } };
  let clearCount = 0;
  const auth = createStore(() => ({ ...initialAuth, clearAuth() {
    clearCount++; auth.setState({ token: "", isAuthenticated: false, user: { user_id: "", roles: [], permissions: [], tier: "" } });
  } }));
  const authority = (user: typeof initialAuth.user) => JSON.stringify([user?.user_id, user?.permissions, user?.roles, user?.tier]);
  let identity = 0;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const react = {
    useState(initial: unknown) {
      const owner = frame; const index = cursor++;
      if (!(index in owner.slots)) owner.slots[index] = typeof initial === "function" ? initial() : initial;
      return [owner.slots[index], (next: unknown) => {
        const value = typeof next === "function" ? next(owner.slots[index]) : next;
        if (!Object.is(value, owner.slots[index])) { owner.slots[index] = value; dirty = true; }
      }];
    },
    useRef(initial: unknown) { const index = cursor++; return frame.slots[index] ?? (frame.slots[index] = { current: initial }); },
    useCallback(callback: () => unknown, deps: unknown[]) { return react.useMemo(() => callback, deps); },
    useMemo(compute: () => unknown, deps: unknown[]) {
      const index = cursor++; const old = frame.slots[index] as { deps?: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) frame.slots[index] = { deps, value: compute() };
      return (frame.slots[index] as { value: unknown }).value;
    },
    useEffect(run: () => void | (() => void), deps: unknown[]) {
      const owner = frame; const index = cursor++;
      if (!same(owner.slots[index] as unknown[], deps)) {
        owner.slots[index] = deps;
        owner.effects.push(() => { owner.cleanup[index]?.(); owner.cleanup[index] = run() || (() => {}); });
      }
    },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const modules: Record<string, unknown> = {
    "@/components/PlatformCapabilities": { PlatformCapabilities: () => null },
    "@/lib/consoleStepUp": { sensitiveMutationOptions: { retry: false } },
    react: { ...react, useLayoutEffect: react.useEffect }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "Link" }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en", resolvedLanguage: "en" } }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: ReturnType<typeof auth.getState>) => unknown) => select(auth.getState()), auth) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++identity}` },
    "@/lib/tenantGovernance": { canAdministerTenantPolicies: () => true }, "@/lib/clipboard": { copyToClipboard: () => {} },
    "@/lib/api": { api: {
      async get(path: string, options?: { signal?: AbortSignal }) {
        if (path.endsWith("/load-balancer")) { reads.push(options ?? {}); return { data: await read() }; }
        if (path.endsWith("/auth")) return { data: { runtime: {} } };
        return { data: {} };
      },
      async put(path: string, body: { strategy: string }) {
        assert.ok(path.endsWith("/load-balancer"), "No other setting may be written");
        writes.push(structuredClone(body)); return { data: await write(body) };
      },
      async post() { throw new Error("No keys or rate rules may be created"); },
    } },
    "@tanstack/react-query": {
      useQueryClient: () => client,
      useQuery(options: Record<string, unknown>) {
        const index = cursor++; let observer = frame.slots[index] as QueryObserver<unknown, Error> | undefined;
        if (!observer) {
          observer = new QueryObserver(client, options as never); frame.slots[index] = observer;
          const stop = observer.subscribe(() => { dirty = true; }); subscriptions.push(stop); frame.cleanup[index] = stop;
        }
        observer.setOptions(options as never); queryKeys.push(options.queryKey as unknown[]);
        return observer.getCurrentResult();
      },
      useMutation(options: Record<string, unknown>) {
        const index = cursor++; let observer = frame.slots[index] as MutationObserver<unknown, Error, unknown> | undefined;
        if (!observer) { observer = new MutationObserver(client, options); frame.slots[index] = observer; const stop = observer.subscribe(() => { dirty = true; }); subscriptions.push(stop); frame.cleanup[index] = stop; }
        observer.setOptions(options);
        return { ...observer.getCurrentResult(), mutate: (value: unknown) => { void observer!.mutate(value).catch(() => {}); } };
      },
    },
  };
  if (options.realApi) {
    const baseApi = (modules["@/lib/api"] as { api: { get: (path: string, config: unknown) => Promise<unknown>; put: (path: string, body: unknown) => Promise<unknown> } }).api;
    const exports: Record<string, unknown> = {};
    const storage = { removeItem() {}, getItem() { return null; } };
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../../lib/api.ts", import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => {
        if (name === "@/observability/browserTelemetry") return { browserTraceparent: () => undefined };
        if (name === "@/lib/fetchClient") return { __esModule: true, default: fetchHttp, ...fetchHttp, HttpError };
        if (name === "@/config/runtime") return { getApiBaseUrlConfig: () => "" };
        if (name === "@/hooks/use-toast") return { toast: { warning() {} } };
        if (name === "@/i18n") return { __esModule: true, default: { t: (key: string) => key } };
        if (name in modules) return modules[name];
        throw new Error(`Unexpected API import ${name}`);
      }, __hot: { dispose(stop: () => void) { subscriptions.push(stop); } }, localStorage: storage, sessionStorage: storage, window: { location: { pathname: "/settings", href: "" } }, Date, Error, console,
    });
    const realApi = exports.api as ReturnType<typeof fetchHttp.create>;
    realApi.defaults.adapter = async request => {
      try {
        const response = await (request.method === "put" ? baseApi.put(request.url!, JSON.parse(request.data)) : baseApi.get(request.url!, request));
        return { ...(response as object), status: 200, statusText: "OK", config: request, headers: {} };
      } catch (error) {
        if (fetchHttp.isHttpError(error)) { error.config = request; if (error.response) error.response.config = request; }
        throw error;
      }
    };
    modules["@/lib/api"] = { api: realApi };
  }
  function load(filename: string): Record<string, (props: Record<string, unknown>) => Tree> {
    const source = readFileSync(new URL(filename, import.meta.url), "utf8");
    const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => {
        if (name === "./settings/AuthSettings") return load("./AuthSettings.tsx");
        if (name === "./PasskeyStepUp") return { PasskeyStepUp: () => null };
        if (name === "@/hooks/useAuthenticatedReadScope") return load("../../hooks/useAuthenticatedReadScope.ts");
        if (name === "./settings/LoadBalancerSettings") return load("./LoadBalancerSettings.tsx");
        if (name.startsWith("@/components/ui/")) return named;
        assert.ok(name in modules, `Unexpected import ${name}`); return modules[name];
      }, Error, AbortController, console,
    });
    return exports;
  }
  const page = load("../Settings.tsx").SettingsPage;
  function renderNode(node: unknown, path: string): unknown {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((value, index) => renderNode(value, `${path}:${index}`));
    const value = node as Tree;
    if (typeof value.type === "function") {
      const key = `${path}:${value.type.name}:${value.key ?? ""}`;
      seen.add(key);
      frame = frames.get(key) ?? { slots: [], effects: [], cleanup: [] }; frames.set(key, frame); cursor = 0;
      return renderNode(value.type(value.props), key);
    }
    return { ...value, props: { ...value.props, children: renderNode(value.props?.children, `${path}:children`) } };
  }
  function render() {
    for (let turn = 0; turn < 15; turn++) {
      dirty = false; seen = new Set();
      tree = renderNode({ type: page, props: {} }, "root") as Tree;
      for (const [key, owner] of frames) {
        if (!seen.has(key)) { owner.cleanup.forEach(cleanup => cleanup?.()); frames.delete(key); }
        else owner.effects.splice(0).forEach(effect => effect());
      }
      if (!dirty) return tree;
    }
    throw new Error("Unexpected render loop");
  }
  const nodes = (predicate: (node: Tree) => boolean) => all(render(), predicate);
  const button = (suffix: string) => {
    const found = nodes(node => node.type === "Button" && node.props.children === `settings.loadBalancer.${suffix}`)[0];
    assert.ok(found, `Missing button ${suffix}`); return found;
  };
  const radio = (value: string) => {
    const found = nodes(node => node.type === "input" && node.props.name === "load-balancer-strategy" && node.props.value === value)[0];
    assert.ok(found, `Missing strategy ${value}`); return found;
  };
  return {
    get auth() { return auth.getState(); }, setAuth: auth.setState, writes, reads, queryKeys, render, radio, button, clearCount: () => clearCount,
    async start() { render(); await settle(); render(); },
    async click(suffix: string) { const node = button(suffix); assert.equal(Boolean(node.props.disabled), false, `${suffix} disabled`); (node.props.onClick as () => void)(); await settle(); render(); },
    choose(value: string) { const node = radio(value); assert.equal(Boolean(node.props.disabled), false); (node.props.onChange as () => void)(); render(); },
    async focus() { focusManager.setFocused(false); focusManager.setFocused(true); await settle(); render(); },
    async refetch() { await client.refetchQueries({ queryKey: ["load-balancer"] }); await settle(); render(); },
    setRead(next: typeof read) { read = next; }, setWrite(next: typeof write) { write = next; },
    setCurrent(strategy: string) { live = config(strategy); },
    has(suffix: string) { return nodes(node => node.props?.children === `settings.loadBalancer.${suffix}`).length > 0; },
    value(id: string) { return nodes(node => node.props?.["data-testid"] === `lb-${id}`)[0]?.props.children; },
    dispose() { for (const owner of frames.values()) owner.cleanup.forEach(cleanup => cleanup?.()); subscriptions.forEach(stop => stop()); client.unmount(); client.clear(); },
  };
}

test("a background read preserves an unsaved strategy instead of replacing the draft", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setCurrent("random"); await h.refetch();
    assert.equal(h.radio("weighted").props.checked, true);
    assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("initial read failure cannot save a fabricated default strategy", async () => {
  const h = host({ initialError: true }); try {
    await h.start(); assert.equal(h.button("save").props.disabled, true); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("a failed save retains the draft across a later successful read", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setWrite(async () => { throw new Error("fixture failure"); });
    await h.click("save"); h.setCurrent("random"); await h.refetch();
    assert.equal(h.radio("weighted").props.checked, true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("successful save is only confirmed after matching runtime read-back", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); await h.click("save");
    assert.equal(h.has("saved"), true); assert.equal(h.value("current"), "weighted");
    assert.equal(h.value("draft"), undefined); assert.equal(h.writes.length, 1); assert.equal(h.reads.length, 3);
    assert.equal(h.button("save").props.disabled, true);
  } finally { h.dispose(); }
});
test("failure after server mutation remains uncertain and never automatically repeats the write", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setWrite(async body => { h.setCurrent(body.strategy); throw new Error("audit failed after mutation"); });
    await h.click("save"); assert.equal(h.has("saveUnknown"), true);
    assert.equal(h.has("saved"), false); assert.equal(h.value("current"), "weighted"); assert.equal(h.value("draft"), "weighted");
    await h.click("refresh"); assert.equal(h.writes.length, 1); assert.equal(h.button("save").props.disabled, true);
    await h.click("useCurrent"); assert.equal(h.value("draft"), undefined); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("failed read-back holds the draft and blocks replay until refresh and explicit review", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setWrite(async () => { h.setRead(async () => { throw new Error("read unavailable"); }); return {}; });
    await h.click("save"); assert.equal(h.has("readBackFailed"), true); assert.equal(h.has("loadFailed"), true);
    assert.equal(h.value("draft"), "weighted"); assert.equal(h.button("save").props.disabled, true);
    h.setRead(async () => config()); await h.click("refresh"); assert.equal(h.button("save").props.disabled, true);
    await h.click("reviewDraft"); assert.equal(h.button("save").props.disabled, false); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("preflight detects an unobserved administrative edit and issues no PUT", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setCurrent("random"); await h.click("save");
    assert.equal(h.writes.length, 0); assert.equal(h.has("conflict"), true); assert.equal(h.value("draft"), "weighted");
    await h.click("reviewDraft"); await h.click("save"); assert.equal(h.writes.length, 1); assert.equal(h.has("saved"), true);
  } finally { h.dispose(); }
});
test("preflight read failure does not issue a write", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setRead(async () => { throw new Error("preflight unavailable"); }); await h.click("save");
    assert.equal(h.writes.length, 0); assert.equal(h.has("loadFailed"), true); assert.equal(h.value("draft"), "weighted");
  } finally { h.dispose(); }
});
test("accepted save with a different read-back preserves the draft and requires review", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setWrite(async () => { h.setCurrent("random"); return {}; }); await h.click("save");
    assert.equal(h.has("readBackChanged"), true); assert.equal(h.has("saved"), false); assert.equal(h.value("draft"), "weighted");
    assert.equal(h.button("save").props.disabled, true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("duplicate clicks and a stale radio closure cannot alter an in-flight save", async () => {
  const h = host(); const pending = deferred<unknown>(); try {
    await h.start(); h.choose("weighted"); h.setWrite(() => pending.promise);
    const save = h.button("save").props.onClick as () => void;
    const edit = h.radio("random").props.onChange as () => void;
    save(); save(); edit(); await settle(); h.render();
    assert.equal(h.writes.length, 1); assert.equal(h.value("draft"), "weighted"); assert.equal(h.radio("random").props.disabled, true);
    h.setCurrent("weighted"); pending.resolve({}); await settle(); h.render(); assert.equal(h.has("saved"), true);
  } finally { pending.resolve({}); h.dispose(); }
});
for (const boundary of ["account", "token", "roles", "permissions"] as const) {
  test(`a late save response cannot affect a new ${boundary} scope`, async () => {
    const h = host(); const pending = deferred<unknown>(); try {
      await h.start(); h.choose("weighted"); h.setWrite(() => pending.promise); await h.click("save");
      if (boundary === "account") h.setAuth({ user: { ...h.auth.user, user_id: "admin-B" } });
      if (boundary === "token") h.setAuth({ token: "synthetic-session-B" });
      if (boundary === "roles") h.setAuth({ user: { ...h.auth.user, roles: ["operator"] } });
      if (boundary === "permissions") h.setAuth({ user: { ...h.auth.user, permissions: ["another-permission"] } });
      h.render(); await settle(); h.render(); h.choose("random");
      const reads = h.reads.length; pending.resolve({}); await settle(); h.render();
      assert.equal(h.value("draft"), "random"); assert.equal(h.has("saved"), false); assert.equal(h.reads.length, reads);
      assert.equal(JSON.stringify(h.queryKeys).includes("synthetic-session"), false);
    } finally { pending.resolve({}); h.dispose(); }
  });
}
test("unmount during preflight prevents the later PUT", async () => {
  const h = host(); const pending = deferred<unknown>();
  await h.start(); h.choose("weighted"); h.setRead(() => pending.promise);
  const save = h.button("save").props.onClick as () => void; save(); await settle(); h.dispose();
  pending.resolve(config()); await settle(); assert.equal(h.writes.length, 0);
});
test("only strategies from fresh server read-back can be submitted", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setRead(async () => ({ ...config(), available_strategies: [{ value: "random", label: "random" }] }));
    await h.click("save"); assert.equal(h.writes.length, 0); assert.equal(h.value("draft"), "weighted");
    assert.equal(h.button("save").props.disabled, true);
  } finally { h.dispose(); }
});

test("credential expiry during preflight prevents a later PUT even before rerender", async () => {
  const h = host(); const pending = deferred<unknown>(); try {
    await h.start(); h.choose("weighted"); h.setRead(() => pending.promise);
    const save = h.button("save").props.onClick as () => void; save(); await settle();
    h.setAuth({ token: "expired-session" }); pending.resolve(config()); await settle();
    assert.equal(h.writes.length, 0);
  } finally { pending.resolve(config()); h.dispose(); }
});
test("read retry recovers an initially unavailable editor without writing", async () => {
  const h = host({ initialError: true }); try {
    await h.start(); assert.equal(h.has("loadFailed"), true);
    h.setRead(async () => config("random")); await h.click("refresh");
    assert.equal(h.value("current"), "random"); assert.equal(h.radio("random").props.checked, true);
    assert.equal(h.has("loadFailed"), false); assert.equal(h.writes.length, 0);
    h.choose("weighted"); assert.equal(h.button("save").props.disabled, false);
  } finally { h.dispose(); }
});
test("both failed write and failed reconciliation remain uncertain without replay", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setWrite(async () => {
      h.setRead(async () => { throw new Error("read unavailable"); }); throw new Error("response lost");
    });
    await h.click("save"); assert.equal(h.has("saveUnknown"), true); assert.equal(h.has("loadFailed"), true);
    assert.equal(h.value("draft"), "weighted"); assert.equal(h.button("save").props.disabled, true);
    h.setRead(async () => config("random")); await h.click("refresh");
    assert.equal(h.has("saveUnknown"), true); assert.equal(h.writes.length, 1);
    assert.equal(h.button("save").props.disabled, true);
  } finally { h.dispose(); }
});
test("a delayed read from the previous scope cannot replace the new editor", async () => {
  const h = host(); const pending = deferred<unknown>(); try {
    await h.start(); h.choose("weighted"); h.setRead(() => pending.promise);
    const refreshing = h.refetch(); await settle();
    const oldSignal = h.reads.at(-1)?.signal;
    h.setAuth({ token: "synthetic-session-B" }); h.setRead(async () => config("random"));
    h.render(); await settle(); h.render(); h.choose("consistent_hash");
    pending.resolve(config("weighted")); await refreshing; await settle();
    assert.equal(oldSignal?.aborted, true); assert.equal(h.value("current"), "random");
    assert.equal(h.value("draft"), "consistent_hash"); assert.equal(h.writes.length, 0);
  } finally { pending.resolve(config()); h.dispose(); }
});

// Production component + production Fetch client integration regression.
for (const boundary of ["account", "token", "permissions", "roles", "tier"]) {
test(`old ${boundary} PUT returning 401 must not clear the new account or its draft`, async () => {
  const h = host({ realApi: true }); const pending = deferred<unknown>(); try {
    await h.start(); h.choose("weighted"); h.setWrite(() => pending.promise); await h.click("save");
    assert.equal(h.writes.length, 1);
    if (boundary === "account") { h.setAuth({ token: "synthetic-session-B" }); h.setAuth({ user: { ...h.auth.user, user_id: "admin-B" } }); }
    if (boundary === "token") h.setAuth({ token: "synthetic-session-B" });
    if (boundary === "permissions") h.setAuth({ user: { ...h.auth.user, permissions: ["changed-permission"] } });
    if (boundary === "roles") h.setAuth({ user: { ...h.auth.user, roles: ["changed-role"] } });
    if (boundary === "tier") h.setAuth({ user: { ...h.auth.user, tier: "changed-tier" } });
    h.render(); await settle(); h.render(); h.choose("random");
    assert.equal(h.value("draft"), "random");
    const failure = new HttpError("synthetic unauthorized", "ERR_BAD_REQUEST", undefined, undefined, { status: 401, statusText: "Unauthorized", data: {}, headers: {}, config: {} as never });
    pending.reject(failure); await settle(); h.render();
    assert.equal(h.clearCount(), 0, "old PUT response cleared the new account");
    assert.equal(h.auth.token, boundary === "account" || boundary === "token" ? "synthetic-session-B" : "synthetic-session-A");
    assert.equal(h.value("draft"), "random");
  } finally { pending.resolve({}); h.dispose(); }
});
}

test("real QueryClient window-focus reconciliation preserves unsaved draft", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setCurrent("random");
    const previousReads = h.reads.length; await h.focus();
    assert.ok(h.reads.length > previousReads);
    assert.equal(h.value("current"), "random"); assert.equal(h.value("draft"), "weighted");
    assert.equal(h.has("conflict"), true); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("background fetch failure blocks save while retaining last read and draft", async () => {
  const h = host(); try {
    await h.start(); h.choose("weighted"); h.setRead(async () => { throw new Error("offline fixture"); }); await h.focus();
    assert.equal(h.has("loadFailed"), true); assert.equal(h.value("current"), "round_robin");
    assert.equal(h.value("draft"), "weighted"); assert.equal(h.button("save").props.disabled, true); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("late old-account HTTP 500 cannot change new account draft or trigger reads", async () => {
  const h = host({ realApi: true }); const pending = deferred<unknown>(); try {
    await h.start(); h.choose("weighted"); h.setWrite(() => pending.promise); await h.click("save");
    h.setAuth({ token: "synthetic-session-B" }); h.setAuth({ user: { ...h.auth.user, user_id: "admin-B" } });
    h.render(); await settle(); h.render(); h.choose("random");
    const count = h.reads.length;
    pending.reject(new HttpError("synthetic failure", "ERR_BAD_RESPONSE", undefined, undefined, { status: 500, statusText: "Internal Server Error", data: {}, headers: {}, config: {} as never }));
    await settle(); h.render();
    assert.equal(h.value("draft"), "random"); assert.equal(h.clearCount(), 0); assert.equal(h.reads.length, count); assert.equal(h.writes.length, 1);
  } finally { pending.resolve({}); h.dispose(); }
});
