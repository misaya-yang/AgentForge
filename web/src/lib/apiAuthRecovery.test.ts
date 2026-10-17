import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import axios, { AxiosError, AxiosHeaders, type AxiosInstance, type InternalAxiosRequestConfig } from "axios";
import { createTelemetryClient } from "../observability/telemetryCore.ts";
import { sdkAuthorityScope } from "./sdkAuthorityScope.ts";

// Real production Axios interceptors and adapter dispatch; synthetic transport
// only. The fixtures are deliberately fake credentials and never contact a server.
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function host(token: string | null = "synthetic-token-A", userId: string | undefined = "user-A", trace: (url: string, base?: string) => string | undefined = () => undefined) {
  const storage = () => {
    const values = new Map<string, string>();
    return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  };
  const local = storage(); const session = storage();
  const location = { pathname: "/settings", href: "/settings" };
  let clears = 0;
  const auth = createStore<{
    token: string | null;
    user: { user_id: string; roles: string[]; permissions: string[]; tier: string; display_name?: string } | null;
    isAuthenticated: boolean;
    isLoading: boolean;
    csrfToken: string | null;
    clearAuth: () => void;
  }>()(set => ({
    token, user: userId ? { user_id: userId, roles: ["admin"], permissions: [], tier: "normal" } : null,
    isAuthenticated: Boolean(token && userId), isLoading: false, csrfToken: "synthetic-csrf",
    clearAuth() { clears++; set({ token: null, user: null, isAuthenticated: false }); },
  }));
  let dispose: (() => void) | undefined;
  const modules: Record<string, unknown> = {
    axios: { ...axios, default: axios, AxiosError },
    "@/observability/browserTelemetry": { browserTraceparent: trace },
    "@/config/runtime": { getApiBaseUrlConfig: () => "" },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/hooks/use-toast": { toast: { warning: () => {} } }, "@/i18n": { default: { t: (key: string) => key } },
    "@/store/useAuthStore": { useAuthStore: auth },
  };
  const exports: { api?: AxiosInstance } = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./api.ts", import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
    localStorage: local, sessionStorage: session, window: { location }, Date, Promise, WeakMap,
    __hot: { dispose(callback: () => void) { dispose = callback; } } });
  const api = exports.api!;
  function pending401(method: "get" | "put" = "put") {
    const entered = deferred<InternalAxiosRequestConfig>(); const finish = deferred<void>();
    let responseError: AxiosError;
    const result = api.request({ method, url: "/api/v1/config/load-balancer", data: { strategy: "weighted" },
      adapter: async config => {
        entered.resolve(config); await finish.promise;
        responseError = new AxiosError("synthetic unauthorized", "ERR_BAD_REQUEST", config, undefined,
          { status: 401, statusText: "Unauthorized", config, headers: new AxiosHeaders(), data: {} });
        throw responseError;
      },
    }).then(() => { throw new Error("401 must remain a rejected request"); }, error => error);
    return { entered: entered.promise, async reject() { finish.resolve(); const error = await result; assert.equal(error, responseError); return error; } };
  }
  function seed(token: string) {
    const value = JSON.stringify({ state: { token } });
    local.setItem("agent-gateway-auth", value); session.setItem("agent-gateway-auth", value);
  }
  return { api, dispose() { assert.ok(dispose); dispose(); }, get state() { return auth.getState(); }, setState: auth.setState, local, session, location, pending401, seed, get clears() { return clears; } };
}

for (const mode of ["different-user", "refreshed-token", "same-token-different-user"] as const) {
  test(`old PUT 401 cannot clear a ${mode} authentication scope`, async () => {
    const h = host(); const request = h.pending401(); const config = await request.entered;
    assert.equal(config.headers.Authorization, undefined);
    assert.equal(config.headers["X-CSRF-Token"], "synthetic-csrf");
    assert.equal(config.withCredentials, true);
    if (mode !== "same-token-different-user") h.setState({ token: "synthetic-token-B" });
    if (mode !== "refreshed-token") h.setState({ user: { user_id: "user-B", roles: ["admin"], permissions: [], tier: "normal" }, isAuthenticated: true });
    h.seed(h.state.token!); const before = h.local.getItem("agent-gateway-auth");
    await request.reject();
    assert.equal(h.clears, 0); assert.equal(h.location.href, "/settings");
    assert.equal(h.local.getItem("agent-gateway-auth"), before); assert.equal(h.session.getItem("agent-gateway-auth"), before);
    assert.equal(h.state.token, mode === "same-token-different-user" ? "synthetic-token-A" : "synthetic-token-B");
  });
}
test("a current authenticated 401 still clears both stores and redirects", async () => {
  const h = host(); h.seed("synthetic-token-A"); const request = h.pending401(); await request.entered; await request.reject();
  assert.equal(h.clears, 1); assert.equal(h.state.token, null); assert.equal(h.location.href, "/login");
  assert.equal(h.local.getItem("agent-gateway-auth"), null); assert.equal(h.session.getItem("agent-gateway-auth"), null);
});
test("the first stale 401 cannot prevent a later current-token 401 from logging out", async () => {
  const h = host(); const old = h.pending401(); await old.entered;
  h.setState({ token: "synthetic-token-B" }); h.seed("synthetic-token-B");
  const current = h.pending401("get"); await current.entered;
  await old.reject(); assert.equal(h.clears, 0);
  await current.reject(); assert.equal(h.clears, 1); assert.equal(h.state.token, null);
});
test("an unauthenticated current request preserves existing login redirect behavior", async () => {
  const h = host(null, undefined); const request = h.pending401("get"); const config = await request.entered;
  assert.equal(config.headers.Authorization, undefined); await request.reject();
  assert.equal(h.clears, 1); assert.equal(h.location.href, "/login");
});
test("an unauthenticated old request cannot erase a login completed while it waited", async () => {
  const h = host(null, undefined); const request = h.pending401("get"); await request.entered;
  h.setState({ token: "synthetic-token-B" }); h.setState({ user: { user_id: "user-B", roles: ["admin"], permissions: [], tier: "normal" }, isAuthenticated: true }); h.seed("synthetic-token-B");
  await request.reject(); assert.equal(h.clears, 0); assert.equal(h.state.token, "synthetic-token-B");
});
test("legacy storage credentials are never sent or treated as a replacement session", async () => {
  const h = host(null, undefined); h.seed("synthetic-fallback-A"); const request = h.pending401("get");
  assert.equal((await request.entered).headers.Authorization, undefined);
  h.seed("synthetic-fallback-B"); await request.reject();
  assert.equal(h.clears, 1); assert.equal(h.local.getItem("agent-gateway-auth"), null);
});
test("a current anonymous 401 removes leftover persisted legacy authentication", async () => {
  const h = host(null, undefined); h.seed("synthetic-fallback-A"); const request = h.pending401("get");
  await request.entered; await request.reject(); assert.equal(h.clears, 1); assert.equal(h.local.getItem("agent-gateway-auth"), null);
});
test("current 401 on the login route does not redirect again", async () => {
  const h = host(null, undefined); h.location.pathname = "/login"; h.location.href = "/login?return=settings";
  const request = h.pending401("get"); await request.entered; await request.reject();
  assert.equal(h.clears, 1); assert.equal(h.location.href, "/login?return=settings");
});
test("a non-auth error stays rejected without touching current authentication", async () => {
  const h = host(); const error = await h.api.get("/api/v1/config/load-balancer", { adapter: async config => {
    throw new AxiosError("synthetic forbidden", "ERR_BAD_REQUEST", config, undefined,
      { status: 403, statusText: "Forbidden", config, headers: new AxiosHeaders(), data: {} });
  } }).catch(error => error);
  assert.equal(error.response.status, 403); assert.equal(h.clears, 0); assert.equal(h.state.token, "synthetic-token-A");
});

for (const boundary of ["roles", "permissions", "tier"] as const) {
  test(`old same-token 401 cannot clear changed ${boundary}, but a new owned 401 still logs out`, async () => {
    const h = host(); const old = h.pending401(); await old.entered;
    if (boundary === "tier") h.setState({ user: { ...h.state.user!, tier: "premium" } });
    else h.setState({ user: { ...h.state.user!, [boundary]: ["changed-authority"] } });
    h.seed("synthetic-token-A"); const current = h.pending401("get"); await current.entered;
    await old.reject(); assert.equal(h.clears, 0); assert.equal(h.state.token, "synthetic-token-A");
    await current.reject(); assert.equal(h.clears, 1); assert.equal(h.state.token, null);
  });
}

for (const boundary of ["token", "user_id", "roles", "permissions", "tier", "isAuthenticated", "logout"] as const) {
  test(`old 401 cannot clear restored authentication after ${boundary} A→B→A`, async () => {
    const h = host(); h.seed("synthetic-token-A");
    const original = h.state; const old = h.pending401(); await old.entered;
    if (boundary === "token") h.setState({ token: "synthetic-token-B" });
    else if (boundary === "isAuthenticated") h.setState({ isAuthenticated: false });
    else if (boundary === "logout") h.state.clearAuth();
    else h.setState({ user: { ...h.state.user!, [boundary]:
      boundary === "roles" || boundary === "permissions" ? ["changed-authority"] : "changed-authority" } });
    h.setState(original); h.seed("synthetic-token-A");
    const restored = h.state; const clearsBefore = h.clears; const persisted = h.local.getItem("agent-gateway-auth");
    const current = h.pending401("get"); await current.entered;
    await old.reject();
    assert.equal(h.clears, clearsBefore); assert.equal(h.state, restored);
    assert.equal(h.location.href, "/settings");
    assert.equal(h.local.getItem("agent-gateway-auth"), persisted);
    assert.equal(h.session.getItem("agent-gateway-auth"), persisted);
    await current.reject();
    assert.equal(h.clears, clearsBefore + 1); assert.equal(h.state.token, null);
    assert.equal(h.location.href, "/login");
    assert.equal(h.local.getItem("agent-gateway-auth"), null);
    assert.equal(h.session.getItem("agent-gateway-auth"), null);
  });
}

test("profile and loading changes retain current-request 401 recovery", async () => {
  const h = host(); h.seed("synthetic-token-A"); const request = h.pending401(); await request.entered;
  h.setState({ isLoading: true, user: { ...h.state.user!, display_name: "Updated profile" } });
  await request.reject();
  assert.equal(h.clears, 1); assert.equal(h.state.token, null); assert.equal(h.location.href, "/login");
});

test("network errors retain their identity and never clear current authentication", async () => {
  const h = host(); h.seed("synthetic-token-A"); const persisted = h.local.getItem("agent-gateway-auth");
  let transportError: AxiosError | undefined;
  const result = await h.api.get("/controlled", { adapter: async config => {
    transportError = new AxiosError("synthetic network failure", "ERR_NETWORK", config);
    throw transportError;
  } }).catch(error => error);
  assert.equal(result, transportError); assert.equal(h.clears, 0);
  assert.equal(h.state.token, "synthetic-token-A"); assert.equal(h.location.href, "/settings");
  assert.equal(h.local.getItem("agent-gateway-auth"), persisted);
  assert.equal(h.session.getItem("agent-gateway-auth"), persisted);
});

test("hot disposal retires pending requests even after authentication returns to A", async () => {
  const h = host(); h.seed("synthetic-token-A"); const original = h.state;
  const request = h.pending401(); await request.entered;
  h.dispose(); h.setState({ token: "synthetic-token-B" }); h.setState(original);
  await request.reject();
  assert.equal(h.clears, 0); assert.equal(h.state.token, "synthetic-token-A");
  assert.equal(h.location.href, "/settings");
  assert.ok(h.local.getItem("agent-gateway-auth")); assert.ok(h.session.getItem("agent-gateway-auth"));
});

test("an explicit Axios retry acquires the new request generation", async () => {
  const h = host(); const request = h.pending401(); const initialConfig = await request.entered;
  h.setState({ token: "synthetic-token-B" }); h.seed("synthetic-token-B");
  await request.reject(); assert.equal(h.clears, 0);
  let retryError: AxiosError | undefined;
  const result = await h.api.request({ ...initialConfig, adapter: async config => {
    assert.notEqual(config, initialConfig);
    assert.equal(config.headers.Authorization, undefined);
    assert.equal(config.headers["X-CSRF-Token"], "synthetic-csrf");
    retryError = new AxiosError("synthetic retry unauthorized", "ERR_BAD_REQUEST", config, undefined,
      { status: 401, statusText: "Unauthorized", config, headers: new AxiosHeaders(), data: {} });
    throw retryError;
  } }).catch(error => error);
  assert.equal(result, retryError); assert.equal(h.clears, 1); assert.equal(h.state.token, null);
  assert.equal(h.location.href, "/login");
});

for (const ownership of ["missing", "cloned"] as const) {
  test(`401 with ${ownership} request config cannot clear authentication`, async () => {
    const h = host(); h.seed("synthetic-token-A"); const persisted = h.local.getItem("agent-gateway-auth");
    let responseError: AxiosError | undefined;
    const result = await h.api.get("/controlled", { adapter: async config => {
      responseError = new AxiosError("synthetic unowned unauthorized", "ERR_BAD_REQUEST",
        ownership === "missing" ? undefined : { ...config }, undefined,
        { status: 401, statusText: "Unauthorized", config, headers: new AxiosHeaders(), data: {} });
      throw responseError;
    } }).catch(error => error);
    assert.equal(result, responseError); assert.equal(h.clears, 0);
    assert.equal(h.state.token, "synthetic-token-A"); assert.equal(h.location.href, "/settings");
    assert.equal(h.local.getItem("agent-gateway-auth"), persisted);
    assert.equal(h.session.getItem("agent-gateway-auth"), persisted);
  });
}


test("real Axios dispatch propagates a sampled trace only to same-origin API URLs", async () => {
  const client = createTelemetryClient({ origin: "https://console.example", authority: () => ({ key: "scope-a", token: "synthetic-token-A", authenticated: true }), fetch: async () => new Response(JSON.stringify({ enabled: true, sampled: true, sessionId: "0123456789abcdef0123456789abcdef", sampleRate: 0.01, ttlSeconds: 900 })) });
  await client.configure();
  const h = host("synthetic-token-A", "user-A", url => client.traceparent(url));
  const request = async (url: string, headers?: Record<string, string>) => h.api.get(url, { headers, adapter: async config => ({ config, status: 200, statusText: "OK", headers: new AxiosHeaders(), data: config.headers.toJSON() }) });
  assert.match((await request("/api/v1/services?query=PRIVATE")).data.traceparent, /^00-0123456789abcdef0123456789abcdef-[a-f0-9]{16}-01$/);
  assert.equal((await request("https://outside.example/api/v1/services")).data.traceparent, undefined);
  assert.equal((await request("/api/v1/services", { traceparent: "existing-context" })).data.traceparent, "existing-context");
  client.reset(); assert.equal((await request("/api/v1/services")).data.traceparent, undefined);
  h.dispose();
});
