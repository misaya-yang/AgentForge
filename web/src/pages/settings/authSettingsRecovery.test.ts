import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import axios, { AxiosError } from "axios";
import { MutationObserver, QueryClient, QueryObserver, focusManager } from "@tanstack/react-query";

// Complete production Settings page/AuthSettings/shared auth scope and Axios.
// Real query observers; controlled React scheduling, HTTP and Radix-compatible
// active/forceMount presence semantics. This is not DOM/browser acceptance.
type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Frame = { slots: unknown[]; effects: (() => void)[]; cleanup: (() => void)[] };
const config = (issuer: string | null = "runtime-before") => ({
  jwt: { enabled: false, issuer: "startup-issuer", algorithms: ["RS256"], audience: null },
  api_key: { enabled: false, header_name: "X-Startup-Key", key_count: 2 },
  runtime: { jwt_enabled: true, jwt_secret_configured: true, jwt_algorithms: ["HS512"],
    jwt_issuer: issuer, api_key_enabled: true, api_key_header: "X-Runtime-Key", api_keys_count: 3 },
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("background read preserves an unsaved authentication issuer", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.setCurrent("other-admin"); await h.refetch();
    assert.equal(h.input("jwt-issuer").props.value, "local-draft");
    assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("initial authentication read failure cannot save fabricated defaults", async () => {
  const h = host({ initialError: true }); try {
    await h.start(); assert.equal(h.button("save").props.disabled, true);
    assert.equal(h.has("loadFailed"), true); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
test("failed save preserves non-secret input across later reads", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft");
    h.setWrite(async () => { throw new Error("synthetic failure"); }); await h.click("save");
    h.setCurrent("other-admin"); await h.refetch();
    assert.equal(h.input("jwt-issuer").props.value, "local-draft"); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("success preserves algorithms and omits blank secrets and API keys", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "new-issuer"); await h.click("save");
    assert.equal(h.writes.length, 1); assert.deepEqual(h.writes[0], {
      jwt_enabled: true, jwt_issuer: "new-issuer", api_key_enabled: true, api_key_header: "X-Runtime-Key", jwt_algorithms: ["HS512"],
    });
    assert.equal(h.has("saved"), true); assert.equal(h.has("draft"), false);
    assert.equal(h.reads.length, 3); assert.equal(h.button("save").props.disabled, true);
    assert.ok(JSON.stringify(h.value("current")).includes("new-issuer"));
    assert.ok(JSON.stringify(h.value("startup")).includes("startup-issuer"));
  } finally { h.dispose(); }
});
test("empty issuer uses the existing nullable API contract", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", ""); await h.click("save");
    assert.equal(h.writes[0].jwt_issuer, null); assert.equal(h.has("saved"), true);
  } finally { h.dispose(); }
});
test("observed conflict requires explicit review and fresh preflight", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.setCurrent("other-admin"); await h.click("save");
    assert.equal(h.writes.length, 0); assert.equal(h.has("conflict"), true);
    assert.equal(h.input("jwt-issuer").props.value, "local-draft");
    await h.click("reviewDraft"); await h.click("save");
    assert.equal(h.writes.length, 1); assert.equal(h.has("saved"), true);
  } finally { h.dispose(); }
});
test("adopting current metadata discards only the explicitly abandoned draft", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.edit("jwt-secret", "synthetic-replacement");
    h.setCurrent("other-admin"); await h.refetch(); await h.click("useCurrent");
    assert.equal(h.input("jwt-issuer").props.value, "other-admin"); assert.equal(h.input("jwt-secret").props.value, "");
    assert.equal(h.has("draft"), false); assert.equal(h.writes.length, 0);
  } finally { h.dispose(); }
});
for (const failure of ["rejected", "malformed-ack"] as const) {
  test(`${failure} after mutation stays unknown even when read-back matches`, async () => {
    const h = host(); try {
      await h.start(); h.edit("jwt-issuer", "new-issuer"); h.edit("jwt-secret", "synthetic-replacement");
      h.setWrite(async body => {
        h.setCurrent(body.jwt_issuer as string);
        if (failure === "rejected") throw new Error("audit failed after mutation");
        return {};
      });
      await h.click("save");
      assert.equal(h.has("saveUnknown"), true); assert.equal(h.has("saved"), false);
      assert.equal(h.input("jwt-issuer").props.value, "new-issuer"); assert.equal(h.input("jwt-secret").props.value, "");
      await h.click("refresh"); assert.equal(h.writes.length, 1); assert.equal(h.button("save").props.disabled, true);
      await h.click("useCurrent"); assert.equal(h.has("saveUnknown"), false); assert.equal(h.writes.length, 1);
    } finally { h.dispose(); }
  });
}
test("preflight failure retains an unsubmitted write-only replacement", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.edit("jwt-secret", "synthetic-replacement");
    h.setRead(async () => { throw new Error("offline"); }); await h.click("save");
    assert.equal(h.writes.length, 0); assert.equal(h.input("jwt-secret").props.value, "synthetic-replacement");
    assert.equal(h.input("jwt-issuer").props.value, "local-draft"); assert.equal(h.has("loadFailed"), true);
    h.setRead(async () => config()); await h.click("refresh"); assert.equal(h.button("save").props.disabled, false);
  } finally { h.dispose(); }
});
test("accepted write followed by failed read-back requires read and explicit review", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.setWrite(async () => {
      h.setRead(async () => { throw new Error("read unavailable"); }); return { status: "success" };
    });
    await h.click("save"); assert.equal(h.has("readBackFailed"), true); assert.equal(h.has("saved"), false);
    h.setRead(async () => config()); await h.click("refresh"); assert.equal(h.button("save").props.disabled, true);
    await h.click("reviewDraft"); assert.equal(h.button("save").props.disabled, false); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("accepted but mismatching read-back preserves draft without claiming success", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.setWrite(async () => ({ status: "success" }));
    await h.click("save"); assert.equal(h.has("readBackChanged"), true); assert.equal(h.has("saved"), false);
    assert.equal(h.input("jwt-issuer").props.value, "local-draft"); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("double clicks and stale same-render edits cannot issue duplicate writes", async () => {
  const h = host(); const wait = deferred<unknown>(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft");
    const save = h.button("save").props.onClick as () => void;
    const edit = h.input("jwt-issuer").props.onChange as (event: unknown) => void;
    h.setWrite(() => wait.promise); save(); save(); edit({ target: { value: "late-edit" } }); await settle(); h.render();
    assert.equal(h.writes.length, 1); assert.equal(h.input("jwt-issuer").props.value, "local-draft");
    h.setCurrent("local-draft"); wait.resolve({ status: "success" }); await settle();
    save(); await settle(); assert.equal(h.writes.length, 1);
    h.render(); assert.equal(h.has("saved"), true);
  } finally { wait.resolve({ status: "success" }); h.dispose(); }
});
test("unmount during preflight prevents a late PUT", async () => {
  const h = host(); const wait = deferred<unknown>();
  await h.start(); h.edit("jwt-issuer", "local-draft"); h.setRead(() => wait.promise);
  (h.button("save").props.onClick as () => void)(); await settle(); h.dispose();
  wait.resolve(config()); await settle(); assert.equal(h.writes.length, 0);
});
for (const boundary of ["account", "token", "roles", "permissions", "tier", "aba"]) {
  test(`${boundary} authority boundary fences a late preflight before rerender`, async () => {
    const h = host(); const wait = deferred<unknown>(); try {
      await h.start(); h.edit("jwt-issuer", "local-draft"); h.edit("jwt-secret", "synthetic-replacement");
      h.setRead(() => wait.promise); (h.button("save").props.onClick as () => void)(); await settle();
      if (boundary === "account") h.setAuth({ user: { ...h.auth.user, user_id: "admin-B" } });
      if (boundary === "token" || boundary === "aba") h.setAuth({ token: "synthetic-session-B" });
      if (boundary === "aba") h.setAuth({ token: "synthetic-session-A" });
      if (boundary === "roles") h.setAuth({ user: { ...h.auth.user, roles: ["operator"] } });
      if (boundary === "permissions") h.setAuth({ user: { ...h.auth.user, permissions: ["new-scope"] } });
      if (boundary === "tier") h.setAuth({ user: { ...h.auth.user, tier: "platform" } });
      wait.resolve(config()); await settle(); assert.equal(h.writes.length, 0);
      h.setRead(async () => config()); h.render(); await settle(); h.render();
      assert.equal(h.input("jwt-issuer").props.value, "runtime-before"); assert.equal(h.input("jwt-secret").props.value, "");
      assert.equal(JSON.stringify(h.queryKeys).includes("synthetic-session"), false);
    } finally { wait.resolve(config()); h.dispose(); }
  });
}
test("old-account 401 cannot clear a new account or its local draft", async () => {
  const h = host(); const wait = deferred<unknown>(); try {
    await h.start(); h.edit("jwt-issuer", "old-draft"); h.setWrite(() => wait.promise); await h.click("save");
    h.setAuth({ token: "synthetic-session-B", user: { ...h.auth.user, user_id: "admin-B" } });
    h.render(); await settle(); h.render(); h.edit("jwt-issuer", "new-account-draft");
    const reads = h.reads.length;
    wait.reject(new AxiosError("synthetic unauthorized", "ERR_BAD_REQUEST", undefined, undefined,
      { status: 401, statusText: "Unauthorized", data: {}, headers: {}, config: {} as never }));
    await settle(); h.render();
    assert.equal(h.clearCount(), 0); assert.equal(h.input("jwt-issuer").props.value, "new-account-draft");
    assert.equal(h.reads.length, reads); assert.equal(h.writes.length, 1);
  } finally { wait.resolve({ status: "success" }); h.dispose(); }
});
test("secret echoes never enter query or mutation caches", async () => {
  const h = host(); try {
    const echoed = { ...config(), jwt_secret: "synthetic-echo", runtime: { ...config().runtime, jwt_secret: "synthetic-echo", api_keys: ["synthetic-raw-key"] } };
    h.setRead(async () => echoed); await h.start();
    assert.equal(h.input("jwt-secret").props.value, "");
    h.edit("jwt-secret", "synthetic-replacement"); await h.click("save");
    assert.equal(h.writes[0].jwt_secret, "synthetic-replacement"); assert.equal(h.input("jwt-secret").props.value, "");
    const cached = JSON.stringify([h.cache(), h.mutations()]);
    assert.equal(cached.includes("synthetic-echo"), false); assert.equal(cached.includes("synthetic-raw-key"), false);
    assert.equal(cached.includes("synthetic-replacement"), false);
  } finally { h.dispose(); }
});
for (const corrupt of [null, {}, { runtime: {} }, { ...config(), runtime: { ...config().runtime, jwt_enabled: "true" } }]) {
  test(`invalid auth projection ${JSON.stringify(corrupt)} is not savable`, async () => {
    const h = host(); try {
      h.setRead(async () => corrupt); await h.start();
      assert.equal(h.has("loadFailed"), true); assert.equal(h.button("save").props.disabled, true); assert.equal(h.writes.length, 0);
    } finally { h.dispose(); }
  });
}
test("invalid headers prevent writes and retain editable input", async () => {
  const h = host(); try {
    await h.start(); h.edit("api-key-header", "Invalid Header\r\n");
    assert.equal(h.has("invalid"), true); assert.equal(h.button("save").props.disabled, true); assert.equal(h.writes.length, 0);
    h.edit("api-key-header", "X-Custom-Key"); assert.equal(h.button("save").props.disabled, false);
  } finally { h.dispose(); }
});
test("tab switching retains draft and uncertainty with an explicitly hidden inactive panel", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft"); h.edit("jwt-secret", "synthetic-replacement");
    h.tab("capacity");
    const panel = h.nodes(node => node.type === "TabsContent" && node.props.value === "auth")[0];
    assert.equal(panel.props.forceMount, true); assert.ok(String(panel.props.className).includes("data-[state=inactive]:hidden"));
    h.tab("auth"); assert.equal(h.input("jwt-issuer").props.value, "local-draft"); assert.equal(h.input("jwt-secret").props.value, "synthetic-replacement");
    h.setWrite(async () => { throw new Error("unknown write"); }); await h.click("save");
    h.tab("capacity"); h.tab("auth"); assert.equal(h.has("saveUnknown"), true); assert.equal(h.button("save").props.disabled, true);
    assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
test("failed background read marks cached metadata stale and blocks save", async () => {
  const h = host(); try {
    await h.start(); h.edit("jwt-issuer", "local-draft");
    h.setRead(async () => { throw new Error("offline"); }); await h.focus();
    assert.equal(h.has("loadFailed"), true); assert.ok(h.value("current"));
    assert.equal(h.input("jwt-issuer").props.value, "local-draft"); assert.equal(h.button("save").props.disabled, true);
  } finally { h.dispose(); }
});
test("initial read failure recovers through explicit refresh and successful save", async () => {
  const h = host({ initialError: true }); try {
    await h.start(); assert.equal(h.has("loadFailed"), true);
    h.setRead(async () => config()); await h.click("refresh");
    assert.equal(h.has("loadFailed"), false); h.edit("jwt-issuer", "recovered-issuer");
    h.setWrite(async body => { h.setRead(async () => config(body.jwt_issuer as string)); return { status: "success" }; });
    await h.click("save"); assert.equal(h.has("saved"), true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});
for (const boundary of ["token", "aba"]) {
  test(`authority ${boundary} during post-write read cannot publish a stale success`, async () => {
    const h = host(); const wait = deferred<unknown>(); try {
      await h.start(); h.edit("jwt-issuer", "old-submitted");
      h.setWrite(async () => { h.setRead(() => wait.promise); return { status: "success" }; });
      await h.click("save"); assert.equal(h.writes.length, 1);
      h.setAuth({ token: "synthetic-session-B" });
      if (boundary === "aba") h.setAuth({ token: "synthetic-session-A" });
      h.setRead(async () => config("replacement-view")); h.render(); await settle(); h.render();
      h.edit("jwt-issuer", "new-local-draft"); const reads = h.reads.length;
      wait.resolve(config("old-submitted")); await settle(); h.render();
      assert.equal(h.has("saved"), false); assert.equal(h.input("jwt-issuer").props.value, "new-local-draft");
      assert.ok(JSON.stringify(h.value("current")).includes("replacement-view"));
      assert.equal(h.reads.length, reads); assert.equal(h.writes.length, 1);
    } finally { wait.resolve(config()); h.dispose(); }
  });
}
async function settle() { for (let i = 0; i < 80; i++) await Promise.resolve(); }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function host(options: { initialError?: boolean } = {}) {
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
  let selectedTab = "auth";
  let read: () => Promise<unknown> = options.initialError ? async () => { throw new Error("fixture unavailable"); } : async () => structuredClone(live);
  let write: (body: Record<string, unknown>) => Promise<unknown> = async body => {
    live = { ...live, runtime: { ...live.runtime, ...body } };
    return { status: "success" };
  };
  const reads: { signal?: AbortSignal }[] = [];
  const writes: Record<string, unknown>[] = [];
  const queryKeys: unknown[][] = [];
  const initialAuth = { isAuthenticated: true, token: "synthetic-session-A", user: { user_id: "admin-A", roles: ["admin"], permissions: [] as string[], tier: "normal" } };
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
    react: { ...react, useLayoutEffect: react.useEffect }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "Link" }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: ReturnType<typeof auth.getState>) => unknown) => select(auth.getState()), auth) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++identity}` },
    "@/lib/tenantGovernance": { canAdministerTenantPolicies: () => true }, "@/lib/clipboard": { copyToClipboard: () => {} },
    "@/lib/api": { api: {
      async get(path: string, options?: { signal?: AbortSignal }) {
        if (path.endsWith("/auth")) { reads.push(options ?? {}); return { data: await read() }; }
        if (path.endsWith("/load-balancer")) return { data: { strategy: "random", available_strategies: [] } };
        return { data: {} };
      },
      async put(path: string, body: Record<string, unknown>) {
        assert.equal(path, "/api/v1/config/auth", "No other setting may be written");
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
  {
    const baseApi = (modules["@/lib/api"] as { api: { get: (path: string, config: unknown) => Promise<unknown>; put: (path: string, body: unknown) => Promise<unknown> } }).api;
    const exports: Record<string, unknown> = {};
    const storage = { removeItem() {}, getItem() { return null; } };
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../../lib/api.ts", import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => {
        if (name === "axios") return { __esModule: true, default: axios, AxiosError };
        if (name === "@/config/runtime") return { getApiBaseUrlConfig: () => "" };
        if (name === "@/hooks/use-toast") return { toast: { warning() {} } };
        if (name === "@/i18n") return { __esModule: true, default: { t: (key: string) => key } };
        if (name in modules) return modules[name];
        throw new Error(`Unexpected API import ${name}`);
      }, __hot: { dispose(stop: () => void) { subscriptions.push(stop); } }, localStorage: storage, sessionStorage: storage, window: { location: { pathname: "/settings", href: "" } }, Date, Error, console,
    });
    const realApi = exports.api as ReturnType<typeof axios.create>;
    realApi.defaults.adapter = async request => {
      try {
        const response = await (request.method === "put" ? baseApi.put(request.url!, JSON.parse(request.data)) : baseApi.get(request.url!, request));
        return { ...(response as object), status: 200, statusText: "OK", config: request, headers: {} };
      } catch (error) {
        if (axios.isAxiosError(error)) { error.config = request; if (error.response) error.response.config = request; }
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
    if (value.type === "TabsContent" && value.props.value !== selectedTab && !value.props.forceMount) return null;
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
    const key = suffix === "save" || suffix === "saving" ? `settings.auth.${suffix}` : `settings.auth.recovery.${suffix}`;
    const found = nodes(node => node.type === "Button" && node.props.children === key)[0];
    assert.ok(found, `Missing button ${suffix}`); return found;
  };
  const input = (field: string) => {
    const found = nodes(node => node.type === "Input" && (node.props.id === `settings-${field}`
      || (field === "jwt-issuer" && node.props.placeholder === "settings.auth.jwt.issuerPlaceholder")
      || (field === "jwt-secret" && node.props.type === "password")
      || (field === "api-key-header" && node.props.placeholder === "settings.auth.apiKey.headerPlaceholder")))[0];
    assert.ok(found, `Missing input ${field}`); return found;
  };
  return {
    get auth() { return auth.getState(); }, setAuth: auth.setState, writes, reads, queryKeys, render, input, button, nodes,
    clearCount: () => clearCount,
    cache: () => client.getQueryCache().getAll().map(query => ({ key: query.queryKey, data: query.state.data })),
    mutations: () => client.getMutationCache().getAll(),
    async start() { render(); await settle(); render(); },
    async click(suffix: string) { const node = button(suffix); assert.equal(Boolean(node.props.disabled), false, `${suffix} disabled`); (node.props.onClick as () => void)(); await settle(); render(); },
    edit(field: string, value: string) { const node = input(field); assert.equal(Boolean(node.props.disabled), false); (node.props.onChange as (event: unknown) => void)({ target: { value } }); render(); },
    async focus() { focusManager.setFocused(false); focusManager.setFocused(true); await settle(); render(); },
    async refetch() { await client.refetchQueries({ queryKey: ["auth-config"] }); await settle(); render(); },
    tab(value: string) { selectedTab = value; render(); },
    setRead(next: typeof read) { read = next; }, setWrite(next: typeof write) { write = next; },
    setCurrent(issuer: string | null) { live = config(issuer); },
    has(suffix: string) { return nodes(node => node.props?.children === `settings.auth.recovery.${suffix}`).length > 0; },
    value(id: string) { return nodes(node => node.props?.["data-testid"] === `auth-${id}`)[0]; },
    dispose() { for (const owner of frames.values()) owner.cleanup.forEach(cleanup => cleanup?.()); subscriptions.forEach(stop => stop()); client.unmount(); client.clear(); },
  };
}
