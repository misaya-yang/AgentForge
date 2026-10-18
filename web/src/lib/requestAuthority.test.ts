import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import fetchHttp, { type FetchClient, type RequestConfig } from './fetchClient.ts';

/** Real shared client, installed Fetch pipeline and controlled adapter; no sockets. */
function client() {
  let runtimeBase = "";
  let cleared = 0; let removed = 0; let storageThrows = false; let authThrows = false;
  const user = { user_id: "actor-a", roles: ["platform_admin"], permissions: ["role:create"], tier: "normal" };
  const auth = createStore(() => ({ token: "synthetic-a" as string | null, user, isAuthenticated: true, csrfToken: "synthetic-csrf", clearAuth() { cleared++; auth.setState({ token: null, isAuthenticated: false }); } }));
  const location = { origin: "https://console.example", pathname: "/users/roles", href: "" };
  const storage = { getItem() { if (storageThrows) throw new Error("storage unavailable"); return null; }, removeItem() { removed++; } };
  const modules: Record<string, unknown> = { "@/lib/fetchClient": fetchHttp,
    "@/observability/browserTelemetry": { browserTraceparent: () => undefined },
    "@/config/runtime": { getApiBaseUrlConfig: () => runtimeBase },
    "@/hooks/use-toast": { toast: { warning() {} } }, "@/i18n": { default: { t: (key: string) => key } },
    "@/store/useAuthStore": { useAuthStore: { getState() { if (authThrows) throw new Error("auth unavailable"); return auth.getState(); }, subscribe: auth.subscribe } },
  };
  function load(file: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, localStorage: storage, sessionStorage: storage, window: { location }, URL, Error, __hot: undefined,
    }); return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = load("./sdkAuthorityScope.ts");
  const api = load("./api.ts").api as FetchClient;
  const calls: RequestConfig[] = [];
  let reject!: (error: unknown) => void;
  api.defaults.adapter = config => { calls.push(config); return new Promise((_resolve, fail) => { reject = fail; }); };
  return { api, setRuntimeBase(value: string) { runtimeBase = value; }, get state() { return auth.getState(); }, setState: auth.setState, calls, location, cleared: () => cleared, removed: () => removed,
    fail401() { reject({ config: calls.at(-1), response: { status: 401 } }); },
    storageFailure() { storageThrows = true; }, authFailure() { authThrows = true; },
  };
}

test("shared synchronous authentication binds initial identity before immediate replacement", async () => {
  const h = client(); const pending = h.api.post("/controlled", { role_name: "old_review" });
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].headers.Authorization, undefined); assert.equal(h.calls[0].headers["X-CSRF-Token"], "synthetic-csrf"); assert.equal(h.calls[0].withCredentials, true);
  h.setState({ token: "synthetic-b", user: { ...h.state.user, user_id: "actor-b" } });
  h.fail401(); await assert.rejects(pending); assert.equal(h.cleared(), 0); assert.equal(h.removed(), 0); assert.equal(h.location.href, "");
});
test("current-identity 401 still clears both stores and redirects", async () => {
  const h = client(); const pending = h.api.get("/controlled"); h.fail401(); await assert.rejects(pending);
  assert.equal(h.cleared(), 1); assert.equal(h.removed(), 2); assert.equal(h.location.href, "/login");
});
test("same token with changed authority cannot clear replacement state", async () => {
  const h = client(); const pending = h.api.get("/controlled"); h.setState({ user: { ...h.state.user, roles: ["user"] } });
  h.fail401(); await assert.rejects(pending); assert.equal(h.cleared(), 0); assert.equal(h.removed(), 0);
});
test("missing token preserves anonymous request semantics without a bearer", async () => {
  const h = client(); h.setState({ token: null }); const pending = h.api.get("/controlled");
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].headers.Authorization, undefined);
  h.fail401(); await assert.rejects(pending);
});
test("auth lookup exceptions fail closed before adapter dispatch", async () => {
  const h=client();h.authFailure();
  await assert.rejects(h.api.post("/controlled",{role_name:"never_sent"}),/unavailable/);assert.equal(h.calls.length,0);
});
test("blocked Web Storage cannot prevent cookie authenticated requests", async () => {
  const h=client();h.storageFailure(); const request=h.api.get("/controlled");
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].headers.Authorization,undefined);
  h.fail401();await assert.rejects(request);assert.equal(h.cleared(),1);
});

test("central client refuses cross-origin cookie requests before adapter dispatch",async()=>{
  const h=client();await assert.rejects(h.api.get("https://foreign.example/api/v1/items"),/same origin/);assert.equal(h.calls.length,0);
});
test("central client refuses writes without memory CSRF",async()=>{
  const h=client();h.setState({csrfToken:""});await assert.rejects(h.api.post("/api/v1/items",{}),/CSRF bootstrap/);assert.equal(h.calls.length,0);
});
test("an explicit SDK bearer is preserved, omits cookies and cannot clear Console on 401",async()=>{
  const h=client();const pending=h.api.get("/v1/models",{headers:{Authorization:"Bearer sdk-api-key"}});
  assert.equal(h.calls[0].headers.Authorization,"Bearer sdk-api-key");assert.equal(h.calls[0].headers["X-CSRF-Token"],undefined);assert.equal(h.calls[0].withCredentials,false);
  h.fail401();await assert.rejects(pending);assert.equal(h.cleared(),0);
});

test("same-origin relative API base remains supported in the cookie BFF client",async()=>{
  const h=client();const pending=h.api.get("/api/v1/items",{baseURL:"/"});assert.equal(h.calls.length,1);assert.equal(h.calls[0].withCredentials,true);h.fail401();await assert.rejects(pending);
});

test("central client rejects old UI marker instead of substituting the new actor cookie",async()=>{
  const h=client();h.setState({token:"console-session:new"});
  await assert.rejects(h.api.post("/api/v1/items",{}, {headers:{Authorization:"Bearer console-session:old"}}),/session changed/);assert.equal(h.calls.length,0);
});


test("client reads runtime authority after JSON bootstrap instead of capturing the import-time fallback", async () => {
  const h = client();
  h.setRuntimeBase("/configured-api");
  const pending = h.api.get("/api/v1/items");
  assert.equal(h.calls[0].baseURL, "/configured-api");
  h.fail401();
  await assert.rejects(pending);
  const foreign = client();
  foreign.setRuntimeBase("https://foreign.invalid");
  await assert.rejects(foreign.api.get("/api/v1/items"), /same origin/);
  assert.equal(foreign.calls.length, 0);
});
