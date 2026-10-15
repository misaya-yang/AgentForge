import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createStore } from "zustand/vanilla";
import axios, { type AxiosInstance, type InternalAxiosRequestConfig } from "axios";

/** Real shared client, installed Axios pipeline and controlled adapter; no sockets. */
function client() {
  let cleared = 0; let removed = 0; let storageThrows = false; let authThrows = false;
  const user = { user_id: "actor-a", roles: ["platform_admin"], permissions: ["role:create"], tier: "normal" };
  const auth = createStore(() => ({ token: "synthetic-a" as string | null, user, isAuthenticated: true, clearAuth() { cleared++; auth.setState({ token: null, isAuthenticated: false }); } }));
  const location = { pathname: "/users/roles", href: "" };
  const storage = { getItem() { if (storageThrows) throw new Error("storage unavailable"); return null; }, removeItem() { removed++; } };
  const modules: Record<string, unknown> = { axios,
    "@/config/runtime": { getApiBaseUrlConfig: () => "https://unused.invalid" },
    "@/hooks/use-toast": { toast: { warning() {} } }, "@/i18n": { default: { t: (key: string) => key } },
    "@/store/useAuthStore": { useAuthStore: { getState() { if (authThrows) throw new Error("auth unavailable"); return auth.getState(); }, subscribe: auth.subscribe } },
  };
  function load(file: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, localStorage: storage, sessionStorage: storage, window: { location }, Error, __hot: undefined,
    }); return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = load("./sdkAuthorityScope.ts");
  const api = load("./api.ts").api as AxiosInstance;
  const calls: InternalAxiosRequestConfig[] = [];
  let reject!: (error: unknown) => void;
  api.defaults.adapter = config => { calls.push(config); return new Promise((_resolve, fail) => { reject = fail; }); };
  return { api, get state() { return auth.getState(); }, setState: auth.setState, calls, location, cleared: () => cleared, removed: () => removed,
    fail401() { reject({ config: calls.at(-1), response: { status: 401 } }); },
    storageFailure() { storageThrows = true; }, authFailure() { authThrows = true; },
  };
}

test("shared synchronous authentication binds initial identity before immediate replacement", async () => {
  const h = client(); const pending = h.api.post("/controlled", { role_name: "old_review" });
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].headers.Authorization, "Bearer synthetic-a");
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
test("auth and missing-token storage exceptions fail closed before adapter dispatch", async () => {
  for (const type of ["auth", "storage"]) { const h = client(); if (type === "auth") h.authFailure(); else { h.setState({ token: null }); h.storageFailure(); }
    await assert.rejects(h.api.post("/controlled", { role_name: "never_sent" }), /unavailable/); assert.equal(h.calls.length, 0);
  }
});
